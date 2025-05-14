use bollard::secret::ContainerStateStatusEnum;
use colored::Colorize;
use futures_util::StreamExt;
use serde_json::json;
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, atomic::AtomicBool},
};
use tokio::sync::RwLock;

pub mod backup;
pub mod configuration;
pub mod container;
pub mod filesystem;
pub mod installation;
pub mod manager;
pub mod permissions;
pub mod resources;
pub mod state;
pub mod websocket;

pub struct Server {
    pub uuid: uuid::Uuid,
    config: Arc<crate::config::Config>,

    pub configuration: RwLock<configuration::ServerConfiguration>,
    pub process_configuration: RwLock<configuration::process::ProcessConfiguration>,

    pub websocket: tokio::sync::broadcast::Sender<websocket::WebsocketMessage>,
    // Dummy receiver to avoid channel being closed
    _websocket_receiver: tokio::sync::broadcast::Receiver<websocket::WebsocketMessage>,
    websocket_sender: RwLock<Option<tokio::task::JoinHandle<()>>>,

    pub container: Arc<RwLock<Option<container::Container>>>,

    pub state: Arc<state::ServerStateLock>,
    suspended: AtomicBool,
    installing: AtomicBool,
    restoring: AtomicBool,
    transferring: AtomicBool,

    restarting: AtomicBool,
    stopping: AtomicBool,
    last_crash: RwLock<Option<std::time::Instant>>,
    crash_handled: AtomicBool,

    pub filesystem: Arc<filesystem::Filesystem>,
}

impl Server {
    pub fn new(
        configuration: configuration::ServerConfiguration,
        process_configuration: configuration::process::ProcessConfiguration,
        config: Arc<crate::config::Config>,
    ) -> Self {
        let filesystem = Arc::new(filesystem::Filesystem::new(
            PathBuf::from(&config.system.data_directory).join(configuration.uuid.to_string()),
            configuration.build.disk_space * 1024 * 1024,
            config.system.disk_check_interval,
            &config,
        ));

        let (rx, tx) = tokio::sync::broadcast::channel(128);

        let state = Arc::new(state::ServerStateLock::new(rx.clone()));
        let container = Arc::new(RwLock::new(None::<container::Container>));

        Self {
            uuid: configuration.uuid,

            config,

            configuration: RwLock::new(configuration),
            process_configuration: RwLock::new(process_configuration),

            websocket: rx,
            _websocket_receiver: tx,
            websocket_sender: RwLock::new(None),

            container,

            state,
            suspended: AtomicBool::new(false),
            installing: AtomicBool::new(false),
            restoring: AtomicBool::new(false),
            transferring: AtomicBool::new(false),

            restarting: AtomicBool::new(false),
            stopping: AtomicBool::new(false),
            last_crash: RwLock::new(None),
            crash_handled: AtomicBool::new(false),

            filesystem,
        }
    }

    pub async fn setup_websocket_sender(&self, server: Arc<Self>, client: Arc<bollard::Docker>) {
        *self.websocket_sender.write().await = Some(tokio::task::spawn(async move {
            let mut prev_usage = resources::ResourceUsage::default();

            loop {
                let usage = if let Some(container) = server.container.read().await.as_ref() {
                    *container.resource_usage.read().await
                } else {
                    resources::ResourceUsage::default()
                };
                let container_state =
                    if let Some(container) = server.container.read().await.as_ref() {
                        container.state.read().await.clone()
                    } else {
                        bollard::models::ContainerState::default()
                    };

                if usage != prev_usage {
                    let message = websocket::WebsocketMessage::new(
                        websocket::WebsocketEvent::ServerStats,
                        &[serde_json::to_string(&usage).unwrap()],
                    );

                    if let Err(e) = server.websocket.send(message) {
                        crate::logger::log(
                            crate::logger::LoggerLevel::Error,
                            format!("Failed to send message: {}", e),
                        );
                    }

                    prev_usage = usage;
                }

                if server.filesystem.is_full()
                    && server.state.get_state() != state::ServerState::Offline
                    && !server.stopping.load(std::sync::atomic::Ordering::Relaxed)
                {
                    server
                        .log_daemon_with_prelude("Server is exceeding the assigned disk space limit, stopping process now.")
                        .await;

                    tokio::spawn({
                        let client = Arc::clone(&client);
                        let server = Arc::clone(&server);

                        async move {
                            if let Err(err) = server
                                .stop(&client, Some(std::time::Duration::from_secs(60)))
                                .await
                            {
                                crate::logger::log(
                                    crate::logger::LoggerLevel::Debug,
                                    format!(
                                        "Failed to stop server after disk usage was exceeded, killing: {}",
                                        err
                                    ),
                                );

                                if let Err(err) = server.kill(&client).await {
                                    crate::logger::log(
                                        crate::logger::LoggerLevel::Error,
                                        format!(
                                            "Failed to kill server after disk usage was exceeded: {}",
                                            err
                                        ),
                                    );
                                }
                            }
                        }
                    });
                }

                if let Some(status) = container_state.status {
                    match status {
                        ContainerStateStatusEnum::RUNNING => {
                            if !matches!(
                                server.state.get_state(),
                                state::ServerState::Running
                                    | state::ServerState::Starting
                                    | state::ServerState::Stopping,
                            ) {
                                server.state.set_state(state::ServerState::Running);
                            }
                        }
                        ContainerStateStatusEnum::DEAD | ContainerStateStatusEnum::EXITED => {
                            server.state.set_state(state::ServerState::Offline);

                            if server.restarting.load(std::sync::atomic::Ordering::Relaxed) {
                                server
                                    .crash_handled
                                    .store(true, std::sync::atomic::Ordering::Relaxed);
                                server
                                    .restarting
                                    .store(false, std::sync::atomic::Ordering::Relaxed);
                                server
                                    .stopping
                                    .store(false, std::sync::atomic::Ordering::Relaxed);

                                if let Err(err) = server.start(&client, None).await {
                                    crate::logger::log(
                                        crate::logger::LoggerLevel::Error,
                                        format!("Failed to start server after restart: {}", err),
                                    );
                                }
                            } else if server.stopping.load(std::sync::atomic::Ordering::Relaxed) {
                                server
                                    .crash_handled
                                    .store(true, std::sync::atomic::Ordering::Relaxed);
                                server
                                    .stopping
                                    .store(false, std::sync::atomic::Ordering::Relaxed);
                            } else if server.config.system.crash_detection.enabled
                                && !server
                                    .crash_handled
                                    .load(std::sync::atomic::Ordering::Relaxed)
                            {
                                server
                                    .crash_handled
                                    .store(true, std::sync::atomic::Ordering::Relaxed);

                                if container_state.exit_code.is_some_and(|code| code == 0)
                                    && !container_state.oom_killed.unwrap_or(false)
                                    && !server
                                        .config
                                        .system
                                        .crash_detection
                                        .detect_clean_exit_as_crash
                                {
                                    crate::logger::log(
                                        crate::logger::LoggerLevel::Debug,
                                        "Container exited cleanly, not restarting due to crash detection settings".to_string(),
                                    );
                                    return;
                                }

                                server.log_daemon_with_prelude("---------- Detected server process in a crashed state! ----------").await;
                                server
                                    .log_daemon_with_prelude(&format!(
                                        "Exit code: {}",
                                        container_state.exit_code.unwrap_or_default()
                                    ))
                                    .await;
                                server
                                    .log_daemon_with_prelude(&format!(
                                        "Out of memory: {}",
                                        container_state.oom_killed.unwrap_or(false)
                                    ))
                                    .await;

                                if let Some(last_crash) = *server.last_crash.read().await {
                                    if last_crash.elapsed().as_secs()
                                        < server.config.system.crash_detection.timeout
                                    {
                                        server.log_daemon_with_prelude(
                                            &format!(
                                                "Aborting automatic restart, last crash occurred less than {} seconds ago.",
                                                server.config.system.crash_detection.timeout
                                            ),
                                        ).await;
                                        return;
                                    } else {
                                        server
                                            .last_crash
                                            .write()
                                            .await
                                            .replace(std::time::Instant::now());
                                    }
                                } else {
                                    server
                                        .last_crash
                                        .write()
                                        .await
                                        .replace(std::time::Instant::now());
                                }

                                if let Err(err) = server.start(&client, None).await {
                                    crate::logger::log(
                                        crate::logger::LoggerLevel::Error,
                                        format!("Failed to start server after crash: {}", err),
                                    );
                                }
                            }
                        }
                        _ => {}
                    }
                }

                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            }
        }));
    }

    pub async fn container_stdin(&self) -> Option<tokio::sync::mpsc::Sender<String>> {
        self.container
            .read()
            .await
            .as_ref()
            .map(|c| c.stdin.clone())
    }

    pub async fn container_stdout(&self) -> Option<tokio::sync::broadcast::Receiver<String>> {
        self.container
            .read()
            .await
            .as_ref()
            .map(|c| c.stdout.resubscribe())
    }

    pub async fn resource_usage(&self) -> resources::ResourceUsage {
        if let Some(container) = self.container.read().await.as_ref() {
            *container.resource_usage.read().await
        } else {
            resources::ResourceUsage {
                disk_bytes: self.filesystem.cached_usage(),
                ..Default::default()
            }
        }
    }

    pub async fn update_configuration(
        &self,
        configuration: configuration::ServerConfiguration,
        process_configuration: configuration::process::ProcessConfiguration,
        client: &Arc<bollard::Docker>,
    ) {
        *self.configuration.write().await = configuration;
        *self.process_configuration.write().await = process_configuration;

        if let Err(err) = self.sync_container(client).await {
            crate::logger::log(
                crate::logger::LoggerLevel::Error,
                format!("Failed to sync container: {}", err),
            );
        }
    }

    pub fn is_locked_state(&self) -> bool {
        self.suspended.load(std::sync::atomic::Ordering::SeqCst)
            || self.installing.load(std::sync::atomic::Ordering::SeqCst)
            || self.restoring.load(std::sync::atomic::Ordering::SeqCst)
            || self.transferring.load(std::sync::atomic::Ordering::SeqCst)
    }

    pub async fn setup_container(
        &self,
        client: &Arc<bollard::Docker>,
    ) -> Result<(), bollard::errors::Error> {
        self.crash_handled
            .store(false, std::sync::atomic::Ordering::Relaxed);

        if self.container.read().await.is_some() {
            return Ok(());
        }

        let container = client
            .create_container(
                Some(bollard::container::CreateContainerOptions {
                    name: self.configuration.read().await.uuid,
                    ..Default::default()
                }),
                self.configuration
                    .read()
                    .await
                    .container_config(&self.config, client, &self.filesystem)
                    .await,
            )
            .await?;

        *self.container.write().await = Some(
            container::Container::new(
                container.id.clone(),
                self.process_configuration.read().await.startup.clone(),
                Arc::clone(client),
                Arc::clone(&self.state),
                Arc::clone(&self.filesystem),
            )
            .await?,
        );

        Ok(())
    }

    pub async fn attach_container(
        &self,
        client: &Arc<bollard::Docker>,
    ) -> Result<(), bollard::errors::Error> {
        if self.container.read().await.is_some() {
            return Ok(());
        }

        if let Ok(containers) = client
            .list_containers(Some(bollard::container::ListContainersOptions {
                all: true,
                filters: HashMap::from([("name".to_string(), vec![self.uuid.to_string()])]),
                ..Default::default()
            }))
            .await
        {
            if let Some(container) = containers.first() {
                let container = container.id.clone().unwrap();

                *self.container.write().await = Some(
                    container::Container::new(
                        container.to_string(),
                        self.process_configuration.read().await.startup.clone(),
                        Arc::clone(client),
                        Arc::clone(&self.state),
                        Arc::clone(&self.filesystem),
                    )
                    .await?,
                );
            }
        }

        Ok(())
    }

    pub async fn sync_container(
        &self,
        client: &bollard::Docker,
    ) -> Result<(), bollard::errors::Error> {
        self.filesystem.disk_limit.store(
            self.configuration.read().await.build.disk_space as i64 * 1024 * 1024,
            std::sync::atomic::Ordering::Relaxed,
        );

        if let Some(container) = self.container.read().await.as_ref() {
            client
                .update_container(
                    &container.docker_id,
                    self.configuration
                        .read()
                        .await
                        .container_update_config(&self.config),
                )
                .await?;
        }

        Ok(())
    }

    pub async fn read_log(
        &self,
        client: &bollard::Docker,
        lines: usize,
    ) -> Result<String, bollard::errors::Error> {
        let container = match &*self.container.read().await {
            Some(container) => container.docker_id.clone(),
            None => {
                return Ok("".to_string());
            }
        };

        let mut logs_stream = client.logs(
            &container,
            Some(bollard::container::LogsOptions {
                follow: false,
                stdout: true,
                stderr: true,
                timestamps: false,
                tail: lines.to_string(),
                ..Default::default()
            }),
        );

        let mut logs = String::new();
        while let Some(Ok(log)) = logs_stream.next().await {
            logs.push_str(String::from_utf8_lossy(&log.into_bytes()).as_ref());
        }

        Ok(logs)
    }

    pub async fn log_daemon(&self, message: String) {
        self.websocket
            .send(websocket::WebsocketMessage::new(
                websocket::WebsocketEvent::ServerDaemonMessage,
                &[message],
            ))
            .unwrap_or_default();
    }

    pub async fn log_daemon_install(&self, message: String) {
        self.websocket
            .send(websocket::WebsocketMessage::new(
                websocket::WebsocketEvent::ServerInstallOutput,
                &[message],
            ))
            .unwrap_or_default();
    }

    pub async fn log_daemon_with_prelude(&self, message: &str) {
        let prelude = format!("[{} Daemon]: ", self.config.app_name).yellow();

        self.websocket
            .send(websocket::WebsocketMessage::new(
                websocket::WebsocketEvent::ServerConsoleOutput,
                &[format!("{}{}", prelude, message).bold().to_string()],
            ))
            .unwrap_or_default();
    }

    pub async fn log_daemon_error(&self, message: &str) {
        self.log_daemon(message.bold().on_red().to_string()).await
    }

    pub async fn pull_image(
        &self,
        client: &Arc<bollard::Docker>,
        image: String,
    ) -> Result<(), bollard::errors::Error> {
        self.log_daemon_with_prelude(
            "Pulling Docker container image, this could take a few minutes to complete...",
        )
        .await;

        if !image.ends_with("~") {
            let mut registry_auth = None;
            for (registry, config) in self.config.docker.registries.iter() {
                if image.starts_with(registry) {
                    registry_auth = Some(bollard::auth::DockerCredentials {
                        username: Some(config.username.clone()),
                        password: Some(config.password.clone()),
                        serveraddress: Some(registry.clone()),
                        ..Default::default()
                    });
                    break;
                }
            }

            let (image, tag) = image.split_once(':').unwrap_or((&image, "latest"));

            let mut stream = client.create_image(
                Some(bollard::image::CreateImageOptions {
                    from_image: image,
                    tag,
                    ..Default::default()
                }),
                None,
                registry_auth,
            );

            while let Some(status) = stream.next().await {
                match status {
                    Ok(status) => {
                        if let Some(status_str) = status.status {
                            if let Some(progress) = status.progress {
                                self.log_daemon_install(format!("{} {}", status_str, progress))
                                    .await;
                            } else {
                                self.log_daemon_install(status_str).await;
                            }
                        }
                    }
                    Err(err) => {
                        crate::logger::log(
                            crate::logger::LoggerLevel::Error,
                            format!("Failed to pull image: {}", err),
                        );

                        if let Ok(images) = client
                            .list_images(Some(bollard::image::ListImagesOptions {
                                all: true,
                                filters: HashMap::from([(
                                    "reference".to_string(),
                                    vec![image.to_string()],
                                )]),
                                ..Default::default()
                            }))
                            .await
                        {
                            if images.is_empty() {
                                return Err(err);
                            } else {
                                crate::logger::log(
                                    crate::logger::LoggerLevel::Debug,
                                    format!("Image already exists, ignoring error: {}", err),
                                );
                            }
                        } else {
                            return Err(err);
                        }
                    }
                }
            }
        }

        self.log_daemon_with_prelude("Finished pulling Docker container image")
            .await;

        Ok(())
    }

    pub async fn start(
        &self,
        client: &Arc<bollard::Docker>,
        aquire_timeout: Option<std::time::Duration>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        if self.is_locked_state() {
            self.log_daemon_error("Server is in a locked state, cannot start the server")
                .await;
            return Err("server is in a locked state, cannot start the server".into());
        }

        if self.state.get_state() != state::ServerState::Offline {
            self.log_daemon_error("server is already running").await;
            return Err("server is already running".into());
        }

        if self.filesystem.is_full() {
            self.log_daemon_error("disk space is full, cannot start the server")
                .await;
            return Err("disk space is full, cannot start the server".into());
        }

        let success = self
            .state
            .execute_action(
                state::ServerState::Starting,
                |_| async {
                    self.filesystem.setup().await;
                    self.destroy_container(client).await;

                    self.log_daemon_with_prelude("Updating process configuration files...")
                        .await;
                    self.process_configuration
                        .read()
                        .await
                        .update_files(&self.filesystem)
                        .await;

                    if self.config.system.check_permissions_on_boot {
                        self.log_daemon_with_prelude(
                            "Ensuring file permissions are set correctly, this could take a few seconds...",
                        )
                        .await;

                        self.filesystem.chown_path(&self.filesystem.base_path).await;
                    }

                    self.pull_image(
                        client,
                        self.configuration.read().await.container.image.clone(),
                    )
                    .await?;

                    self.setup_container(client).await?;

                    let container = match &*self.container.read().await {
                        Some(container) => container.docker_id.clone(),
                        None => {
                            return Ok(());
                        }
                    };

                    Ok(client.start_container::<String>(&container, None).await?)
                },
                aquire_timeout,
            )
            .await;

        if !success {
            self.log_daemon_error("another power action is currently being processed for this server, please try again later")
                .await;
        }

        Ok(())
    }

    pub async fn kill(&self, client: &bollard::Docker) -> Result<(), bollard::errors::Error> {
        if self.state.get_state() == state::ServerState::Offline {
            self.log_daemon_error("server is already offline").await;
            return Ok(());
        }

        let container = match &*self.container.read().await {
            Some(container) => container.docker_id.clone(),
            None => {
                return Ok(());
            }
        };

        self.stopping
            .store(true, std::sync::atomic::Ordering::Relaxed);
        client
            .kill_container(
                &container,
                Some(bollard::container::KillContainerOptions {
                    signal: "SIGKILL".to_string(),
                }),
            )
            .await?;

        Ok(())
    }

    pub async fn stop(
        &self,
        client: &Arc<bollard::Docker>,
        aquire_timeout: Option<std::time::Duration>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        if self.state.get_state() == state::ServerState::Offline {
            self.log_daemon_error("server is already stopped").await;
            return Err("server is already stopped".into());
        }

        if self.state.get_state() == state::ServerState::Stopping {
            self.log_daemon_error("server is already stopping").await;
            return Err("server is already stopping".into());
        }

        let container = match &*self.container.read().await {
            Some(container) => container.docker_id.clone(),
            None => {
                return Ok(());
            }
        };

        let success = self
            .state
            .execute_action(
                state::ServerState::Stopping,
                |_| async {
                    let stop = &self.process_configuration.read().await.stop;

                    match stop.r#type.as_str() {
                        "signal" => {
                            tokio::spawn({
                                let client = Arc::clone(client);
                                let container = container.clone();
                                let value = stop.value.clone();

                                async move {
                                    client
                                        .kill_container(
                                            &container,
                                            Some(bollard::container::KillContainerOptions {
                                                signal: match value {
                                                    Some(signal) => {
                                                        match signal.to_uppercase().as_str() {
                                                            "SIGABRT" => "SIGABRT".to_string(),
                                                            "SIGINT" => "SIGINT".to_string(),
                                                            "SIGTERM" => "SIGTERM".to_string(),
                                                            "SIGQUIT" => "SIGQUIT".to_string(),
                                                            "SIGKILL" => "SIGKILL".to_string(),
                                                            _ => {
                                                                crate::logger::log(
                                                                    crate::logger::LoggerLevel::Debug,
                                                                    format!(
                                                                        "Invalid signal: {}, defaulting to SIGKILL",
                                                                        signal
                                                                    ),
                                                                );

                                                                "SIGKILL".to_string()
                                                            }
                                                        }
                                                    }
                                                    _ => "SIGKILL".to_string(),
                                                },
                                            }),
                                        )
                                        .await
                                        .unwrap()
                                }
                            });

                            Ok(())
                        }
                        "command" => {
                            if let Some(stdin) = self.container_stdin().await {
                                let command = stop.value.clone().unwrap_or_default();
                                let mut command = command.to_string();
                                command.push('\n');

                                if let Err(err) = stdin.send(command).await {
                                    crate::logger::log(
                                        crate::logger::LoggerLevel::Debug,
                                        format!("Failed to send stop command to docker: {}", err),
                                    );
                                }
                            } else {
                                crate::logger::log(
                                    crate::logger::LoggerLevel::Debug,
                                    "Container stdin is not available for stopping (what)"
                                        .to_string(),
                                );
                            }

                            Ok(())
                        }
                        _ => {
                            crate::logger::log(
                                crate::logger::LoggerLevel::Debug,
                                format!(
                                    "Invalid stop type: {}, defaulting to docker stop",
                                    stop.r#type
                                ),
                            );

                            tokio::spawn({
                                let client = Arc::clone(client);
                                let container = container.clone();

                                async move {
                                    client
                                        .stop_container(
                                            &container,
                                            Some(bollard::container::StopContainerOptions {
                                                t: -1,
                                            }),
                                        )
                                        .await
                                        .unwrap()
                                }
                            });

                            Ok(())
                        }
                    }
                },
                aquire_timeout,
            )
            .await;

        if !success {
            self.log_daemon_error("another power action is currently being processed for this server, please try again later")
                .await;
            Err("another power action is currently being processed for this server, please try again later".into())
        } else {
            self.stopping
                .store(true, std::sync::atomic::Ordering::Relaxed);
            Ok(())
        }
    }

    pub async fn restart(
        &self,
        client: &Arc<bollard::Docker>,
        aquire_timeout: Option<std::time::Duration>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        if self.restarting.load(std::sync::atomic::Ordering::Relaxed) {
            self.log_daemon_error("server is already restarting").await;
            return Err("server is already restarting".into());
        }

        if self.state.get_state() != state::ServerState::Offline {
            self.stop(client, aquire_timeout).await?;
            self.restarting
                .store(true, std::sync::atomic::Ordering::Relaxed);
        } else {
            self.start(client, aquire_timeout).await?;
        }

        Ok(())
    }

    pub async fn destroy_container(&self, client: &bollard::Docker) {
        if let Ok(containers) = client
            .list_containers(Some(bollard::container::ListContainersOptions {
                all: true,
                filters: HashMap::from([("name".to_string(), vec![self.uuid.to_string()])]),
                ..Default::default()
            }))
            .await
        {
            if let Some(container) = containers.first() {
                let container = container.id.clone().unwrap();

                if let Err(e) = client
                    .remove_container(
                        &container,
                        Some(bollard::container::RemoveContainerOptions {
                            force: true,
                            ..Default::default()
                        }),
                    )
                    .await
                {
                    crate::logger::log(
                        crate::logger::LoggerLevel::Error,
                        format!("Failed to remove container {}: {}", container, e),
                    );
                }
            }
        }

        self.container.write().await.take();
    }

    pub async fn destroy(&self, client: &bollard::Docker) {
        self.kill(client).await.ok();
        self.destroy_container(client).await;
        self.filesystem.destroy().await;
    }

    pub async fn to_api_response(&self) -> serde_json::Value {
        json!({
            "state": self.state.get_state(),
            "is_suspended": self.suspended.load(std::sync::atomic::Ordering::Relaxed),
            "utilization": self.resource_usage().await,
            "configuration": *self.configuration.read().await,
        })
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        if let Some(handle) = self.websocket_sender.blocking_write().take() {
            handle.abort();
        }
    }
}
