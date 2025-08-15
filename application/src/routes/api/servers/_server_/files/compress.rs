use super::State;
use utoipa_axum::{router::OpenApiRouter, routes};

mod post {
    use crate::{
        response::{ApiResponse, ApiResponseResult},
        routes::{ApiError, GetState, api::servers::_server_::GetServer},
        server::filesystem::archive::CompressionType,
    };
    use axum::http::StatusCode;
    use serde::Deserialize;
    use std::path::PathBuf;
    use utoipa::ToSchema;

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
        pub format: ArchiveFormat,
        pub name: Option<String>,

        #[serde(default)]
        pub root: String,
        pub files: Vec<String>,
    }

    #[utoipa::path(post, path = "/", responses(
        (status = OK, body = crate::models::DirectoryEntry),
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

        match tokio::spawn({
            let root = root.clone();
            let server = server.0.clone();
            let file_name = file_name.clone();

            async move {
                let ignored = server.filesystem.get_ignored().await;

                match data.format {
                    ArchiveFormat::Tar
                    | ArchiveFormat::TarGz
                    | ArchiveFormat::TarXz
                    | ArchiveFormat::TarBz2
                    | ArchiveFormat::TarLz4
                    | ArchiveFormat::TarZstd => {
                        let writer = crate::server::filesystem::writer::AsyncFileSystemWriter::new(
                            server.clone(),
                            &file_name,
                            None,
                            None,
                        )
                        .await?;

                        crate::server::filesystem::archive::Archive::create_tar(
                            server.filesystem.clone(),
                            writer,
                            &root,
                            data.files.into_iter().map(PathBuf::from).collect(),
                            match data.format {
                                ArchiveFormat::Tar => CompressionType::None,
                                ArchiveFormat::TarGz => CompressionType::Gz,
                                ArchiveFormat::TarXz => CompressionType::Xz,
                                ArchiveFormat::TarBz2 => CompressionType::Bz2,
                                ArchiveFormat::TarLz4 => CompressionType::Lz4,
                                ArchiveFormat::TarZstd => CompressionType::Zstd,
                                _ => unreachable!(),
                            },
                            state.config.system.backups.compression_level,
                            None,
                            &[ignored],
                        )
                        .await
                    }
                    ArchiveFormat::Zip => {
                        let writer = tokio::task::spawn_blocking({
                            let server = server.clone();

                            move || {
                                crate::server::filesystem::writer::FileSystemWriter::new(
                                    server, &file_name, None, None,
                                )
                            }
                        })
                        .await??;

                        crate::server::filesystem::archive::Archive::create_zip(
                            server.filesystem.clone(),
                            writer,
                            &root,
                            data.files.into_iter().map(PathBuf::from).collect(),
                            state.config.system.backups.compression_level,
                            None,
                            vec![ignored],
                        )
                        .await
                    }
                    ArchiveFormat::SevenZip => {
                        let writer = tokio::task::spawn_blocking({
                            let server = server.clone();

                            move || {
                                crate::server::filesystem::writer::FileSystemWriter::new(
                                    server, &file_name, None, None,
                                )
                            }
                        })
                        .await??;

                        crate::server::filesystem::archive::Archive::create_7z(
                            server.filesystem.clone(),
                            writer,
                            &root,
                            data.files.into_iter().map(PathBuf::from).collect(),
                            None,
                            vec![ignored],
                        )
                        .await
                    }
                }
            }
        })
        .await
        {
            Ok(Ok(())) => {}
            Ok(Err(err)) => {
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
    }
}

pub fn router(state: &State) -> OpenApiRouter<State> {
    OpenApiRouter::new()
        .routes(routes!(post::route))
        .with_state(state.clone())
}
