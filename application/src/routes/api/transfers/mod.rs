use super::State;
use axum::extract::DefaultBodyLimit;
use utoipa_axum::{
    router::{OpenApiRouter, UtoipaMethodRouterExt},
    routes,
};

mod _server_;

mod post {
    use crate::{
        io::{
            abort::{AbortGuard, AbortReader},
            compression::{CompressionType, reader::CompressionReader},
            hash_reader::HashReader,
            limited_reader::LimitedReader,
        },
        response::{ApiResponse, ApiResponseResult},
        routes::{ApiError, GetState},
        server::transfer::TransferArchiveFormat,
    };
    use axum::{
        extract::Multipart,
        http::{HeaderMap, StatusCode},
    };
    use cap_std::fs::{Permissions, PermissionsExt};
    use futures::TryStreamExt;
    use serde::Serialize;
    use sha1::Digest;
    use std::{io::Write, path::Path, str::FromStr};
    use utoipa::ToSchema;

    #[derive(ToSchema, Serialize)]
    struct Response {}

    #[utoipa::path(post, path = "/", responses(
        (status = OK, body = inline(Response)),
        (status = UNAUTHORIZED, body = ApiError),
        (status = CONFLICT, body = ApiError),
    ))]
    pub async fn route(
        state: GetState,
        headers: HeaderMap,
        mut multipart: Multipart,
    ) -> ApiResponseResult {
        let key = headers
            .get("Authorization")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        let (r#type, token) = match key.split_once(' ') {
            Some((t, tok)) => (t, tok),
            None => {
                return ApiResponse::error("invalid authorization header")
                    .with_status(StatusCode::UNAUTHORIZED)
                    .with_header("WWW-Authenticate", "Bearer")
                    .ok();
            }
        };

        if r#type != "Bearer" {
            return ApiResponse::error("invalid authorization header")
                .with_status(StatusCode::UNAUTHORIZED)
                .ok();
        }

        let payload: crate::remote::jwt::BasePayload = match state.config.jwt.verify(token) {
            Ok(payload) => payload,
            Err(_) => {
                return ApiResponse::error("invalid token")
                    .with_status(StatusCode::UNAUTHORIZED)
                    .ok();
            }
        };

        if !payload.validate(&state.config.jwt).await {
            return ApiResponse::error("invalid token")
                .with_status(StatusCode::UNAUTHORIZED)
                .ok();
        }

        let subject: uuid::Uuid = match payload.subject {
            Some(subject) => match subject.parse() {
                Ok(subject) => subject,
                Err(_) => {
                    return ApiResponse::error("invalid token")
                        .with_status(StatusCode::UNAUTHORIZED)
                        .ok();
                }
            },
            None => {
                return ApiResponse::error("invalid token")
                    .with_status(StatusCode::UNAUTHORIZED)
                    .ok();
            }
        };

        let is_multiplex = headers.contains_key("Multiplex-Stream");
        let multiplex_stream_count: usize = headers
            .get("Multiplex-Stream-Count")
            .map_or(Ok(0), |v| v.to_str().unwrap_or_default().parse())?;

        let server = if !is_multiplex {
            if state
                .server_manager
                .get_servers()
                .await
                .iter()
                .any(|s| s.uuid == subject)
            {
                return ApiResponse::error("server with this uuid already exists")
                    .with_status(StatusCode::CONFLICT)
                    .ok();
            }

            let server_data = state.config.client.server(subject).await?;
            let server = state
                .server_manager
                .create_server(&state, server_data, false)
                .await;

            server
                .transferring
                .store(true, std::sync::atomic::Ordering::SeqCst);
            server
        } else {
            let mut tries = 0;
            let mut server;

            loop {
                server = state
                    .server_manager
                    .get_servers()
                    .await
                    .iter()
                    .find(|s| s.uuid == subject)
                    .cloned();

                tries += 1;

                if server.is_none() {
                    if tries >= 10 {
                        return ApiResponse::error("unable to find transfer for multiplex")
                            .with_status(StatusCode::CONFLICT)
                            .ok();
                    } else {
                        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                    }
                } else {
                    break;
                }
            }

            match server {
                Some(server) => server,
                None => {
                    return ApiResponse::error("server not found")
                        .with_status(StatusCode::NOT_FOUND)
                        .ok();
                }
            }
        };

        let handle = tokio::spawn({
            let runtime = tokio::runtime::Handle::current();
            let server = server.clone();
            let state = state.clone();

            async move {
                let (guard, listener) = AbortGuard::new();

                let handle = tokio::task::spawn_blocking(
                    move || -> Result<Vec<uuid::Uuid>, anyhow::Error> {
                        let mut backups = Vec::new();
                        let mut archive_checksum = None;
                        let mut backup_checksum = None;

                        while let Ok(Some(field)) = runtime.block_on(multipart.next_field()) {
                            if field.name() == Some("archive") {
                                let file_name =
                                    field.file_name().unwrap_or("archive.tar.gz").to_string();
                                let reader = tokio_util::io::StreamReader::new(
                                    field.into_stream().map_err(|err| {
                                        std::io::Error::other(format!(
                                            "failed to read multipart field: {err}"
                                        ))
                                    }),
                                );
                                let reader = tokio_util::io::SyncIoBridge::new(reader);
                                let reader = AbortReader::new(reader, listener.clone());
                                let reader = LimitedReader::new_with_bytes_per_second(
                                    reader,
                                    state.config.system.transfers.download_limit * 1024 * 1024,
                                );
                                let reader =
                                    HashReader::new_with_hasher(reader, sha2::Sha256::new());
                                let reader = CompressionReader::new(
                                    reader,
                                    match TransferArchiveFormat::from_str(&file_name)
                                        .unwrap_or(TransferArchiveFormat::TarGz)
                                    {
                                        TransferArchiveFormat::Tar => CompressionType::None,
                                        TransferArchiveFormat::TarGz => CompressionType::Gz,
                                        TransferArchiveFormat::TarXz => CompressionType::Xz,
                                        TransferArchiveFormat::TarBz2 => CompressionType::Bz2,
                                        TransferArchiveFormat::TarLz4 => CompressionType::Lz4,
                                        TransferArchiveFormat::TarZstd => CompressionType::Zstd,
                                    },
                                );

                                let mut archive = tar::Archive::new(reader);
                                let mut directory_entries = chunked_vec::ChunkedVec::new();
                                let mut entries = archive.entries()?;

                                let mut read_buffer = vec![0; crate::BUFFER_SIZE];
                                while let Some(Ok(mut entry)) = entries.next() {
                                    let path = entry.path()?;

                                    if path.is_absolute() {
                                        continue;
                                    }

                                    let destination_path = path.as_ref();
                                    let header = entry.header();

                                    match header.entry_type() {
                                        tar::EntryType::Directory => {
                                            server.filesystem.create_dir_all(destination_path)?;
                                            if let Ok(permissions) =
                                                header.mode().map(Permissions::from_mode)
                                            {
                                                server.filesystem.set_permissions(
                                                    destination_path,
                                                    permissions,
                                                )?;
                                            }

                                            if let Ok(modified_time) = header.mtime() {
                                                directory_entries.push((
                                                    destination_path.to_path_buf(),
                                                    modified_time,
                                                ));
                                            }
                                        }
                                        tar::EntryType::Regular => {
                                            if let Some(parent) = destination_path.parent() {
                                                server.filesystem.create_dir_all(parent)?;
                                            }

                                            let mut writer =
                                                crate::server::filesystem::writer::FileSystemWriter::new(
                                                    server.clone(),
                                                    destination_path,
                                                    header.mode().map(Permissions::from_mode).ok(),
                                                    header
                                                        .mtime()
                                                        .map(|t| {
                                                            cap_std::time::SystemTime::from_std(
                                                                std::time::UNIX_EPOCH
                                                                    + std::time::Duration::from_secs(t),
                                                            )
                                                        })
                                                        .ok(),
                                                )?;

                                            crate::io::copy_shared(
                                                &mut read_buffer,
                                                &mut entry,
                                                &mut writer,
                                            )?;
                                            writer.flush()?;
                                        }
                                        tar::EntryType::Symlink => {
                                            let link = entry
                                                .link_name()
                                                .unwrap_or_default()
                                                .unwrap_or_default();

                                            if let Err(err) =
                                                server.filesystem.symlink(link, destination_path)
                                            {
                                                tracing::debug!(
                                                    path = %destination_path.display(),
                                                    "failed to create symlink from archive: {:#?}",
                                                    err
                                                );
                                            } else if let Ok(modified_time) = header.mtime() {
                                                server.filesystem.set_times(
                                                    destination_path,
                                                    std::time::UNIX_EPOCH
                                                        + std::time::Duration::from_secs(
                                                            modified_time,
                                                        ),
                                                    None,
                                                )?;
                                            }
                                        }
                                        _ => {}
                                    }
                                }

                                for (destination_path, modified_time) in directory_entries {
                                    server.filesystem.set_times(
                                        &destination_path,
                                        std::time::UNIX_EPOCH
                                            + std::time::Duration::from_secs(modified_time),
                                        None,
                                    )?;
                                }

                                let mut inner = archive.into_inner().into_inner();
                                crate::io::copy_shared(
                                    &mut read_buffer,
                                    &mut inner,
                                    &mut std::io::sink(),
                                )?;
                                archive_checksum = Some(inner.finish());
                            } else if field.name() == Some("checksum") {
                                let archive_checksum = match archive_checksum.take() {
                                    Some(checksum) => format!("{:x}", checksum),
                                    None => {
                                        return Err(anyhow::anyhow!(
                                            "archive checksum does not match multipart checksum, None to be found"
                                        ));
                                    }
                                };
                                let checksum = runtime.block_on(field.text())?;

                                if archive_checksum != checksum {
                                    return Err(anyhow::anyhow!(
                                        "archive checksum does not match multipart checksum, {checksum} != {archive_checksum}"
                                    ));
                                }
                            } else if field.name().is_some_and(|n| n.starts_with("backup-")) {
                                tracing::debug!(
                                    "processing backup field: {}",
                                    field.name().unwrap_or("unknown")
                                );

                                let backup_uuid = match field
                                    .name()
                                    .and_then(|n| n.strip_prefix("backup-"))
                                    .and_then(|n| uuid::Uuid::from_str(n).ok())
                                {
                                    Some(uuid) => uuid,
                                    None => {
                                        if field.name().is_some_and(|n| n.ends_with("checksum")) {
                                            let backup_checksum = match backup_checksum.take() {
                                                Some(checksum) => format!("{:x}", checksum),
                                                None => {
                                                    return Err(anyhow::anyhow!(
                                                        "backup checksum does not match multipart checksum, None to be found"
                                                    ));
                                                }
                                            };
                                            let checksum = runtime.block_on(field.text())?;

                                            if backup_checksum != checksum {
                                                return Err(anyhow::anyhow!(
                                                    "backup checksum does not match multipart checksum, {checksum} != {backup_checksum}"
                                                ));
                                            }

                                            continue;
                                        }

                                        tracing::warn!(
                                            "invalid backup field name: {}",
                                            field.name().unwrap_or("unknown")
                                        );
                                        continue;
                                    }
                                };

                                let file_name = match field.file_name() {
                                    Some(name) => name.to_string(),
                                    None => {
                                        tracing::warn!(
                                            "backup field without file name found in transfer archive"
                                        );
                                        continue;
                                    }
                                };

                                match field.content_type() {
                                    Some("backup/wings") => {
                                        let file_name =
                                            Path::new(&state.config.system.backup_directory)
                                                .join(file_name);
                                        let reader = tokio_util::io::StreamReader::new(
                                            field.into_stream().map_err(|err| {
                                                std::io::Error::other(format!(
                                                    "failed to read multipart field: {err}"
                                                ))
                                            }),
                                        );
                                        let reader = tokio_util::io::SyncIoBridge::new(reader);
                                        let reader = AbortReader::new(reader, listener.clone());
                                        let reader = LimitedReader::new_with_bytes_per_second(
                                            reader,
                                            state.config.system.transfers.download_limit
                                                * 1024
                                                * 1024,
                                        );
                                        let mut reader = HashReader::new_with_hasher(
                                            reader,
                                            sha2::Sha256::new(),
                                        );

                                        let mut file = match std::fs::File::create(&file_name) {
                                            Ok(file) => file,
                                            Err(err) => {
                                                tracing::error!(
                                                    "failed to create backup file {}: {:#?}",
                                                    file_name.display(),
                                                    err
                                                );
                                                continue;
                                            }
                                        };

                                        if let Err(err) = crate::io::copy(&mut reader, &mut file) {
                                            tracing::error!(
                                                "failed to copy backup file {}: {:#?}",
                                                file_name.display(),
                                                err
                                            );
                                            continue;
                                        }

                                        if let Err(err) = file.flush() {
                                            tracing::error!(
                                                "failed to flush backup file {}: {:#?}",
                                                file_name.display(),
                                                err
                                            );
                                            continue;
                                        }

                                        backups.push(backup_uuid);
                                        backup_checksum = Some(reader.finish());

                                        tracing::debug!(
                                            "backup file {} transferred successfully",
                                            file_name.display()
                                        );
                                    }
                                    _ => {
                                        tracing::warn!(
                                            "invalid content type for backup field: {:?}",
                                            field.content_type()
                                        );
                                        continue;
                                    }
                                }
                            }
                        }

                        Ok(backups)
                    },
                );

                let backups = handle.await??;

                drop(guard);
                Ok(backups)
            }
        });

        if is_multiplex {
            let (sender, receiver) = tokio::sync::oneshot::channel();
            let mut tries = 0;

            loop {
                let mut server_transfer = server.incoming_transfer.write().await;

                tries += 1;

                let incoming_transfer = match &mut *server_transfer {
                    Some(transfer) => transfer,
                    None => {
                        if tries > 10 {
                            return ApiResponse::error(
                                "unable to get incoming transfer for multiplex",
                            )
                            .with_status(StatusCode::CONFLICT)
                            .ok();
                        } else {
                            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                            continue;
                        }
                    }
                };

                incoming_transfer
                    .multiplex_handles
                    .push((handle.abort_handle(), receiver));

                break;
            }

            match handle.await {
                Ok(Ok(_)) => {
                    tracing::info!(
                        server = %server.uuid,
                        "server transfer completed successfully"
                    );
                }
                Ok(Err(err)) => {
                    tracing::error!(
                        server = %server.uuid,
                        "failed to complete server transfer: {:#?}",
                        err
                    );

                    sender.send(Err(err)).ok();

                    return ApiResponse::error("failed to complete server transfer")
                        .with_status(StatusCode::EXPECTATION_FAILED)
                        .ok();
                }
                Err(err) => {
                    tracing::error!(
                        server = %server.uuid,
                        "failed to complete server transfer: {:#?}",
                        err
                    );

                    sender.send(Err(err.into())).ok();

                    return ApiResponse::error("failed to complete server transfer")
                        .with_status(StatusCode::EXPECTATION_FAILED)
                        .ok();
                }
            }
        } else {
            server.incoming_transfer.write().await.replace(
                crate::server::transfer::IncomingServerTransfer {
                    main_handle: handle.abort_handle(),
                    multiplex_handles: vec![],
                },
            );

            if multiplex_stream_count > 0 {
                let mut tries = 0;

                loop {
                    let mut server_transfer = server.incoming_transfer.write().await;

                    tries += 1;

                    let incoming_transfer = match &mut *server_transfer {
                        Some(transfer) => transfer,
                        None => {
                            return ApiResponse::error(
                                "unable to get incoming transfer for multiplex",
                            )
                            .with_status(StatusCode::CONFLICT)
                            .ok();
                        }
                    };

                    if incoming_transfer.multiplex_handles.len() != multiplex_stream_count {
                        if tries > 10 {
                            return ApiResponse::error(
                                "unable to get incoming transfer for multiplex join",
                            )
                            .with_status(StatusCode::CONFLICT)
                            .ok();
                        } else {
                            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                            continue;
                        }
                    }

                    match incoming_transfer.try_join_handles(handle).await {
                        Ok(backups) => {
                            tracing::info!(
                                server = %server.uuid,
                                "server transfer completed successfully"
                            );

                            state
                                .config
                                .client
                                .set_server_transfer(subject, true, backups)
                                .await?;
                            server
                                .transferring
                                .store(false, std::sync::atomic::Ordering::SeqCst);
                            server
                                .websocket
                                .send(crate::server::websocket::WebsocketMessage::new(
                                    crate::server::websocket::WebsocketEvent::ServerTransferStatus,
                                    ["completed".to_string()].into(),
                                ))
                                .ok();
                        }
                        Err(err) => {
                            tracing::error!(
                                server = %server.uuid,
                                "failed to complete server transfer: {:#?}",
                                err
                            );

                            state
                                .config
                                .client
                                .set_server_transfer(subject, false, vec![])
                                .await
                                .unwrap_or_default();

                            return ApiResponse::error("failed to complete server transfer")
                                .with_status(StatusCode::EXPECTATION_FAILED)
                                .ok();
                        }
                    }

                    break;
                }
            }
        }

        ApiResponse::json(Response {}).ok()
    }
}

pub fn router(state: &State) -> OpenApiRouter<State> {
    OpenApiRouter::new()
        .routes(routes!(post::route).layer(DefaultBodyLimit::disable()))
        .nest("/{server}", _server_::router(state))
        .with_state(state.clone())
}
