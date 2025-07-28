use super::State;
use utoipa_axum::{router::OpenApiRouter, routes};

mod delete {
    use crate::{
        response::{ApiResponse, ApiResponseResult},
        routes::{ApiError, api::servers::_server_::GetServer},
    };
    use axum::{extract::Path, http::StatusCode};
    use serde::Serialize;
    use utoipa::ToSchema;

    #[derive(ToSchema, Serialize)]
    struct Response {}

    #[utoipa::path(delete, path = "/", responses(
        (status = OK, body = inline(Response)),
        (status = NOT_FOUND, body = ApiError),
    ), params(
        (
            "server" = uuid::Uuid,
            description = "The server uuid",
            example = "123e4567-e89b-12d3-a456-426614174000",
        ),
        (
            "pull" = uuid::Uuid,
            description = "The pull uuid",
            example = "123e4567-e89b-12d3-a456-426614174000",
        ),
    ))]
    pub async fn route(
        server: GetServer,
        Path((_server, pull_id)): Path<(uuid::Uuid, uuid::Uuid)>,
    ) -> ApiResponseResult {
        let pull = server.filesystem.pulls().await;
        let pull = match pull.iter().find(|p| *p.0 == pull_id) {
            Some(pull) => pull.1,
            None => {
                return ApiResponse::error("pull not found")
                    .with_status(StatusCode::NOT_FOUND)
                    .ok();
            }
        };

        if let Some(download) = pull.write().await.task.take() {
            download.abort();
        }

        server.filesystem.pulls.write().await.remove(&pull_id);

        ApiResponse::json(Response {}).ok()
    }
}

pub fn router(state: &State) -> OpenApiRouter<State> {
    OpenApiRouter::new()
        .routes(routes!(delete::route))
        .with_state(state.clone())
}
