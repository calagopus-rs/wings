use super::{Server, state::ServerState};
use std::{
    collections::HashMap,
    fs::File,
    io::{Seek, Write},
    path::Path,
    sync::Arc,
};
use tokio::sync::{RwLock, Semaphore};

pub struct Manager {
    config: Arc<crate::config::Config>,
    client: Arc<bollard::Docker>,

    pub servers: Arc<RwLock<Vec<Server>>>,
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
        let installing_path = Path::new(&config.system.root_directory).join("installing.json");
        let mut installing: HashMap<uuid::Uuid, (bool, super::installation::InstallationScript)> =
            serde_json::from_str(
                tokio::fs::read_to_string(&installing_path)
                    .await
                    .unwrap_or_default()
                    .as_str(),
            )
            .unwrap_or_default();

        let mut servers = Vec::new();
        let semaphore = Arc::new(Semaphore::new(
            config.remote_query.boot_servers_per_page as usize,
        ));

        for s in raw_servers {
            let server = Server::new(s.settings, s.process_configuration, Arc::clone(&config));
            let state = states.remove(&server.uuid).unwrap_or_default();

            server.filesystem.attach().await;

            if let Some((reinstall, container_script)) = installing.remove(&server.uuid) {
                tokio::spawn({
                    let client = Arc::clone(&client);
                    let server = server.clone();

                    async move {
                        tracing::info!(
                            server = %server.uuid,
                            "restoring installing state {:?}",
                            state
                        );

                        if let Err(err) = super::installation::attach_install_container(
                            &server,
                            &client,
                            container_script,
                            reinstall,
                        )
                        .await
                        {
                            tracing::error!(
                                server = %server.uuid,
                                "failed to attach installation container: {:#?}",
                                err
                            );
                        }
                    }
                });
            } else if config.remote_query.boot_servers_per_page > 0 {
                tokio::spawn({
                    let client = Arc::clone(&client);
                    let semaphore = Arc::clone(&semaphore);
                    let server = server.clone();

                    async move {
                        tracing::info!(
                            server = %server.uuid,
                            "restoring server state {:?}",
                            state
                        );

                        server.attach_container(&client).await.unwrap();

                        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                        if matches!(state, ServerState::Running | ServerState::Starting)
                            && !matches!(
                                server.state.get_state(),
                                ServerState::Running | ServerState::Starting
                            )
                        {
                            let _ = semaphore.acquire().await.unwrap();

                            server.start(&client, None).await.ok();
                        }
                    }
                });
            }

            servers.push(server);
        }

        let servers = Arc::new(RwLock::new(servers));

        tokio::spawn({
            let servers = Arc::clone(&servers);
            let mut states_file = File::create(&states_path).unwrap();

            async move {
                loop {
                    tokio::time::sleep(std::time::Duration::from_secs(10)).await;

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
                }
            }
        });

        tokio::spawn({
            let servers = Arc::clone(&servers);
            let mut installing_file = File::create(&installing_path).unwrap();

            async move {
                loop {
                    tokio::time::sleep(std::time::Duration::from_secs(10)).await;

                    let mut installing = HashMap::new();
                    for server in servers.read().await.iter() {
                        if let Some((reinstall, installation_script)) =
                            server.installation_script.read().await.as_ref()
                        {
                            installing
                                .insert(server.uuid, (*reinstall, installation_script.clone()));
                        }
                    }

                    installing_file.set_len(0).unwrap();
                    installing_file.seek(std::io::SeekFrom::Start(0)).unwrap();
                    serde_json::to_writer(&mut installing_file, &installing).unwrap();
                    installing_file.flush().unwrap();
                    installing_file.sync_all().unwrap();
                }
            }
        });

        Arc::new(Self {
            config,
            client,
            servers,
        })
    }

    pub async fn get_servers(&self) -> tokio::sync::RwLockReadGuard<'_, Vec<Server>> {
        self.servers.read().await
    }

    pub async fn create_server(
        &self,
        raw_server: crate::remote::servers::RawServer,
        install_server: bool,
    ) -> Server {
        let server = Server::new(
            raw_server.settings,
            raw_server.process_configuration,
            Arc::clone(&self.config),
        );

        server.filesystem.setup().await;

        if install_server {
            tokio::spawn({
                let client = Arc::clone(&self.client);
                let server = server.clone();

                async move {
                    if let Err(err) =
                        crate::server::installation::install_server(&server, &client, false).await
                    {
                        tracing::error!(
                            server = %server.uuid,
                            "failed to reinstall server: {:#?}",
                            err
                        );
                    } else if server
                        .configuration
                        .read()
                        .await
                        .start_on_completion
                        .is_some_and(|s| s)
                    {
                        if let Err(err) = server.start(&client, None).await {
                            tracing::error!(
                                server = %server.uuid,
                                "failed to start server on boot: {}",
                                err
                            );
                        }
                    }
                }
            });
        }

        self.servers.write().await.push(server.clone());

        server
    }

    pub async fn delete_server(&self, server: &Server) {
        let mut servers = self.servers.write().await;

        if let Some(pos) = servers.iter().position(|s| Arc::ptr_eq(s, server)) {
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
