use super::State;
use utoipa_axum::{router::OpenApiRouter, routes};

mod post {
    use crate::{
        response::{ApiResponse, ApiResponseResult},
        routes::{ApiError, api::servers::_server_::GetServer},
    };
    use axum::{
        body::Body,
        extract::Query,
        http::{HeaderMap, StatusCode},
    };
    use futures_util::StreamExt;
    use serde::{Deserialize, Serialize};
    use std::path::PathBuf;
    use tokio::io::AsyncWriteExt;
    use utoipa::ToSchema;

    #[derive(ToSchema, Deserialize)]
    pub struct Params {
        file: String,
    }

    #[derive(ToSchema, Serialize)]
    struct Response {}

    #[utoipa::path(post, path = "/", responses(
        (status = OK, body = inline(Response)),
        (status = NOT_FOUND, body = ApiError),
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
    ), request_body = String)]
    pub async fn route(
        server: GetServer,
        headers: HeaderMap,
        Query(data): Query<Params>,
        body: Body,
    ) -> ApiResponseResult {
        let path = match server.filesystem.async_canonicalize(&data.file).await {
            Ok(path) => path,
            Err(_) => PathBuf::from(data.file),
        };

        let content_size: i64 = headers
            .get("Content-Length")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        let metadata = server.filesystem.async_metadata(&path).await;

        if server
            .filesystem
            .is_ignored(
                &path,
                metadata.as_ref().map(|m| m.is_dir()).unwrap_or(false),
            )
            .await
        {
            return ApiResponse::error("file not found")
                .with_status(StatusCode::NOT_FOUND)
                .ok();
        }

        let old_content_size = if let Ok(metadata) = metadata {
            if !metadata.is_file() {
                return ApiResponse::error("file is not a file")
                    .with_status(StatusCode::EXPECTATION_FAILED)
                    .ok();
            }

            metadata.len() as i64
        } else {
            0
        };

        let parent = match path.parent() {
            Some(parent) => parent,
            None => {
                return ApiResponse::error("file has no parent")
                    .with_status(StatusCode::EXPECTATION_FAILED)
                    .ok();
            }
        };

        if server.filesystem.is_ignored(parent, true).await {
            return ApiResponse::error("parent directory not found")
                .with_status(StatusCode::EXPECTATION_FAILED)
                .ok();
        }

        server.filesystem.async_create_dir_all(parent).await?;

        if !server
            .filesystem
            .async_allocate_in_path(parent, content_size - old_content_size, false)
            .await
        {
            return ApiResponse::error("failed to allocate space")
                .with_status(StatusCode::EXPECTATION_FAILED)
                .ok();
        }

        let mut file = server.filesystem.async_create(&path).await?;
        let mut stream = body.into_data_stream();

        while let Some(Ok(chunk)) = stream.next().await {
            file.write_all(&chunk).await?;
        }

        file.flush().await?;
        file.sync_all().await?;

        server.filesystem.chown_path(&path).await?;

        ApiResponse::json(Response {}).ok()
    }
}

pub fn router(state: &State) -> OpenApiRouter<State> {
    OpenApiRouter::new()
        .routes(routes!(post::route))
        .with_state(state.clone())
}
