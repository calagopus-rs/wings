use super::configuration::process::ProcessConfigurationStartup;
use futures_util::StreamExt;
use std::sync::Arc;
use tokio::{
    io::AsyncWriteExt,
    sync::{Mutex, RwLock},
};

pub struct Container {
    pub docker_id: String,

    pub update_reciever: Mutex<
        Option<
            tokio::sync::mpsc::Receiver<(
                bollard::models::ContainerState,
                crate::server::resources::ResourceUsage,
            )>,
        >,
    >,

    state_reciever: tokio::task::JoinHandle<()>,

    pub resource_usage: Arc<RwLock<crate::server::resources::ResourceUsage>>,
    resource_usage_reciever: tokio::task::JoinHandle<()>,

    pub stdin: tokio::sync::mpsc::Sender<String>,
    stdin_reciever: tokio::task::JoinHandle<()>,

    pub stdout: tokio::sync::broadcast::Receiver<String>,
    stdout_reciever: tokio::task::JoinHandle<()>,
}

impl Container {
    pub async fn new(
        docker_id: String,
        startup_configuration: ProcessConfigurationStartup,
        client: Arc<bollard::Docker>,
        server: crate::server::Server,
    ) -> Result<Self, bollard::errors::Error> {
        let (stdin, mut stdin_reciever) = tokio::sync::mpsc::channel(150);
        let (stdout_sender, stdout) = tokio::sync::broadcast::channel(150);

        let (update_channel, update_reciever) = tokio::sync::mpsc::channel(1);

        let resource_usage = Arc::new(RwLock::new(crate::server::resources::ResourceUsage {
            disk_bytes: server.filesystem.limiter_usage().await,
            state: server.state.get_state(),
            ..Default::default()
        }));

        let server_uuid = server.uuid;
        let mut stream = client
            .attach_container::<String>(
                &docker_id,
                Some(bollard::container::AttachContainerOptions {
                    stdin: Some(true),
                    stdout: Some(true),
                    stderr: Some(true),
                    stream: Some(true),
                    ..Default::default()
                }),
            )
            .await?;

        Ok(Self {
            docker_id: docker_id.clone(),

            update_reciever: Mutex::new(Some(update_reciever)),

            state_reciever: tokio::spawn({
                let docker_id = docker_id.clone();
                let client = Arc::clone(&client);
                let resource_usage = Arc::clone(&resource_usage);

                async move {
                    loop {
                        tokio::time::sleep(std::time::Duration::from_secs(1)).await;

                        let container_state = client
                            .inspect_container(&docker_id, None)
                            .await
                            .unwrap_or_default();
                        let container_state = container_state.state.unwrap_or_default();

                        if container_state.status
                            == Some(bollard::secret::ContainerStateStatusEnum::RUNNING)
                        {
                            if let Some(started_at) = &container_state.started_at
                                && let Ok(started_at) =
                                    chrono::DateTime::parse_from_rfc3339(started_at)
                            {
                                let now = chrono::Utc::now();
                                let started_at = started_at.with_timezone(&chrono::Utc);

                                let uptime =
                                    now.signed_duration_since(started_at).num_milliseconds() as u64;
                                resource_usage.write().await.uptime = uptime;
                            }
                        } else {
                            resource_usage.write().await.uptime = 0;
                        }

                        update_channel
                            .send((container_state, *resource_usage.read().await))
                            .await
                            .unwrap_or_default();
                    }
                }
            }),

            resource_usage: Arc::clone(&resource_usage),
            resource_usage_reciever: tokio::spawn({
                let docker_id = docker_id.clone();
                let client = Arc::clone(&client);
                let resource_usage = Arc::clone(&resource_usage);
                let server = server.clone();

                async move {
                    let mut prev_cpu = (0, 0);

                    let mut stats_stream = client.stats(
                        &docker_id,
                        Some(bollard::container::StatsOptions {
                            stream: true,
                            one_shot: false,
                        }),
                    );

                    while let Some(Ok(stats)) = stats_stream.next().await {
                        let (disk_usage, _) = tokio::join!(
                            server.filesystem.limiter_usage(),
                            tokio::time::sleep(std::time::Duration::from_millis(500)),
                        );

                        let mut usage = resource_usage.write().await;

                        usage.memory_bytes = stats.memory_stats.usage.unwrap_or(0);
                        usage.memory_limit_bytes = stats.memory_stats.limit.unwrap_or(0);
                        usage.disk_bytes = disk_usage;
                        usage.state = server.state.get_state();

                        if let Some(networks) = stats.networks
                            && let Some(network) = networks.values().next()
                        {
                            usage.network.rx_bytes = network.rx_bytes;
                            usage.network.tx_bytes = network.tx_bytes;
                        }

                        // TODO: This requires urgent refactoring to handle multiple CPUs correctly (and fix podman support)
                        usage.cpu_absolute = {
                            let cpu_delta = stats
                                .cpu_stats
                                .cpu_usage
                                .total_usage
                                .saturating_sub(prev_cpu.0)
                                as f64;
                            let system_delta = stats
                                .cpu_stats
                                .system_cpu_usage
                                .unwrap_or(0)
                                .saturating_sub(prev_cpu.1)
                                as f64;

                            let cpus = stats.cpu_stats.online_cpus.unwrap_or_else(|| {
                                stats
                                    .cpu_stats
                                    .cpu_usage
                                    .percpu_usage
                                    .unwrap_or_default()
                                    .len() as u64
                            }) as f64;

                            let mut percent = 0.0;
                            if system_delta > 0.0 && cpu_delta > 0.0 {
                                percent = (cpu_delta / system_delta) * 100.0;

                                if cpus > 0.0 {
                                    percent *= cpus;
                                }
                            }

                            (percent * 1000.0).round() / 1000.0
                        };

                        prev_cpu = (
                            stats.cpu_stats.cpu_usage.total_usage,
                            stats.cpu_stats.system_cpu_usage.unwrap_or(0),
                        );
                    }
                }
            }),

            stdin,
            stdin_reciever: tokio::task::spawn(async move {
                while let Some(data) = stdin_reciever.recv().await {
                    if let Err(err) = stream.input.write_all(data.as_bytes()).await {
                        tracing::error!(server = %server_uuid, error = %err, "failed to write to container stdin");
                    }
                }
            }),

            stdout,
            stdout_reciever: tokio::task::spawn(async move {
                let mut buffer = Vec::with_capacity(1024);
                let mut line_start = 0;

                let mut ratelimit_counter = 0;
                let mut ratelimit_start = std::time::Instant::now();

                let mut allow_ratelimit = async || {
                    ratelimit_counter += 1;

                    if server.app_state.config.throttles.enabled
                        && server.app_state.config.throttles.line_reset_interval > 0
                        && ratelimit_counter >= server.app_state.config.throttles.lines
                    {
                        if ratelimit_start.elapsed()
                            < std::time::Duration::from_secs(
                                server.app_state.config.throttles.line_reset_interval,
                            )
                        {
                            if ratelimit_counter == server.app_state.config.throttles.lines {
                                tracing::debug!(
                                    server = %server.uuid,
                                    lines = server.app_state.config.throttles.lines,
                                    reset_interval = server.app_state.config.throttles.line_reset_interval,
                                    "ratelimit reached for server output"
                                );

                                server.log_daemon_with_prelude("Server is outputting console data too quickly -- throttling...").await;
                            }

                            return false;
                        } else {
                            ratelimit_counter = 0;
                            ratelimit_start = std::time::Instant::now();
                        }
                    }

                    true
                };

                while let Some(Ok(data)) = stream.output.next().await {
                    buffer.extend_from_slice(&data.into_bytes());

                    let mut search_start = line_start;

                    loop {
                        if let Some(pos) = buffer[search_start..].iter().position(|&b| b == b'\n') {
                            let newline_pos = search_start + pos;

                            let check_startup = async |line: &String| {
                                if server.state.get_state() != super::state::ServerState::Starting {
                                    return;
                                }

                                if let Some(done_vec) = &startup_configuration.done {
                                    if startup_configuration.strip_ansi {
                                        let mut result_line = line.clone();
                                        let mut chars = line.chars().peekable();

                                        while let Some(c) = chars.next() {
                                            if c == '\u{1b}' {
                                                while let Some(&next) = chars.peek() {
                                                    chars.next();

                                                    if next.is_ascii_alphabetic() {
                                                        break;
                                                    }
                                                }
                                            } else {
                                                result_line.push(c);
                                            }
                                        }

                                        for done in done_vec {
                                            if result_line.contains(done) {
                                                server
                                                    .state
                                                    .set_state(super::state::ServerState::Running)
                                                    .await;
                                                break;
                                            }
                                        }
                                    } else {
                                        for done in done_vec {
                                            if line.contains(done) {
                                                server
                                                    .state
                                                    .set_state(super::state::ServerState::Running)
                                                    .await;
                                                break;
                                            }
                                        }
                                    }
                                }
                            };

                            if newline_pos - line_start <= 512 {
                                let line =
                                    String::from_utf8_lossy(&buffer[line_start..newline_pos])
                                        .trim()
                                        .to_string();

                                check_startup(&line).await;
                                if allow_ratelimit().await
                                    && let Err(err) = stdout_sender.send(line)
                                {
                                    tracing::error!(
                                        server = %server_uuid,
                                        error = %err,
                                        "failed to send stdout line"
                                    );
                                }

                                line_start = newline_pos + 1;
                                search_start = line_start;
                            } else {
                                let line = String::from_utf8_lossy(
                                    &buffer[line_start..(line_start + 512)],
                                )
                                .trim()
                                .to_string();

                                check_startup(&line).await;
                                if allow_ratelimit().await
                                    && let Err(err) = stdout_sender.send(line)
                                {
                                    tracing::error!(
                                        server = %server_uuid,
                                        error = %err,
                                        "failed to send stdout line"
                                    );
                                }

                                line_start += 512;
                                search_start = line_start;
                            }
                        } else {
                            let current_line_length = buffer.len() - line_start;
                            if current_line_length > 512 {
                                let line = String::from_utf8_lossy(
                                    &buffer[line_start..(line_start + 512)],
                                )
                                .trim()
                                .to_string();
                                if allow_ratelimit().await
                                    && let Err(err) = stdout_sender.send(line)
                                {
                                    tracing::error!(
                                        server = %server_uuid,
                                        error = %err,
                                        "failed to send stdout line"
                                    );
                                }

                                line_start += 512;
                                search_start = line_start;
                            } else {
                                break;
                            }
                        }
                    }

                    if line_start > 1024 && line_start > buffer.len() / 2 {
                        buffer.drain(0..line_start);
                        line_start = 0;
                    }
                }

                if line_start < buffer.len() {
                    let line = String::from_utf8_lossy(&buffer[line_start..])
                        .trim()
                        .to_string();
                    if let Err(err) = stdout_sender.send(line) {
                        tracing::error!(
                            server = %server_uuid,
                            error = %err,
                            "failed to send remaining stdout line"
                        );
                    }
                }
            }),
        })
    }
}

impl Drop for Container {
    fn drop(&mut self) {
        self.state_reciever.abort();
        self.resource_usage_reciever.abort();
        self.stdin_reciever.abort();
        self.stdout_reciever.abort();
    }
}
