use super::State;
use utoipa_axum::{router::OpenApiRouter, routes};

mod get {
    use crate::{
        response::{ApiResponse, ApiResponseResult},
        routes::GetState,
    };
    use axum::{
        body::Body,
        extract::Query,
        http::{HeaderMap, StatusCode},
    };
    use serde::Deserialize;
    use std::path::Path;
    use utoipa::ToSchema;

    #[derive(ToSchema, Deserialize)]
    pub struct Params {
        token: String,
    }

    #[derive(Deserialize)]
    pub struct FileJwtPayload {
        #[serde(flatten)]
        pub base: crate::remote::jwt::BasePayload,

        pub file_path: String,
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
        let payload: FileJwtPayload = match state.config.jwt.verify(&data.token) {
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

        let path = Path::new(&payload.file_path);

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
            .backup_fs(&server, &state.backup_manager, path)
            .await
        {
            match backup.read_file(path.clone()).await {
                Ok((size, reader)) => {
                    let mut headers = HeaderMap::new();

                    headers.insert("Content-Length", size.into());
                    headers.insert(
                        "Content-Disposition",
                        format!(
                            "attachment; filename={}",
                            serde_json::Value::String(file_name)
                        )
                        .parse()?,
                    );
                    headers.insert("Content-Type", "application/octet-stream".parse()?);

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

                    return ApiResponse::error("failed to retrieve file contents from backup")
                        .with_status(StatusCode::EXPECTATION_FAILED)
                        .ok();
                }
            }
        }

        let metadata = match server.filesystem.async_metadata(&path).await {
            Ok(metadata) => {
                if !metadata.is_file()
                    || server.filesystem.is_ignored(path, metadata.is_dir()).await
                {
                    return ApiResponse::error("file not found")
                        .with_status(StatusCode::NOT_FOUND)
                        .ok();
                } else {
                    metadata
                }
            }
            Err(_) => {
                return ApiResponse::error("file not found")
                    .with_status(StatusCode::NOT_FOUND)
                    .ok();
            }
        };

        let file = match server.filesystem.async_open(&path).await {
            Ok(file) => file,
            Err(_) => {
                return ApiResponse::error("file not found")
                    .with_status(StatusCode::NOT_FOUND)
                    .ok();
            }
        };

        let mut headers = HeaderMap::new();
        headers.insert("Content-Length", metadata.len().into());
        headers.insert(
            "Content-Disposition",
            format!(
                "attachment; filename={}",
                serde_json::Value::String(file_name)
            )
            .parse()?,
        );
        headers.insert("Content-Type", "application/octet-stream".parse()?);

        ApiResponse::new(Body::from_stream(
            tokio_util::io::ReaderStream::with_capacity(file, crate::BUFFER_SIZE),
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
