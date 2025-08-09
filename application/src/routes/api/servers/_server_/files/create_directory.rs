use super::State;
use utoipa_axum::{router::OpenApiRouter, routes};

mod post {
    use crate::{
        response::{ApiResponse, ApiResponseResult},
        routes::{ApiError, api::servers::_server_::GetServer},
    };
    use axum::http::StatusCode;
    use serde::{Deserialize, Serialize};
    use std::path::PathBuf;
    use utoipa::ToSchema;

    #[derive(ToSchema, Deserialize)]
    pub struct Payload {
        name: String,
        path: String,
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
    ), request_body = inline(Payload))]
    pub async fn route(
        server: GetServer,
        axum::Json(data): axum::Json<Payload>,
    ) -> ApiResponseResult {
        let path = match server.filesystem.async_canonicalize(&data.path).await {
            Ok(path) => path,
            Err(_) => PathBuf::from(data.path),
        };

        let metadata = server.filesystem.async_metadata(&path).await;
        if !metadata.map(|m| m.is_dir()).unwrap_or(true) {
            return ApiResponse::error("path is not a directory")
                .with_status(StatusCode::EXPECTATION_FAILED)
                .ok();
        }

        if server.filesystem.is_ignored(&path, true).await {
            return ApiResponse::error("path not found")
                .with_status(StatusCode::NOT_FOUND)
                .ok();
        }

        let destination = path.join(&data.name);

        if server.filesystem.is_ignored(&destination, true).await {
            return ApiResponse::error("destination not found")
                .with_status(StatusCode::EXPECTATION_FAILED)
                .ok();
        }

        server.filesystem.async_create_dir_all(&destination).await?;
        server.filesystem.chown_path(&destination).await?;

        ApiResponse::json(Response {}).ok()
    }
}

pub fn router(state: &State) -> OpenApiRouter<State> {
    OpenApiRouter::new()
        .routes(routes!(post::route))
        .with_state(state.clone())
}
