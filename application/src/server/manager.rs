use super::{Server, state::ServerState};
use std::{
    collections::HashMap,
    path::Path,
    sync::{Arc, atomic::Ordering},
};
use tokio::{
    fs::File,
    io::{AsyncSeekExt, AsyncWriteExt},
    sync::{RwLock, Semaphore},
};

pub struct Manager {
    pub servers: Arc<RwLock<Vec<Server>>>,
}

impl Manager {
    pub fn new(raw_servers: &[crate::remote::servers::RawServer]) -> Self {
        let servers = Vec::with_capacity(raw_servers.len());

        Self {
            servers: Arc::new(RwLock::new(servers)),
        }
    }

    pub async fn boot(
        &self,
        app_state: &crate::routes::State,
        raw_servers: Vec<crate::remote::servers::RawServer>,
    ) {
        let states_path = Path::new(&app_state.config.system.root_directory).join("states.json");
        let mut states: HashMap<uuid::Uuid, ServerState> = serde_json::from_str(
            tokio::fs::read_to_string(&states_path)
                .await
                .unwrap_or_default()
                .as_str(),
        )
        .unwrap_or_default();

        let installing_path =
            Path::new(&app_state.config.system.root_directory).join("installing.json");
        let mut installing: HashMap<uuid::Uuid, (bool, super::installation::InstallationScript)> =
            serde_json::from_str(
                tokio::fs::read_to_string(&installing_path)
                    .await
                    .unwrap_or_default()
                    .as_str(),
            )
            .unwrap_or_default();

        let mut servers = self.servers.write().await;
        let semaphore = Arc::new(Semaphore::new(
            app_state.config.remote_query.boot_servers_per_page as usize,
        ));

        for s in raw_servers {
            let server = Server::new(s.settings, s.process_configuration, app_state.clone());
            let state = states.remove(&server.uuid).unwrap_or_default();

            server.initialize_schedules().await;
            server.filesystem.attach().await;

            if let Some((reinstall, container_script)) = installing.remove(&server.uuid) {
                tokio::spawn({
                    let client = Arc::clone(&app_state.docker);
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
            } else if app_state.config.remote_query.boot_servers_per_page > 0 {
                tokio::spawn({
                    let semaphore = Arc::clone(&semaphore);
                    let server = server.clone();

                    async move {
                        tracing::info!(
                            server = %server.uuid,
                            "restoring server state {:?}",
                            state
                        );

                        match server.attach_container().await {
                            Ok(_) => {
                                tracing::debug!(server = %server.uuid, "server attached successfully");
                            }
                            Err(err) => {
                                tracing::error!(
                                    server = %server.uuid,
                                    error = %err,
                                    "failed to attach server container"
                                );
                            }
                        }

                        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                        if matches!(state, ServerState::Running | ServerState::Starting)
                            && !matches!(
                                server.state.get_state(),
                                ServerState::Running | ServerState::Starting
                            )
                        {
                            let _ = semaphore.acquire().await.unwrap();

                            server.start(None, false).await.ok();
                        }
                    }
                });
            }

            servers.push(server);
        }

        tokio::spawn({
            let servers = Arc::clone(&self.servers);

            async move {
                let mut states_file = match File::create(&states_path).await {
                    Ok(file) => file,
                    Err(err) => {
                        tracing::error!("failed to create states.json file: {:#?}", err);
                        return;
                    }
                };

                let mut run_inner = async || -> Result<(), anyhow::Error> {
                    let servers = servers.read().await;
                    let states: HashMap<_, _> = servers
                        .iter()
                        .map(|s| (s.uuid, s.state.get_state()))
                        .collect();

                    states_file.set_len(0).await?;
                    states_file.seek(std::io::SeekFrom::Start(0)).await?;
                    states_file
                        .write_all(serde_json::to_string(&states)?.as_bytes())
                        .await?;
                    states_file.flush().await?;
                    states_file.sync_all().await?;

                    Ok(())
                };

                loop {
                    tokio::time::sleep(std::time::Duration::from_secs(10)).await;

                    match run_inner().await {
                        Ok(_) => {}
                        Err(err) => {
                            tracing::error!("failed to write states.json file: {:#?}", err);
                            return;
                        }
                    }
                }
            }
        });

        tokio::spawn({
            let servers = Arc::clone(&self.servers);

            async move {
                let mut installing_file = match File::create(&installing_path).await {
                    Ok(file) => file,
                    Err(err) => {
                        tracing::error!("failed to create installing.json file: {:#?}", err);
                        return;
                    }
                };

                let mut run_inner = async || -> Result<(), anyhow::Error> {
                    let mut installing = HashMap::new();
                    for server in servers.read().await.iter() {
                        if let Some((reinstall, installation_script)) =
                            server.installation_script.read().await.as_ref()
                        {
                            installing
                                .insert(server.uuid, (*reinstall, installation_script.clone()));
                        }
                    }

                    installing_file.set_len(0).await?;
                    installing_file.seek(std::io::SeekFrom::Start(0)).await?;
                    installing_file
                        .write_all(serde_json::to_string(&installing)?.as_bytes())
                        .await?;
                    installing_file.flush().await?;
                    installing_file.sync_all().await?;

                    Ok(())
                };

                loop {
                    tokio::time::sleep(std::time::Duration::from_secs(10)).await;

                    match run_inner().await {
                        Ok(_) => {}
                        Err(err) => {
                            tracing::error!("failed to write installing.json file: {:#?}", err);
                            return;
                        }
                    }
                }
            }
        });
    }

    pub async fn get_servers(&self) -> tokio::sync::RwLockReadGuard<'_, Vec<Server>> {
        self.servers.read().await
    }

    pub async fn create_server(
        &self,
        app_state: &crate::routes::State,
        raw_server: crate::remote::servers::RawServer,
        install_server: bool,
    ) -> Server {
        let server = Server::new(
            raw_server.settings,
            raw_server.process_configuration,
            app_state.clone(),
        );

        server.filesystem.setup().await;

        if install_server {
            tokio::spawn({
                let client = Arc::clone(&app_state.docker);
                let server = server.clone();

                async move {
                    if let Err(err) =
                        crate::server::installation::install_server(&server, &client, false, true)
                            .await
                    {
                        tracing::error!(
                            server = %server.uuid,
                            "failed to install server: {:#?}",
                            err
                        );
                    } else if server
                        .configuration
                        .read()
                        .await
                        .start_on_completion
                        .is_some_and(|s| s)
                        && let Err(err) = server.start(None, false).await
                    {
                        tracing::error!(
                            server = %server.uuid,
                            "failed to start server on boot: {}",
                            err
                        );
                    }
                }
            });
        }

        self.servers.write().await.push(server.clone());

        server
    }

    pub async fn delete_server(&self, server: &Server) {
        let mut servers = self.servers.write().await;

        if let Some(pos) = servers.iter().position(|s| s.uuid == server.uuid) {
            let server = servers.remove(pos);
            server.suspended.store(true, Ordering::SeqCst);

            tokio::spawn(async move { server.destroy().await });
        }
    }
}
