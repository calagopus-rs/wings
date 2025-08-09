use super::State;
use utoipa_axum::{router::OpenApiRouter, routes};

mod get {
    use crate::{
        response::{ApiResponse, ApiResponseResult},
        routes::{ApiError, GetState, api::servers::_server_::GetServer},
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
        file: String,

        #[schema(default = "false")]
        #[serde(default)]
        download: bool,
        max_size: Option<u64>,
    }

    #[utoipa::path(get, path = "/", responses(
        (status = OK, body = String),
        (status = NOT_FOUND, body = ApiError),
        (status = PAYLOAD_TOO_LARGE, body = ApiError),
        (status = EXPECTATION_FAILED, body = ApiError),
    ), params(
        (
            "server" = uuid::Uuid,
            description = "The server uuid",
            example = "123e4567-e89b-12d3-a456-426614174000",
        ),
        (
            "file" = String, Query,
            description = "The file to view contents of",
        ),
        (
            "download" = bool, Query,
            description = "Whether to add 'download headers' to the file",
        ),
        (
            "max_size" = Option<u64>, Query,
            description = "The maximum size of the file to return. If the file is larger than this, an error will be returned.",
        ),
    ))]
    pub async fn route(
        state: GetState,
        server: GetServer,
        Query(data): Query<Params>,
    ) -> ApiResponseResult {
        let path = match server.filesystem.async_canonicalize(&data.file).await {
            Ok(path) => path,
            Err(_) => PathBuf::from(data.file),
        };

        let file_name = match path.file_name() {
            Some(name) => name.to_string_lossy().to_string(),
            None => {
                return ApiResponse::error("file not found")
                    .with_status(StatusCode::NOT_FOUND)
                    .ok();
            }
        };

        if let Some((backup, path)) = server
            .filesystem
            .backup_fs(&server, &state.backup_manager, &path)
            .await
        {
            match backup.read_file(path.clone()).await {
                Ok((size, reader)) => {
                    let mut headers = HeaderMap::new();

                    if let Some(max_size) = data.max_size
                        && size > max_size
                    {
                        return ApiResponse::error("file size exceeds maximum allowed size")
                            .with_status(StatusCode::PAYLOAD_TOO_LARGE)
                            .ok();
                    }

                    headers.insert("Content-Length", size.into());
                    if data.download {
                        headers.insert(
                            "Content-Disposition",
                            format!(
                                "attachment; filename={}",
                                serde_json::Value::String(file_name)
                            )
                            .parse()?,
                        );
                        headers.insert("Content-Type", "application/octet-stream".parse()?);
                    }

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
                        "failed to get backup file contents",
                    );

                    return ApiResponse::error("failed to get backup file contents")
                        .with_status(StatusCode::EXPECTATION_FAILED)
                        .ok();
                }
            }
        }

        let metadata = match server.filesystem.async_metadata(&path).await {
            Ok(metadata) => {
                if !metadata.is_file()
                    || server.filesystem.is_ignored(&path, metadata.is_dir()).await
                {
                    return ApiResponse::error("file not found")
                        .with_status(StatusCode::NOT_FOUND)
                        .ok();
                }

                metadata
            }
            Err(_) => {
                return ApiResponse::error("file not found")
                    .with_status(StatusCode::NOT_FOUND)
                    .ok();
            }
        };

        if let Some(max_size) = data.max_size
            && metadata.len() > max_size
        {
            return ApiResponse::error("file size exceeds maximum allowed size")
                .with_status(StatusCode::PAYLOAD_TOO_LARGE)
                .ok();
        }

        let mut file =
            match crate::server::filesystem::archive::Archive::open(server.0.clone(), path.clone())
                .await
            {
                Some(file) => file,
                None => {
                    return ApiResponse::error("file not found")
                        .with_status(StatusCode::NOT_FOUND)
                        .ok();
                }
            };

        let size = match file.estimated_size().await {
            Some(size) => size,
            None => {
                return ApiResponse::error("unable to retrieve estimated file size")
                    .with_status(StatusCode::EXPECTATION_FAILED)
                    .ok();
            }
        };

        let reader = match file.reader().await {
            Ok(reader) => reader,
            Err(err) => {
                tracing::error!(
                    server = %server.uuid,
                    path = %path.display(),
                    "failed to open file for reading: {:#?}",
                    err,
                );

                return ApiResponse::error("unable to open file for reading")
                    .with_status(StatusCode::EXPECTATION_FAILED)
                    .ok();
            }
        };

        let mut headers = HeaderMap::new();

        headers.insert("Content-Length", size.into());
        if data.download {
            headers.insert(
                "Content-Disposition",
                format!(
                    "attachment; filename={}",
                    serde_json::Value::String(file_name)
                )
                .parse()?,
            );
            headers.insert("Content-Type", "application/octet-stream".parse()?);
        }

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
