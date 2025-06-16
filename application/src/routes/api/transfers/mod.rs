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
    use futures::TryStreamExt;
    use serde::Serialize;
    use std::{fs::Permissions, io::Write, os::unix::fs::PermissionsExt, str::FromStr};
    use tokio_util::io::SyncIoBridge;
    use utoipa::ToSchema;

    #[derive(ToSchema, Serialize)]
    struct Response {}

    #[utoipa::path(post, path = "/", responses(
        (status = OK, body = inline(Response)),
        (status = UNAUTHORIZED, body = inline(ApiError)),
        (status = CONFLICT, body = inline(ApiError)),
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

        let filesystem = server.filesystem.base_dir().await.unwrap();

        let runtime = tokio::runtime::Handle::current();
        server
            .clone()
            .incoming_transfer
            .write()
            .await
            .replace(tokio::task::spawn_blocking(move || {
                while let Ok(Some(field)) = runtime.block_on(multipart.next_field()) {
                    if let Some("archive") = field.name() {
                        let file_name = field.file_name().unwrap_or("archive.tar.gz").to_string();
                        let sync_reader = SyncIoBridge::new(tokio_util::io::StreamReader::new(
                            field.into_stream().map_err(|err| {
                                std::io::Error::other(format!(
                                    "failed to read multipart field: {}",
                                    err
                                ))
                            }),
                        ));
                        let reader: Box<dyn std::io::Read> =
                            match ArchiveFormat::from_str(&file_name).unwrap_or(ArchiveFormat::TarGz) {
                                ArchiveFormat::Tar => Box::new(sync_reader),
                                ArchiveFormat::TarGz => {
                                    Box::new(flate2::read::GzDecoder::new(sync_reader))
                                }
                                ArchiveFormat::TarZstd => {
                                    Box::new(zstd::Decoder::new(sync_reader).unwrap())
                                }
                            };
                        let mut archive = tar::Archive::new(reader);

                        for entry in archive.entries().unwrap() {
                            let mut entry = entry.unwrap();
                            let path = entry.path().unwrap();

                            if path.is_absolute() {
                                continue;
                            }

                            let header = entry.header();
                            match header.entry_type() {
                                tar::EntryType::Directory => {
                                    filesystem.create_dir_all(&path).unwrap();
                                }
                                tar::EntryType::Regular => {
                                    filesystem.create_dir_all(path.parent().unwrap()).unwrap();

                                    let mut writer =
                                        crate::server::filesystem::writer::FileSystemWriter::new(
                                            server.clone(),
                                            path.to_path_buf(),
                                            header.mode().map(Permissions::from_mode).ok(),
                                            header
                                                .mtime()
                                                .map(|t| {
                                                    std::time::UNIX_EPOCH
                                                        + std::time::Duration::from_secs(t)
                                                })
                                                .ok(),
                                        )
                                        .unwrap();

                                    if let Err(err) = std::io::copy(&mut entry, &mut writer) {
                                        tracing::error!(
                                            "failed to copy file from transfer archive: {:#?}",
                                            err
                                        );
                                    }
                                    if let Err(err) = writer.flush() {
                                        tracing::error!(
                                            "failed to flush file from transfer archive: {:#?}",
                                            err
                                        );
                                    }
                                }
                                tar::EntryType::Symlink => {
                                    let link = match entry.link_name() {
                                        Ok(link) => link.unwrap_or_default(),
                                        Err(err) => {
                                            tracing::error!(
                                                "failed to read symlink from transfer archive: {:#?}",
                                                err
                                            );
                                            continue;
                                        }
                                    };

                                    if let Err(err) = filesystem.symlink(
                                        link,
                                        &path,
                                    ) {
                                        tracing::error!(
                                            "failed to create symlink from transfer archive: {:#?}",
                                            err
                                        );
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                }

                futures::executor::block_on(state.config.client.set_server_transfer(subject, true))
                    .ok();
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
            }));

        (
            StatusCode::OK,
            axum::Json(serde_json::to_value(&Response {}).unwrap()),
        )
    }
}

pub fn router(state: &State) -> OpenApiRouter<State> {
    OpenApiRouter::new()
        .routes(routes!(post::route).layer(DefaultBodyLimit::disable()))
        .nest("/{server}", _server_::router(state))
        .with_state(state.clone())
}
