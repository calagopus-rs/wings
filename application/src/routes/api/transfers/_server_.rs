use super::State;
use utoipa_axum::{router::OpenApiRouter, routes};

mod delete {
    use crate::{
        response::{ApiResponse, ApiResponseResult},
        routes::{ApiError, GetState},
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
    ))]
    pub async fn route(state: GetState, Path(server): Path<uuid::Uuid>) -> ApiResponseResult {
        let server = state
            .server_manager
            .get_servers()
            .await
            .iter()
            .find(|s| s.uuid == server)
            .cloned();

        let server = match server {
            Some(server) => server,
            None => {
                return ApiResponse::error("server not found")
                    .with_status(StatusCode::NOT_FOUND)
                    .ok();
            }
        };

        server
            .transferring
            .store(false, std::sync::atomic::Ordering::SeqCst);
        if let Some(handle) = server.incoming_transfer.write().await.take() {
            handle.abort();
        }

        ApiResponse::json(Response {}).ok()
    }
}

pub fn router(state: &State) -> OpenApiRouter<State> {
    OpenApiRouter::new()
        .routes(routes!(delete::route))
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            crate::routes::api::auth,
        ))
        .with_state(state.clone())
}
