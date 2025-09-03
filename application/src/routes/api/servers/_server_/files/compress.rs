use super::State;
use utoipa_axum::{router::OpenApiRouter, routes};

mod post {
    use crate::{
        io::compression::CompressionType,
        response::{ApiResponse, ApiResponseResult},
        routes::{ApiError, GetState, api::servers::_server_::GetServer},
    };
    use axum::http::StatusCode;
    use serde::{Deserialize, Serialize};
    use std::{
        path::{Path, PathBuf},
        sync::{Arc, atomic::AtomicU64},
    };
    use utoipa::ToSchema;

    fn foreground() -> bool {
        true
    }

    #[derive(ToSchema, Deserialize, Default, Clone, Copy)]
    #[serde(rename_all = "snake_case")]
    #[schema(rename_all = "snake_case")]
    pub enum ArchiveFormat {
        Tar,
        #[default]
        TarGz,
        TarXz,
        TarBz2,
        TarLz4,
        TarZstd,
        Zip,
        SevenZip,
    }

    #[derive(ToSchema, Deserialize)]
    pub struct Payload {
        #[serde(default)]
        format: ArchiveFormat,
        name: Option<String>,

        #[serde(default)]
        root: String,
        files: Vec<String>,

        #[serde(default = "foreground")]
        foreground: bool,
    }

    #[derive(ToSchema, Serialize)]
    pub struct Response {
        identifier: uuid::Uuid,
    }

    #[utoipa::path(post, path = "/", responses(
        (status = OK, body = crate::models::DirectoryEntry),
        (status = ACCEPTED, body = inline(Response)),
        (status = NOT_FOUND, body = ApiError),
        (status = EXPECTATION_FAILED, body = ApiError),
    ), params(
        (
            "server" = uuid::Uuid,
            description = "The server uuid",
            example = "123e4567-e89b-12d3-a456-426614174000",
        ),
    ), request_body = inline(Payload))]
    pub async fn route(
        state: GetState,
        server: GetServer,
        axum::Json(data): axum::Json<Payload>,
    ) -> ApiResponseResult {
        let root = match server.filesystem.async_canonicalize(data.root).await {
            Ok(path) => path,
            Err(_) => {
                return ApiResponse::error("root not found")
                    .with_status(StatusCode::NOT_FOUND)
                    .ok();
            }
        };

        let metadata = server.filesystem.async_symlink_metadata(&root).await;
        if !metadata.map(|m| m.is_dir()).unwrap_or(true) {
            return ApiResponse::error("root is not a directory")
                .with_status(StatusCode::EXPECTATION_FAILED)
                .ok();
        }

        let file_name = data.name.unwrap_or_else(|| {
            format!(
                "archive-{}.{}",
                chrono::Local::now().format("%Y-%m-%dT%H%M%S%z"),
                match data.format {
                    ArchiveFormat::Tar => "tar",
                    ArchiveFormat::TarGz => "tar.gz",
                    ArchiveFormat::TarXz => "tar.xz",
                    ArchiveFormat::TarBz2 => "tar.bz2",
                    ArchiveFormat::TarLz4 => "tar.lz4",
                    ArchiveFormat::TarZstd => "tar.zst",
                    ArchiveFormat::Zip => "zip",
                    ArchiveFormat::SevenZip => "7z",
                }
            )
        });
        let file_name = root.join(file_name);

        if server.filesystem.is_ignored(&file_name, false).await {
            return ApiResponse::error("file not found")
                .with_status(StatusCode::EXPECTATION_FAILED)
                .ok();
        }

        let progress = Arc::new(AtomicU64::new(0));
        let total = Arc::new(AtomicU64::new(0));

        let (identifier, task) = server
            .filesystem
            .operations
            .add_operation(
                crate::server::filesystem::operations::FilesystemOperation::Compress {
                    path: file_name.clone(),
                    progress: progress.clone(),
                    total: total.clone(),
                },
                {
                    let root = root.clone();
                    let server = server.0.clone();
                    let file_name = file_name.clone();

                    async move {
                        let ignored = server.filesystem.get_ignored().await;
                        let writer = tokio::task::spawn_blocking({
                            let server = server.clone();

                            move || {
                                crate::server::filesystem::writer::FileSystemWriter::new(
                                    server, &file_name, None, None,
                                )
                            }
                        })
                        .await??;

                        let mut total_size = 0;
                        for file in &data.files {
                            if let Ok(metadata) = server.filesystem.async_metadata(file).await {
                                if metadata.is_dir() {
                                    total_size += server
                                        .filesystem
                                        .disk_usage
                                        .read()
                                        .await
                                        .get_size(Path::new(file))
                                        .unwrap_or(0);
                                } else {
                                    total_size += metadata.len();
                                }
                            }
                        }

                        total.store(total_size, std::sync::atomic::Ordering::Relaxed);

                        match data.format {
                            ArchiveFormat::Tar
                            | ArchiveFormat::TarGz
                            | ArchiveFormat::TarXz
                            | ArchiveFormat::TarBz2
                            | ArchiveFormat::TarLz4
                            | ArchiveFormat::TarZstd => {
                                crate::server::filesystem::archive::create::create_tar(
                                    server.filesystem.clone(),
                                    writer,
                                    &root,
                                    data.files.into_iter().map(PathBuf::from).collect(),
                                    Some(progress),
                                    vec![ignored],
                                    crate::server::filesystem::archive::create::CreateTarOptions {
                                        compression_type: match data.format {
                                            ArchiveFormat::Tar => CompressionType::None,
                                            ArchiveFormat::TarGz => CompressionType::Gz,
                                            ArchiveFormat::TarXz => CompressionType::Xz,
                                            ArchiveFormat::TarBz2 => CompressionType::Bz2,
                                            ArchiveFormat::TarLz4 => CompressionType::Lz4,
                                            ArchiveFormat::TarZstd => CompressionType::Zstd,
                                            _ => unreachable!(),
                                        },
                                        compression_level: state
                                            .config
                                            .system
                                            .backups
                                            .compression_level,
                                        threads: state.config.api.file_compression_threads,
                                    },
                                )
                                .await
                            }
                            ArchiveFormat::Zip => {
                                crate::server::filesystem::archive::create::create_zip(
                                    server.filesystem.clone(),
                                    writer,
                                    &root,
                                    data.files.into_iter().map(PathBuf::from).collect(),
                                    Some(progress),
                                    vec![ignored],
                                    crate::server::filesystem::archive::create::CreateZipOptions {
                                        compression_level: state
                                            .config
                                            .system
                                            .backups
                                            .compression_level,
                                    },
                                )
                                .await
                            }
                            ArchiveFormat::SevenZip => {
                                crate::server::filesystem::archive::create::create_7z(
                                    server.filesystem.clone(),
                                    writer,
                                    &root,
                                    data.files.into_iter().map(PathBuf::from).collect(),
                                    Some(progress),
                                    vec![ignored],
                                    crate::server::filesystem::archive::create::Create7zOptions {},
                                )
                                .await
                            }
                        }
                    }
                },
            )
            .await;

        if data.foreground {
            match task.await {
                Ok(Some(Ok(()))) => {}
                Ok(None) => {
                    return ApiResponse::error("archive compression aborted by another source")
                        .with_status(StatusCode::EXPECTATION_FAILED)
                        .ok();
                }
                Ok(Some(Err(err))) => {
                    tracing::error!(
                        server = %server.uuid,
                        root = %root.display(),
                        "failed to compress files: {:#?}",
                        err,
                    );

                    return ApiResponse::error(&format!("failed to compress files: {err}"))
                        .with_status(StatusCode::EXPECTATION_FAILED)
                        .ok();
                }
                Err(err) => {
                    tracing::error!(
                        server = %server.uuid,
                        root = %root.display(),
                        "failed to compress files: {:#?}",
                        err,
                    );

                    return ApiResponse::error("failed to compress files")
                        .with_status(StatusCode::EXPECTATION_FAILED)
                        .ok();
                }
            }

            let metadata = server.filesystem.async_symlink_metadata(&file_name).await?;

            ApiResponse::json(server.filesystem.to_api_entry(file_name, metadata).await).ok()
        } else {
            ApiResponse::json(Response { identifier })
                .with_status(StatusCode::ACCEPTED)
                .ok()
        }
    }
}

pub fn router(state: &State) -> OpenApiRouter<State> {
    OpenApiRouter::new()
        .routes(routes!(post::route))
        .with_state(state.clone())
}
