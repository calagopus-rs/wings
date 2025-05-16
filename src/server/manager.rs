use super::{Server, state::ServerState};
use std::{
    collections::HashMap,
    fs::File,
    io::{Seek, Write},
    path::Path,
    sync::Arc,
};
use tokio::sync::RwLock;

pub struct Manager {
    state_writer: tokio::task::JoinHandle<()>,
    config: Arc<crate::config::Config>,
    client: Arc<bollard::Docker>,

    pub servers: Arc<RwLock<Vec<Arc<Server>>>>,
}

impl Manager {
    pub async fn new(
        config: Arc<crate::config::Config>,
        client: Arc<bollard::Docker>,
        raw_servers: Vec<crate::remote::servers::RawServer>,
    ) -> Arc<Self> {
        let states_path = Path::new(&config.system.root_directory).join("states.json");
        let mut states: HashMap<uuid::Uuid, ServerState> = serde_json::from_str(
            tokio::fs::read_to_string(&states_path)
                .await
                .unwrap_or_default()
                .as_str(),
        )
        .unwrap_or_default();
        let mut servers = Vec::new();

        for s in raw_servers {
            let server = Server::new(s.settings, s.process_configuration, Arc::clone(&config));

            let state = states.remove(&server.uuid).unwrap_or_default();

            let server = Arc::new(server);
            server
                .setup_websocket_sender(Arc::clone(&server), Arc::clone(&client))
                .await;

            if state == ServerState::Starting || state == ServerState::Running {
                tokio::spawn({
                    let server = Arc::clone(&server);
                    let client = Arc::clone(&client);

                    async move {
                        crate::logger::log(
                            crate::logger::LoggerLevel::Info,
                            format!("Restoring server {} state: {:?}", server.uuid, state),
                        );

                        server.attach_container(&client).await.unwrap();

                        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                        if server.state.get_state() != ServerState::Running
                            && state != ServerState::Starting
                        {
                            server.start(&client, None).await.ok();
                        }
                    }
                });
            }

            servers.push(server);
        }

        let servers = Arc::new(RwLock::new(servers));

        Arc::new(Self {
            state_writer: tokio::spawn({
                let servers = Arc::clone(&servers);
                let mut states_file = File::create(&states_path).unwrap();

                async move {
                    loop {
                        let servers = servers
                            .read()
                            .await
                            .iter()
                            .map(|s| (s.uuid, s.state.get_state()))
                            .collect::<HashMap<_, _>>();

                        states_file.set_len(0).unwrap();
                        states_file.seek(std::io::SeekFrom::Start(0)).unwrap();
                        serde_json::to_writer(&mut states_file, &servers).unwrap();
                        states_file.flush().unwrap();
                        states_file.sync_all().unwrap();

                        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                    }
                }
            }),

            config,
            client,
            servers,
        })
    }

    pub async fn get_servers(&self) -> tokio::sync::RwLockReadGuard<'_, Vec<Arc<Server>>> {
        self.servers.read().await
    }

    pub async fn create_server(&self, raw_server: crate::remote::servers::RawServer) {
        let server = Arc::new(Server::new(
            raw_server.settings,
            raw_server.process_configuration,
            Arc::clone(&self.config),
        ));

        server.filesystem.setup().await;
        server
            .setup_websocket_sender(Arc::clone(&server), Arc::clone(&self.client))
            .await;

        tokio::spawn({
            let server = Arc::clone(&server);
            let client = Arc::clone(&self.client);

            async move {
                if let Err(err) =
                    crate::server::installation::install_server(&server, &client, false).await
                {
                    crate::logger::log(
                        crate::logger::LoggerLevel::Error,
                        format!("Failed to reinstall server: {}", err),
                    );
                } else if server
                    .configuration
                    .read()
                    .await
                    .start_on_completion
                    .is_some_and(|s| s)
                {
                    if let Err(err) = server.start(&client, None).await {
                        crate::logger::log(
                            crate::logger::LoggerLevel::Error,
                            format!("Failed to start server: {}", err),
                        );
                    }
                }
            }
        });

        self.servers.write().await.push(server);
    }

    pub async fn delete_server(&self, server: Arc<Server>) {
        let mut servers = self.servers.write().await;

        if let Some(pos) = servers.iter().position(|s| Arc::ptr_eq(s, &server)) {
            let server = servers.remove(pos);
            server
                .suspended
                .store(true, std::sync::atomic::Ordering::Relaxed);

            tokio::spawn({
                let client = Arc::clone(&self.client);

                async move { server.destroy(&client).await }
            });
        }
    }
}

impl Drop for Manager {
    fn drop(&mut self) {
        self.state_writer.abort();
    }
}
