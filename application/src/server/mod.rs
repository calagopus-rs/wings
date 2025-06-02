use bollard::secret::ContainerStateStatusEnum;
use futures_util::StreamExt;
use serde_json::json;
use std::{
    collections::HashMap,
    ops::Deref,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio::sync::{Mutex, RwLock};

pub mod activity;
pub mod backup;
pub mod configuration;
pub mod container;
pub mod filesystem;
pub mod installation;
pub mod manager;
pub mod permissions;
pub mod resources;
pub mod state;
pub mod transfer;
pub mod websocket;

pub struct InnerServer {
    pub uuid: uuid::Uuid,
    config: Arc<crate::config::Config>,

    pub configuration: RwLock<configuration::ServerConfiguration>,
    pub process_configuration: RwLock<configuration::process::ProcessConfiguration>,

    pub websocket: tokio::sync::broadcast::Sender<websocket::WebsocketMessage>,
    // Dummy receiver to avoid channel being closed
    _websocket_receiver: tokio::sync::broadcast::Receiver<websocket::WebsocketMessage>,
    websocket_sender: RwLock<Option<tokio::task::JoinHandle<()>>>,

    pub container: RwLock<Option<Arc<container::Container>>>,
    pub activity: activity::ActivityManager,

    pub state: state::ServerStateLock,
    pub outgoing_transfer: RwLock<Option<transfer::OutgoingServerTransfer>>,
    pub incoming_transfer: RwLock<Option<tokio::task::JoinHandle<()>>>,
    pub installation_script: RwLock<Option<(bool, installation::InstallationScript)>>,

    suspended: AtomicBool,
    installing: AtomicBool,
    restoring: AtomicBool,
    pub transferring: AtomicBool,

    restarting: AtomicBool,
    stopping: AtomicBool,
    last_crash: Mutex<Option<std::time::Instant>>,
    crash_handled: AtomicBool,

    pub filesystem: filesystem::Filesystem,
}

pub struct Server(pub Arc<InnerServer>);

impl Server {
    pub fn new(
        configuration: configuration::ServerConfiguration,
        process_configuration: configuration::process::ProcessConfiguration,
        config: Arc<crate::config::Config>,
    ) -> Self {
        tracing::info!(
            server = %configuration.uuid,
            "creating server instance"
        );

        let filesystem = filesystem::Filesystem::new(
            configuration.uuid,
            configuration.build.disk_space * 1024 * 1024,
            config.system.disk_check_interval,
            Arc::clone(&config),
            &configuration.egg.file_denylist,
        );

        let (rx, tx) = tokio::sync::broadcast::channel(128);

        let state = state::ServerStateLock::new(rx.clone());
        let activity = activity::ActivityManager::new(configuration.uuid, &config);

        Self(Arc::new(InnerServer {
            uuid: configuration.uuid,

            config,

            configuration: RwLock::new(configuration),
            process_configuration: RwLock::new(process_configuration),

            websocket: rx,
            _websocket_receiver: tx,
            websocket_sender: RwLock::new(None),

            container: RwLock::new(None),
            activity,

            state,
            outgoing_transfer: RwLock::new(None),
            incoming_transfer: RwLock::new(None),
            installation_script: RwLock::new(None),

            suspended: AtomicBool::new(false),
            installing: AtomicBool::new(false),
            restoring: AtomicBool::new(false),
            transferring: AtomicBool::new(false),

            restarting: AtomicBool::new(false),
            stopping: AtomicBool::new(false),
            last_crash: Mutex::new(None),
            crash_handled: AtomicBool::new(false),

            filesystem,
        }))
    }

    pub fn setup_websocket_sender(
        &self,
        container: Arc<container::Container>,
        client: Arc<bollard::Docker>,
    ) -> Pin<Box<dyn std::future::Future<Output = ()> + Send>> {
        tracing::debug!(
            server = %self.uuid,
            "setting up websocket sender"
        );
        let server = self.clone();

        Box::pin(async move {
            let old_sender = server.clone().websocket_sender.write().await.replace(tokio::spawn(async move {
            let mut prev_usage = resources::ResourceUsage::default();

            let mut container_channel = match container.update_reciever.lock().await.take() {
                Some(channel) => channel,
                None => {
                    tracing::error!(
                        server = %server.uuid,
                        "failed to get container channel"
                    );
                    return;
                }
            };

            loop {
                let (container_state, usage) = match container_channel.recv().await {
                    Some((container_state, usage)) => (container_state, usage),
                    None => break,
                };

                if usage != prev_usage {
                    let message = websocket::WebsocketMessage::new(
                        websocket::WebsocketEvent::ServerStats,
                        &[serde_json::to_string(&usage).unwrap()],
                    );

                    if let Err(err) = server.websocket.send(message) {
                        tracing::error!(
                            server = %server.uuid,
                            "failed to send websocket message: {}",
                            err
                        );
                    }

                    prev_usage = usage;
                }

                if server.filesystem.is_full().await
                    && server.state.get_state() != state::ServerState::Offline
                    && !server.stopping.load(Ordering::Relaxed)
                {
                    server
                    .log_daemon_with_prelude("Server is exceeding the assigned disk space limit, stopping process now.")
                    .await;

                    let client_clone = Arc::clone(&client);
                    let server_clone = server.clone();
                    tokio::spawn(async move {
                        server_clone
                            .stop_with_kill_timeout(
                                &client_clone,
                                std::time::Duration::from_secs(30),
                            )
                            .await;
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
                        ContainerStateStatusEnum::EMPTY
                        | ContainerStateStatusEnum::DEAD
                        | ContainerStateStatusEnum::EXITED => {
                            server.state.set_state(state::ServerState::Offline);

                            if server.restarting.load(Ordering::Relaxed) {
                                server
                                    .crash_handled
                                    .store(true, Ordering::Relaxed);
                                server
                                    .restarting
                                    .store(false, Ordering::Relaxed);
                                server
                                    .stopping
                                    .store(false, Ordering::Relaxed);

                                let client = Arc::clone(&client);
                                let server = server.clone();
                                tokio::spawn(async move {
                                    if let Err(err) = server.start(&client, Some(std::time::Duration::from_secs(5))).await {
                                        tracing::error!(
                                            server = %server.uuid,
                                            "failed to start server after stopping to restart: {}",
                                            err
                                        );
                                    }
                                });
                            } else if server.stopping.load(Ordering::Relaxed)
                            {
                                server
                                    .crash_handled
                                    .store(true, Ordering::Relaxed);
                                server
                                    .stopping
                                    .store(false, Ordering::Relaxed);
                            } else if server.config.system.crash_detection.enabled
                                && server.configuration.read().await.crash_detection_enabled
                                && !server
                                    .crash_handled
                                    .load(Ordering::Relaxed)
                            {
                                server
                                    .crash_handled
                                    .store(true, Ordering::Relaxed);

                                if container_state.exit_code.is_some_and(|code| code == 0)
                                    && !container_state.oom_killed.unwrap_or(false)
                                    && !server
                                        .config
                                        .system
                                        .crash_detection
                                        .detect_clean_exit_as_crash
                                {
                                    tracing::debug!(
                                        server = %server.uuid,
                                        "container exited cleanly, not restarting due to crash detection settings"
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

                                let mut last_crash_lock = server.last_crash.lock().await;
                                if let Some(last_crash) = *last_crash_lock {
                                    if last_crash.elapsed().as_secs()
                                        < server.config.system.crash_detection.timeout
                                    {
                                        tracing::debug!(
                                            server = %server.uuid,
                                            "last crash was less than {} seconds ago, aborting automatic restart",
                                            server.config.system.crash_detection.timeout
                                        );

                                        server.log_daemon_with_prelude(
                                        &format!(
                                            "Aborting automatic restart, last crash occurred less than {} seconds ago.",
                                            server.config.system.crash_detection.timeout
                                        ),
                                    ).await;
                                        return;
                                    } else {
                                        tracing::debug!(
                                            server = %server.uuid,
                                            "last crash was more than {} seconds ago, restarting server",
                                            server.config.system.crash_detection.timeout
                                        );

                                        last_crash_lock.replace(std::time::Instant::now());
                                    }
                                } else {
                                    tracing::debug!(
                                        server = %server.uuid,
                                        "no previous crash recorded, restarting server"
                                    );

                                    last_crash_lock.replace(std::time::Instant::now());
                                }

                                drop(last_crash_lock);

                                tracing::info!(
                                    server = %server.uuid,
                                    "restarting server due to crash"
                                );

                                let client = Arc::clone(&client);
                                let server = server.clone();
                                tokio::spawn(async move {
                                    if let Err(err) = server.start(&client, Some(std::time::Duration::from_secs(5))).await {
                                        tracing::error!(
                                            server = %server.uuid,
                                            "failed to start server after crash: {}",
                                            err
                                        );
                                    }
                                });
                            }

                            break;
                        }
                        _ => {}
                    }
                }
            }
        }));

            if let Some(old_sender) = old_sender {
                old_sender.abort();
            }
        })
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
                disk_bytes: self.filesystem.limiter_usage().await,
                state: self.state.get_state(),
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
        self.filesystem
            .update_ignored(&configuration.egg.file_denylist)
            .await;
        *self.configuration.write().await = configuration;
        *self.process_configuration.write().await = process_configuration;

        if let Err(err) = self.sync_container(client).await {
            tracing::error!(
                server = %self.uuid,
                "failed to sync container: {}",
                err
            );
        }
    }

    pub async fn sync_configuration(&self, client: &Arc<bollard::Docker>) {
        match self.config.client.server(self.uuid).await {
            Ok(configuration) => {
                self.update_configuration(
                    configuration.settings,
                    configuration.process_configuration,
                    client,
                )
                .await;
            }
            Err(err) => {
                tracing::error!(
                    server = %self.uuid,
                    "failed to sync server configuration: {}",
                    err
                );
            }
        }
    }

    pub fn reset_state(&self) {
        self.state.set_state(state::ServerState::Offline);
    }

    #[inline]
    pub fn is_locked_state(&self) -> bool {
        self.suspended.load(Ordering::SeqCst)
            || self.installing.load(Ordering::SeqCst)
            || self.restoring.load(Ordering::SeqCst)
            || self.transferring.load(Ordering::SeqCst)
    }

    pub async fn setup_container(
        &self,
        client: &Arc<bollard::Docker>,
    ) -> Result<(), bollard::errors::Error> {
        self.crash_handled.store(false, Ordering::Relaxed);

        if self.container.read().await.is_some() {
            return Ok(());
        }

        tracing::info!(
            server = %self.uuid,
            "setting up container"
        );

        let name = &self.configuration.read().await.meta.name;
        let mut name_filtered = "".to_string();
        for c in name.chars() {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                name_filtered.push(c);
            }
        }

        name_filtered.truncate(63 - 1 - 36);

        let container = client
            .create_container(
                Some(bollard::container::CreateContainerOptions {
                    name: if self.config.docker.server_name_in_container_name {
                        format!("{}.{}", name_filtered, self.configuration.read().await.uuid)
                    } else {
                        self.configuration.read().await.uuid.to_string()
                    },
                    ..Default::default()
                }),
                self.configuration
                    .read()
                    .await
                    .container_config(&self.config, client, &self.filesystem)
                    .await,
            )
            .await?;

        let container = Arc::new(
            container::Container::new(
                container.id.clone(),
                self.process_configuration.read().await.startup.clone(),
                Arc::clone(client),
                self.clone(),
            )
            .await?,
        );

        self.setup_websocket_sender(Arc::clone(&container), Arc::clone(client))
            .await;
        *self.container.write().await = Some(container);

        Ok(())
    }

    pub async fn attach_container(
        &self,
        client: &Arc<bollard::Docker>,
    ) -> Result<(), bollard::errors::Error> {
        if self.container.read().await.is_some() {
            return Ok(());
        }

        tracing::info!(
            server = %self.uuid,
            "attaching to container"
        );

        if let Ok(containers) = client
            .list_containers(Some(bollard::container::ListContainersOptions {
                all: true,
                filters: HashMap::from([("name".to_string(), vec![self.uuid.to_string()])]),
                ..Default::default()
            }))
            .await
        {
            for container in containers {
                if container
                    .names
                    .as_ref()
                    .is_some_and(|names| names.iter().any(|name| name.contains("installer")))
                {
                    tracing::debug!(
                        server = %self.uuid,
                        "installer container found, skipping attachment"
                    );

                    continue;
                }

                let container = container.id.clone().unwrap();
                let container = Arc::new(
                    container::Container::new(
                        container.to_string(),
                        self.process_configuration.read().await.startup.clone(),
                        Arc::clone(client),
                        self.clone(),
                    )
                    .await?,
                );

                self.crash_handled.store(true, Ordering::Relaxed);
                self.setup_websocket_sender(Arc::clone(&container), Arc::clone(client))
                    .await;
                *self.container.write().await = Some(container);

                tokio::spawn({
                    let server = self.clone();

                    async move {
                        tokio::time::sleep(std::time::Duration::from_secs(5)).await;

                        if server.state.get_state() != state::ServerState::Offline {
                            server.crash_handled.store(false, Ordering::Relaxed);
                        }
                    }
                });
            }
        }

        Ok(())
    }

    pub async fn sync_container(
        &self,
        client: &bollard::Docker,
    ) -> Result<(), bollard::errors::Error> {
        self.filesystem
            .update_disk_limit(self.configuration.read().await.build.disk_space * 1024 * 1024)
            .await;

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
            .ok();
    }

    pub async fn log_daemon_install(&self, message: String) {
        self.websocket
            .send(websocket::WebsocketMessage::new(
                websocket::WebsocketEvent::ServerInstallOutput,
                &[message],
            ))
            .ok();
    }

    pub async fn log_daemon_with_prelude(&self, message: &str) {
        let prelude = ansi_term::Color::Yellow
            .bold()
            .paint(format!("[{} Daemon]:", self.config.app_name));

        self.websocket
            .send(websocket::WebsocketMessage::new(
                websocket::WebsocketEvent::ServerConsoleOutput,
                &[format!(
                    "{} {}",
                    prelude,
                    ansi_term::Style::new().bold().paint(message)
                )],
            ))
            .ok();
    }

    pub async fn log_daemon_error(&self, message: &str) {
        self.log_daemon(
            ansi_term::Style::new()
                .bold()
                .on(ansi_term::Color::Red)
                .paint(message)
                .to_string(),
        )
        .await
    }

    pub async fn pull_image(
        &self,
        client: &Arc<bollard::Docker>,
        image: &str,
    ) -> Result<(), bollard::errors::Error> {
        tracing::info!(
            server = %self.uuid,
            image = %image,
            "pulling image"
        );

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

            let (image, tag) = image.split_once(':').unwrap_or((image, "latest"));

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
                        tracing::error!(
                            server = %self.uuid,
                            image = %image,
                            "failed to pull image: {}",
                            err
                        );

                        self.log_daemon_error(&format!("failed to pull image: {}", err))
                            .await;

                        if let Ok(images) = client
                            .list_images(Some(bollard::image::ListImagesOptions {
                                all: true,
                                filters: HashMap::from([("reference", vec![image])]),
                                ..Default::default()
                            }))
                            .await
                        {
                            if images.is_empty() {
                                return Err(err);
                            } else {
                                tracing::error!(
                                    server = %self.uuid,
                                    image = %image,
                                    "image already exists, ignoring error: {}",
                                    err
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
            return Err("server is already running".into());
        }

        if self.filesystem.is_full().await {
            self.log_daemon_error("disk space is full, cannot start the server")
                .await;
            return Err("disk space is full, cannot start the server".into());
        }

        tracing::info!(
            server = %self.uuid,
            "starting server"
        );

        let success = self
            .state
            .execute_action(
                state::ServerState::Starting,
                |_| async {
                    self.filesystem.setup().await;
                    self.destroy_container(client).await;

                    self.sync_configuration(client).await;

                    self.log_daemon_with_prelude("Updating process configuration files...")
                        .await;
                    if let Err(err) = self.process_configuration
                        .read()
                        .await
                        .update_files(self)
                        .await {
                        tracing::error!(
                            server = %self.uuid,
                            "failed to update process configuration files: {}",
                            err
                        );
                    }

                    if self.config.system.check_permissions_on_boot {
                        tracing::debug!(
                            server = %self.uuid,
                            "checking permissions on boot"
                        );
                        self.log_daemon_with_prelude(
                            "Ensuring file permissions are set correctly, this could take a few seconds...",
                        )
                        .await;

                        self.filesystem.chown_path(&self.filesystem.base_path).await;
                    }

                    self.pull_image(
                        client,
                        &self.configuration.read().await.container.image,
                    )
                    .await?;

                    self.setup_container(client).await?;

                    let container = match &*self.container.read().await {
                        Some(container) => container.docker_id.clone(),
                        None => {
                            return Ok(());
                        }
                    };

                    if let Err(err) = client.start_container::<String>(&container, None).await {
                        tracing::error!(
                            server = %self.uuid,
                            "failed to start container: {}",
                            err
                        );

                        self.log_daemon_error(&format!("failed to start container: {}", err))
                            .await;

                        return Err(anyhow::anyhow!(err));
                    }

                    Ok(())
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
            return Ok(());
        }

        let container = match &*self.container.read().await {
            Some(container) => container.docker_id.clone(),
            None => {
                return Ok(());
            }
        };

        tracing::info!(
            server = %self.uuid,
            "killing server"
        );

        self.stopping.store(true, Ordering::Relaxed);
        if client
            .kill_container(
                &container,
                Some(bollard::container::KillContainerOptions {
                    signal: "SIGKILL".to_string(),
                }),
            )
            .await
            .is_err()
        {
            self.reset_state();
        }

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

        tracing::info!(
            server = %self.uuid,
            "stopping server"
        );

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
                                let server = self.clone();

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
                                                                tracing::error!(
                                                                    server = %server.uuid,
                                                                    "invalid signal: {}, defaulting to SIGKILL",
                                                                    signal
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
                                    tracing::error!(
                                        server = %self.uuid,
                                        "failed to send command to container stdin: {}",
                                        err
                                    );
                                }
                            } else {
                                tracing::error!(
                                    server = %self.uuid,
                                    "failed to get container stdin"
                                );
                            }

                            Ok(())
                        }
                        _ => {
                            tracing::error!(
                                server = %self.uuid,
                                "invalid stop type: {}, defaulting to docker stop",
                                stop.r#type
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
            self.stopping.store(true, Ordering::Relaxed);

            Ok(())
        }
    }

    pub async fn restart(
        &self,
        client: &Arc<bollard::Docker>,
        aquire_timeout: Option<std::time::Duration>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        if self.restarting.load(Ordering::Relaxed) {
            self.log_daemon_error("server is already restarting").await;
            return Err("server is already restarting".into());
        }

        tracing::info!(
            server = %self.uuid,
            "restarting server"
        );

        if self.state.get_state() != state::ServerState::Offline {
            self.stop(client, aquire_timeout).await?;
            self.restarting.store(true, Ordering::Relaxed);
        } else {
            self.start(client, aquire_timeout).await?;
        }

        Ok(())
    }

    pub async fn stop_with_kill_timeout(
        &self,
        client: &Arc<bollard::Docker>,
        timeout: std::time::Duration,
    ) {
        if self.state.get_state() == state::ServerState::Offline {
            return;
        }

        tracing::info!(
            server = %self.uuid,
            "stopping server with kill timeout {}s",
            timeout.as_secs()
        );

        let mut stream = client.wait_container::<String>(
            &self.container.read().await.as_ref().unwrap().docker_id,
            None,
        );

        self.stop(client, None).await.ok();

        if tokio::time::timeout(timeout, stream.next()).await.is_err() {
            tracing::info!(
                server = %self.uuid,
                "kill timeout reached, killing server"
            );

            self.kill(client).await.ok();
        }
    }

    pub async fn destroy_container(&self, client: &bollard::Docker) {
        tracing::info!(
            server = %self.uuid,
            "destroying container"
        );

        if let Ok(containers) = client
            .list_containers(Some(bollard::container::ListContainersOptions {
                all: true,
                filters: HashMap::from([(
                    "name".to_string(),
                    vec![self.uuid.to_string(), format!("{}_installer", self.uuid)],
                )]),
                ..Default::default()
            }))
            .await
        {
            for container in containers {
                let container = container.id.clone().unwrap();

                if let Err(err) = client
                    .remove_container(
                        &container,
                        Some(bollard::container::RemoveContainerOptions {
                            force: true,
                            ..Default::default()
                        }),
                    )
                    .await
                {
                    tracing::error!(
                        server = %self.uuid,
                        container = %container,
                        "failed to remove container: {}",
                        err
                    );
                }
            }
        }

        self.container.write().await.take();
        if let Some(handle) = self.websocket_sender.write().await.take() {
            handle.abort();
        }
    }

    pub async fn destroy(&self, client: &bollard::Docker) {
        tracing::info!(
            server = %self.uuid,
            "destroying server"
        );

        self.kill(client).await.ok();
        self.destroy_container(client).await;
        self.filesystem.destroy().await;
    }

    pub async fn to_api_response(&self) -> serde_json::Value {
        json!({
            "state": self.state.get_state(),
            "is_suspended": self.suspended.load(Ordering::Relaxed),
            "utilization": self.resource_usage().await,
            "configuration": *self.configuration.read().await,
        })
    }
}

impl Clone for Server {
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}

impl Deref for Server {
    type Target = Arc<InnerServer>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
