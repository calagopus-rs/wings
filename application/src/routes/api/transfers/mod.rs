use super::State;
use axum::extract::DefaultBodyLimit;
use utoipa_axum::{
    router::{OpenApiRouter, UtoipaMethodRouterExt},
    routes,
};

mod _server_;

mod post {
    use crate::{
        routes::{ApiError, GetState},
        server::transfer::ArchiveFormat,
    };
    use axum::{
        extract::Multipart,
        http::{HeaderMap, StatusCode},
    };
    use futures::{StreamExt, TryStreamExt};
    use serde::Serialize;
    use std::{fs::Permissions, os::unix::fs::PermissionsExt, path::Path, str::FromStr};
    use tokio::io::AsyncWriteExt;
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
    ) -> (StatusCode, axum::Json<serde_json::Value>) {
        let key = headers
            .get("Authorization")
            .map(|v| v.to_str().unwrap())
            .unwrap_or("")
            .to_string();
        let mut parts = key.splitn(2, " ");
        let r#type = parts.next().unwrap();
        let token = parts.next();

        if r#type != "Bearer" || token.is_none() {
            return (
                StatusCode::UNAUTHORIZED,
                axum::Json(ApiError::new("invalid authorization token").to_json()),
            );
        }

        let payload: crate::remote::jwt::BasePayload = match state.config.jwt.verify(token.unwrap())
        {
            Ok(payload) => payload,
            Err(_) => {
                return (
                    StatusCode::UNAUTHORIZED,
                    axum::Json(ApiError::new("invalid token").to_json()),
                );
            }
        };

        if !payload.validate(&state.config.jwt) {
            return (
                StatusCode::UNAUTHORIZED,
                axum::Json(ApiError::new("invalid token").to_json()),
            );
        }

        let subject: uuid::Uuid = match payload.subject {
            Some(subject) => match subject.parse() {
                Ok(subject) => subject,
                Err(_) => {
                    return (
                        StatusCode::UNAUTHORIZED,
                        axum::Json(ApiError::new("invalid token").to_json()),
                    );
                }
            },
            None => {
                return (
                    StatusCode::UNAUTHORIZED,
                    axum::Json(ApiError::new("invalid token").to_json()),
                );
            }
        };

        if state
            .server_manager
            .get_servers()
            .await
            .iter()
            .any(|s| s.uuid == subject)
        {
            return (
                StatusCode::CONFLICT,
                axum::Json(
                    serde_json::to_value(ApiError::new("server with this uuid already exists"))
                        .unwrap(),
                ),
            );
        }

        let server_data = state.config.client.server(subject).await.unwrap();
        let server = state.server_manager.create_server(server_data, false).await;

        server
            .transferring
            .store(true, std::sync::atomic::Ordering::SeqCst);

        let handle: tokio::task::JoinHandle<Result<(), anyhow::Error>> = tokio::spawn({
            let server = server.clone();

            async move {
                while let Ok(Some(field)) = multipart.next_field().await {
                    if let Some("archive") = field.name() {
                        let file_name = field.file_name().unwrap_or("archive.tar.gz").to_string();
                        let reader =
                            tokio_util::io::StreamReader::new(field.into_stream().map_err(|err| {
                                std::io::Error::other(format!(
                                    "failed to read multipart field: {err}"
                                ))
                            }));
                        let reader: Box<dyn tokio::io::AsyncRead + Send> =
                            match ArchiveFormat::from_str(&file_name)
                                .unwrap_or(ArchiveFormat::TarGz)
                            {
                                ArchiveFormat::Tar => Box::new(reader),
                                ArchiveFormat::TarGz => Box::new(
                                    async_compression::tokio::bufread::GzipDecoder::new(reader),
                                ),
                                ArchiveFormat::TarZstd => Box::new(
                                    async_compression::tokio::bufread::ZstdDecoder::new(reader),
                                ),
                            };

                        let mut archive = tokio_tar::Archive::new(Box::into_pin(reader));
                        let mut entries = archive.entries()?;

                        while let Some(Ok(mut entry)) = entries.next().await {
                            let path = entry.path()?;

                            if path.is_absolute() {
                                continue;
                            }

                            let destination_path = path.as_ref();
                            let header = entry.header();

                            match header.entry_type() {
                                tokio_tar::EntryType::Directory => {
                                    server.filesystem.create_dir_all(&destination_path).await?;
                                    if let Ok(permissions) =
                                        header.mode().map(Permissions::from_mode)
                                    {
                                        server
                                            .filesystem
                                            .set_permissions(
                                                &destination_path,
                                                cap_std::fs::Permissions::from_std(permissions),
                                            )
                                            .await?;
                                    }
                                }
                                tokio_tar::EntryType::Regular => {
                                    if let Some(parent) = destination_path.parent() {
                                        server.filesystem.create_dir_all(parent).await?;
                                    }

                                    let mut writer =
                                    crate::server::filesystem::writer::AsyncFileSystemWriter::new(
                                        server.clone(),
                                        destination_path.to_path_buf(),
                                        header.mode().map(Permissions::from_mode).ok(),
                                        header
                                            .mtime()
                                            .map(|t| {
                                                std::time::UNIX_EPOCH
                                                    + std::time::Duration::from_secs(t)
                                            })
                                            .ok(),
                                    )
                                    .await?;

                                    tokio::io::copy(&mut entry, &mut writer).await?;
                                    writer.flush().await?;
                                }
                                tokio_tar::EntryType::Symlink => {
                                    let link =
                                        entry.link_name().unwrap_or_default().unwrap_or_default();

                                    server
                                        .filesystem
                                        .symlink(link.as_ref(), destination_path)
                                        .await
                                        .unwrap_or_else(|err| {
                                            tracing::debug!(
                                                "failed to create symlink from archive: {:#?}",
                                                err
                                            );
                                        });
                                }
                                _ => {}
                            }
                        }
                    } else if field.name().is_some_and(|n| n.starts_with("backup-")) {
                        tracing::debug!(
                            "processing backup field: {}",
                            field.name().unwrap_or("unknown")
                        );

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
                                let file_name = Path::new(&state.config.system.backup_directory)
                                    .join(file_name);
                                let mut reader = tokio_util::io::StreamReader::new(
                                    field.into_stream().map_err(|err| {
                                        std::io::Error::other(format!(
                                            "failed to read multipart field: {err}"
                                        ))
                                    }),
                                );

                                let mut file = match tokio::fs::File::create(&file_name).await {
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

                                if let Err(err) = tokio::io::copy(&mut reader, &mut file).await {
                                    tracing::error!(
                                        "failed to copy backup file {}: {:#?}",
                                        file_name.display(),
                                        err
                                    );
                                    continue;
                                }

                                if let Err(err) = file.flush().await {
                                    tracing::error!(
                                        "failed to flush backup file {}: {:#?}",
                                        file_name.display(),
                                        err
                                    );
                                    continue;
                                }

                                tracing::debug!(
                                    "backup file {} transferred successfully",
                                    file_name.display()
                                );
                            }
                            _ => {
                                tracing::warn!(
                                    "invalid content type for backup field: {}",
                                    field.content_type().unwrap_or("unknown")
                                );
                                continue;
                            }
                        }
                    }
                }

                state
                    .config
                    .client
                    .set_server_transfer(subject, true)
                    .await?;
                server
                    .transferring
                    .store(false, std::sync::atomic::Ordering::SeqCst);
                server
                    .websocket
                    .send(crate::server::websocket::WebsocketMessage::new(
                        crate::server::websocket::WebsocketEvent::ServerTransferStatus,
                        &["completed".to_string()],
                    ))
                    .ok();

                Ok(())
            }
        });

        server
            .incoming_transfer
            .write()
            .await
            .replace(handle.abort_handle());
        if let Ok(Err(err)) = handle.await {
            tracing::error!(
                server = %server.uuid,
                "failed to complete server transfer: {:#?}",
                err
            );

            return (
                StatusCode::EXPECTATION_FAILED,
                axum::Json(ApiError::new("failed to complete server transfer").to_json()),
            );
        }

        (
            StatusCode::OK,
            axum::Json(serde_json::to_value(Response {}).unwrap()),
        )
    }
}

pub fn router(state: &State) -> OpenApiRouter<State> {
    OpenApiRouter::new()
        .routes(routes!(post::route).layer(DefaultBodyLimit::disable()))
        .nest("/{server}", _server_::router(state))
        .with_state(state.clone())
}
