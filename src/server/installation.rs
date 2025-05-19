use super::configuration::string_to_option;
use futures_util::StreamExt;
use serde::Deserialize;
use std::{
    collections::HashMap, fs::Permissions, os::unix::fs::PermissionsExt, path::Path, sync::Arc,
};
use tokio::{io::AsyncWriteExt, sync::Mutex};

#[derive(Deserialize, Clone)]
pub struct InstallationScript {
    pub container_image: String,
    pub entrypoint: String,
    pub script: String,
}

async fn container_config(
    server: &super::Server,
    script: &InstallationScript,
) -> tokio::io::Result<bollard::container::Config<String>> {
    let mut labels = HashMap::with_capacity(2);
    labels.insert("Service".to_string(), "Pterodactyl".to_string());
    labels.insert("ContainerType".to_string(), "server_installer".to_string());

    let mut resources = server
        .configuration
        .read()
        .await
        .convert_container_resources(&server.config);

    if resources.memory_reservation.is_some_and(|m| {
        m > 0 && m < server.config.docker.installer_limits.memory as i64 * 1024 * 1024
    }) {
        resources.memory = None;
        resources.memory_reservation =
            Some(server.config.docker.installer_limits.memory as i64 * 1024 * 1024);
    }

    if resources
        .cpu_quota
        .is_some_and(|c| c > 0 && c < server.config.docker.installer_limits.cpu as i64 * 1000)
    {
        resources.cpu_quota = Some(server.config.docker.installer_limits.cpu as i64 * 1000);
    }

    let tmp_dir = Path::new(&server.config.system.tmp_directory).join(server.uuid.to_string());
    tokio::fs::create_dir_all(&tmp_dir).await?;
    tokio::fs::write(
        tmp_dir.join("install.sh"),
        script.script.replace("\r\n", "\n"),
    )
    .await?;
    tokio::fs::set_permissions(&tmp_dir, Permissions::from_mode(0o755)).await?;

    Ok(bollard::container::Config {
        host_config: Some(bollard::secret::HostConfig {
            memory: resources.memory,
            memory_reservation: resources.memory_reservation,
            memory_swap: resources.memory_swap,
            cpu_quota: resources.cpu_quota,
            cpu_period: resources.cpu_period,
            cpu_shares: resources.cpu_shares,
            cpuset_cpus: resources.cpuset_cpus,
            pids_limit: resources.pids_limit,
            blkio_weight: resources.blkio_weight,
            oom_kill_disable: resources.oom_kill_disable,

            mounts: Some(vec![
                bollard::models::Mount {
                    typ: Some(bollard::secret::MountTypeEnum::BIND),
                    source: Some(server.filesystem.base()),
                    target: Some("/mnt/server".to_string()),
                    ..Default::default()
                },
                bollard::models::Mount {
                    typ: Some(bollard::secret::MountTypeEnum::BIND),
                    source: Some(tmp_dir.to_string_lossy().to_string()),
                    target: Some("/mnt/install".to_string()),
                    ..Default::default()
                },
            ]),
            network_mode: Some(server.config.docker.network.mode.clone()),
            dns: Some(server.config.docker.network.dns.clone()),
            tmpfs: Some(HashMap::from([(
                "/tmp".to_string(),
                format!("rw,exec,nosuid,size={}M", server.config.docker.tmpfs_size),
            )])),
            log_config: Some(bollard::secret::HostConfigLogConfig {
                typ: serde_json::to_value(&server.config.docker.log_config.r#type)
                    .unwrap()
                    .as_str()
                    .map(|s| s.to_string()),
                config: Some(server.config.docker.log_config.config.clone()),
            }),
            userns_mode: string_to_option(&server.config.docker.userns_mode),
            ..Default::default()
        }),
        cmd: Some(vec![
            script.entrypoint.clone(),
            "/mnt/install/install.sh".to_string(),
        ]),
        hostname: Some("installer".to_string()),
        image: Some(script.container_image.clone()),
        env: Some(server.configuration.read().await.environment()),
        labels: Some(labels),
        attach_stdin: Some(true),
        attach_stdout: Some(true),
        attach_stderr: Some(true),
        open_stdin: Some(true),
        tty: Some(true),
        ..Default::default()
    })
}

async fn cleanup_container(
    server: &super::Server,
    client: &bollard::Docker,
    container_id: &str,
    container_script: &InstallationScript,
    container_env: Vec<String>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut logs_stream = client.logs::<String>(
        container_id,
        Some(bollard::container::LogsOptions {
            follow: false,
            stdout: true,
            stderr: true,
            timestamps: false,
            ..Default::default()
        }),
    );

    let mut env = String::new();
    for var in container_env {
        env.push_str(&format!("  {}\n", var));
    }

    let log_path = Path::new(&server.config.system.log_directory)
        .join("install")
        .join(format!("{}.log", server.uuid));
    tokio::fs::create_dir_all(log_path.parent().unwrap()).await?;

    let mut file = tokio::io::BufWriter::new(tokio::fs::File::create(&log_path).await?);
    file.write_all(
        format!(
            r"Pterodactyl Server Installation Log

|
| Details
| ------------------------------
  Server UUID:          {}
  Container Image:      {}
  Container Entrypoint: {}

|
| Environment Variables
| ------------------------------
{env}

|
| Script Output
| ------------------------------
",
            server.uuid, container_script.container_image, container_script.entrypoint,
        )
        .as_bytes(),
    )
    .await?;

    while let Some(Ok(log)) = logs_stream.next().await {
        file.write_all(&log.into_bytes()).await?;
    }

    file.flush().await?;

    Ok(client
        .remove_container(
            container_id,
            Some(bollard::container::RemoveContainerOptions {
                force: true,
                ..Default::default()
            }),
        )
        .await?)
}

pub async fn install_server(
    server: &super::Server,
    client: &Arc<bollard::Docker>,
    reinstall: bool,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    if server.configuration.read().await.skip_egg_scripts && !reinstall {
        return Ok(());
    }

    if server.is_locked_state() {
        return Err("Server is in a locked state".into());
    }

    server
        .installing
        .store(true, std::sync::atomic::Ordering::SeqCst);
    server
        .websocket
        .send(super::websocket::WebsocketMessage::new(
            super::websocket::WebsocketEvent::ServerInstallStarted,
            &[],
        ))?;

    tracing::info!(
        server = %server.uuid,
        "starting installation process"
    );

    server
        .log_daemon("Starting installation process, this could take a few minutes...".to_string())
        .await;

    let container_id: Mutex<Option<String>> = Mutex::new(None);
    let container_script: Mutex<Option<InstallationScript>> = Mutex::new(None);
    let unset_installing = async |successful: bool| {
        server
            .installing
            .store(false, std::sync::atomic::Ordering::SeqCst);

        let environment = server.configuration.read().await.environment();
        if let Some(container_id) = container_id.lock().await.take() {
            if let Some(script) = container_script.lock().await.take() {
                if let Err(err) =
                    cleanup_container(server, client, &container_id, &script, environment).await
                {
                    tracing::error!(
                        server = %server.uuid,
                        container = %container_id,
                        "failed to clean up container: {}",
                        err
                    );
                }
            } else if let Err(err) = cleanup_container(
                server,
                client,
                &container_id,
                &InstallationScript {
                    container_image: String::new(),
                    entrypoint: String::new(),
                    script: String::new(),
                },
                environment,
            )
            .await
            {
                tracing::error!(
                    server = %server.uuid,
                    container = %container_id,
                    "failed to clean up container: {}",
                    err
                );
            }
        }

        tokio::fs::remove_dir_all(
            Path::new(&server.config.system.tmp_directory).join(server.uuid.to_string()),
        )
        .await
        .ok();
        if let Err(err) = server
            .config
            .client
            .set_server_install(server.uuid, successful, reinstall)
            .await
        {
            tracing::error!(
                server = %server.uuid,
                "failed to set server install status: {}",
                err
            );
        }

        server
            .websocket
            .send(super::websocket::WebsocketMessage::new(
                super::websocket::WebsocketEvent::ServerInstallCompleted,
                &[],
            ))
    };

    let script = match server
        .config
        .client
        .server_install_script(server.uuid)
        .await
    {
        Ok(script) => script,
        Err(err) => {
            unset_installing(false).await?;
            return Err(err.into());
        }
    };

    *container_script.lock().await = Some(script.clone());

    match server
        .pull_image(client, script.container_image.clone())
        .await
    {
        Ok(_) => {}
        Err(err) => {
            unset_installing(false).await?;
            return Err(err.into());
        }
    }

    let container = match client
        .create_container(
            Some(bollard::container::CreateContainerOptions {
                name: format!("{}_installer", server.uuid),
                ..Default::default()
            }),
            match container_config(server, &script).await {
                Ok(config) => config,
                Err(err) => {
                    unset_installing(false).await?;
                    return Err(err.into());
                }
            },
        )
        .await
    {
        Ok(container) => container,
        Err(err) => {
            unset_installing(false).await?;
            return Err(err.into());
        }
    };

    *container_id.lock().await = Some(container.id.clone());

    match tokio::time::timeout(std::time::Duration::from_secs(15 * 60), async move {
        let thread = tokio::spawn({
            let docker_id = container.id.clone();
            let server = Arc::clone(server);
            let client = Arc::clone(client);

            async move {
                let mut stream = client
                    .attach_container::<String>(
                        &docker_id,
                        Some(bollard::container::AttachContainerOptions {
                            stdout: Some(true),
                            stderr: Some(true),
                            stream: Some(true),
                            ..Default::default()
                        }),
                    )
                    .await
                    .unwrap();

                let mut buffer = Vec::with_capacity(1024);
                let mut line_start = 0;

                while let Some(Ok(data)) = stream.output.next().await {
                    buffer.extend_from_slice(&data.into_bytes());

                    let mut search_start = line_start;

                    loop {
                        if let Some(pos) = buffer[search_start..].iter().position(|&b| b == b'\n') {
                            let newline_pos = search_start + pos;

                            if newline_pos - line_start <= 512 {
                                let line =
                                    String::from_utf8_lossy(&buffer[line_start..newline_pos])
                                        .trim()
                                        .to_string();
                                server
                                    .websocket
                                    .send(super::websocket::WebsocketMessage::new(
                                        super::websocket::WebsocketEvent::ServerInstallOutput,
                                        &[line],
                                    ))
                                    .ok();

                                line_start = newline_pos + 1;
                                search_start = line_start;
                            } else {
                                let line = String::from_utf8_lossy(
                                    &buffer[line_start..(line_start + 512)],
                                )
                                .trim()
                                .to_string();
                                server
                                    .websocket
                                    .send(super::websocket::WebsocketMessage::new(
                                        super::websocket::WebsocketEvent::ServerInstallOutput,
                                        &[line],
                                    ))
                                    .ok();

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
                                server
                                    .websocket
                                    .send(super::websocket::WebsocketMessage::new(
                                        super::websocket::WebsocketEvent::ServerInstallOutput,
                                        &[line],
                                    ))
                                    .ok();

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
                    server
                        .websocket
                        .send(super::websocket::WebsocketMessage::new(
                            super::websocket::WebsocketEvent::ServerInstallOutput,
                            &[line],
                        ))
                        .ok();
                }
            }
        });

        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        client
            .start_container::<String>(&container.id, None)
            .await
            .unwrap();

        let wait_thread = tokio::spawn({
            let client = Arc::clone(client);

            async move {
                client
                    .wait_container::<String>(&container.id, None)
                    .next()
                    .await;
            }
        });

        tokio::try_join!(thread, wait_thread)
    })
    .await
    {
        Ok(_) => {}
        Err(_) => {
            unset_installing(false).await?;
            return Err("Timeout while waiting for installation".into());
        }
    }

    unset_installing(true).await?;

    Ok(())
}
