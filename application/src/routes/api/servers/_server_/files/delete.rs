use super::State;
use utoipa_axum::{router::OpenApiRouter, routes};

mod post {
    use crate::{
        response::{ApiResponse, ApiResponseResult},
        routes::{ApiError, api::servers::_server_::GetServer},
    };
    use axum::http::StatusCode;
    use serde::{Deserialize, Serialize};
    use utoipa::ToSchema;

    #[derive(ToSchema, Deserialize)]
    pub struct Payload {
        #[serde(default)]
        root: String,

        files: Vec<String>,
    }

    #[derive(ToSchema, Serialize)]
    struct Response {
        deleted: usize,
    }

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
        let root = match server.filesystem.async_canonicalize(data.root).await {
            Ok(path) => path,
            Err(_) => {
                return ApiResponse::error("root not found")
                    .with_status(StatusCode::NOT_FOUND)
                    .ok();
            }
        };

        let metadata = server.filesystem.async_symlink_metadata(&root).await;
        if !metadata.map(|m| m.is_dir()).unwrap_or(false) {
            return ApiResponse::error("root is not a directory")
                .with_status(StatusCode::EXPECTATION_FAILED)
                .ok();
        }

        let mut deleted_count = 0;
        for file in data.files {
            let destination = root.join(file);
            if destination == root {
                continue;
            }

            if server
                .filesystem
                .is_ignored(
                    &destination,
                    server
                        .filesystem
                        .async_symlink_metadata(&destination)
                        .await
                        .is_ok_and(|m| m.is_dir()),
                )
                .await
            {
                continue;
            }

            if server.filesystem.truncate_path(&destination).await.is_ok() {
                deleted_count += 1;
            }
        }

        ApiResponse::json(Response {
            deleted: deleted_count,
        })
        .ok()
    }
}

pub fn router(state: &State) -> OpenApiRouter<State> {
    OpenApiRouter::new()
        .routes(routes!(post::route))
        .with_state(state.clone())
}
