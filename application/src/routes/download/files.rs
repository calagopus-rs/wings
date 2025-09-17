use super::State;
use utoipa_axum::{router::OpenApiRouter, routes};

mod get {
    use crate::{
        response::{ApiResponse, ApiResponseResult},
        routes::GetState,
        server::filesystem::archive::StreamableArchiveFormat,
    };
    use axum::{
        body::Body,
        extract::Query,
        http::{HeaderMap, StatusCode},
    };
    use serde::Deserialize;
    use std::path::PathBuf;
    use utoipa::ToSchema;

    #[derive(ToSchema, Deserialize)]
    pub struct Params {
        token: String,

        #[serde(default)]
        archive_format: StreamableArchiveFormat,
    }

    #[derive(Deserialize)]
    pub struct FilesJwtPayload {
        #[serde(flatten)]
        pub base: crate::remote::jwt::BasePayload,

        pub file_path: String,
        pub file_paths: Vec<String>,
        pub server_uuid: uuid::Uuid,
        pub unique_id: String,
    }

    #[utoipa::path(get, path = "/", responses(
        (status = OK, body = String),
        (status = UNAUTHORIZED, body = String),
        (status = NOT_FOUND, body = String),
        (status = EXPECTATION_FAILED, body = String),
    ), params(
        (
            "token" = String, Query,
            description = "The JWT token to use for authentication",
        ),
    ))]
    pub async fn route(state: GetState, Query(data): Query<Params>) -> ApiResponseResult {
        let payload: FilesJwtPayload = match state.config.jwt.verify(&data.token) {
            Ok(payload) => payload,
            Err(_) => {
                return ApiResponse::error("invalid token")
                    .with_status(StatusCode::UNAUTHORIZED)
                    .ok();
            }
        };

        if !payload.base.validate(&state.config.jwt).await {
            return ApiResponse::error("invalid token")
                .with_status(StatusCode::UNAUTHORIZED)
                .ok();
        }

        if !state.config.jwt.one_time_id(&payload.unique_id).await {
            return ApiResponse::error("token has already been used")
                .with_status(StatusCode::UNAUTHORIZED)
                .ok();
        }

        let server = state
            .server_manager
            .get_servers()
            .await
            .iter()
            .find(|s| s.uuid == payload.server_uuid)
            .cloned();

        let server = match server {
            Some(server) => server,
            None => {
                return ApiResponse::error("server not found")
                    .with_status(StatusCode::NOT_FOUND)
                    .ok();
            }
        };

        let path = PathBuf::from(payload.file_path);

        let mut folder_ascii = String::new();
        for (i, file_path) in payload.file_paths.iter().enumerate() {
            let file_name = PathBuf::from(file_path)
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();

            for c in file_name.chars() {
                if c.is_ascii() {
                    folder_ascii.push(c);
                } else {
                    folder_ascii.push('_');
                }
            }

            if i < payload.file_paths.len() - 1 {
                folder_ascii.push('_');
            }
        }

        folder_ascii.push('.');
        folder_ascii.push_str(data.archive_format.extension());

        let mut headers = HeaderMap::new();
        headers.insert(
            "Content-Disposition",
            format!(
                "attachment; filename={}",
                serde_json::Value::String(folder_ascii)
            )
            .parse()
            .unwrap(),
        );
        headers.insert(
            "Content-Type",
            data.archive_format.mime_type().parse().unwrap(),
        );

        if let Some((backup, path)) = server
            .filesystem
            .backup_fs(&server, &state.backup_manager, &path)
            .await
        {
            match backup
                .read_files_archive(
                    path.clone(),
                    payload.file_paths.into_iter().map(PathBuf::from).collect(),
                    data.archive_format,
                )
                .await
            {
                Ok(reader) => {
                    return ApiResponse::new(Body::from_stream(
                        tokio_util::io::ReaderStream::with_capacity(reader, crate::BUFFER_SIZE),
                    ))
                    .with_headers(headers)
                    .ok();
                }
                Err(err) => {
                    tracing::error!(
                        server = %server.uuid,
                        path = %path.display(),
                        error = %err,
                        "failed to get backup directory contents",
                    );

                    return ApiResponse::error("failed to retrieve backup folder contents")
                        .with_status(StatusCode::EXPECTATION_FAILED)
                        .ok();
                }
            }
        }

        let metadata = server.filesystem.async_symlink_metadata(&path).await;
        if let Ok(metadata) = metadata {
            if !metadata.is_dir() || server.filesystem.is_ignored(&path, metadata.is_dir()).await {
                return ApiResponse::error("directory not found")
                    .with_status(StatusCode::NOT_FOUND)
                    .ok();
            }
        } else {
            return ApiResponse::error("directory not found")
                .with_status(StatusCode::NOT_FOUND)
                .ok();
        }

        let (reader, writer) = tokio::io::duplex(crate::BUFFER_SIZE);

        tokio::spawn(async move {
            let ignored = server.filesystem.get_ignored().await;
            let writer = tokio_util::io::SyncIoBridge::new(writer);

            match data.archive_format {
                StreamableArchiveFormat::Zip => {
                    if let Err(err) =
                        crate::server::filesystem::archive::create::create_zip_streaming(
                            server.filesystem.clone(),
                            writer,
                            &path,
                            payload.file_paths.into_iter().map(PathBuf::from).collect(),
                            None,
                            vec![ignored],
                            crate::server::filesystem::archive::create::CreateZipOptions {
                                compression_level: state.config.system.backups.compression_level,
                            },
                        )
                        .await
                    {
                        tracing::error!(
                            server = %server.uuid,
                            "failed to create zip archive: {:#?}",
                            err
                        );
                    }
                }
                _ => {
                    if let Err(err) = crate::server::filesystem::archive::create::create_tar(
                        server.filesystem.clone(),
                        writer,
                        &path,
                        payload.file_paths.into_iter().map(PathBuf::from).collect(),
                        None,
                        vec![ignored],
                        crate::server::filesystem::archive::create::CreateTarOptions {
                            compression_type: data.archive_format.compression_format(),
                            compression_level: state.config.system.backups.compression_level,
                            threads: state.config.api.file_compression_threads,
                        },
                    )
                    .await
                    {
                        tracing::error!(
                            server = %server.uuid,
                            "failed to create tar archive: {:#?}",
                            err
                        );
                    }
                }
            }
        });

        ApiResponse::new(Body::from_stream(
            tokio_util::io::ReaderStream::with_capacity(reader, crate::BUFFER_SIZE),
        ))
        .with_headers(headers)
        .ok()
    }
}

pub fn router(state: &State) -> OpenApiRouter<State> {
    OpenApiRouter::new()
        .routes(routes!(get::route))
        .with_state(state.clone())
}
