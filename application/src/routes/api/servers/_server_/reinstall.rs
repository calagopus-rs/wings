use super::State;
use utoipa_axum::{router::OpenApiRouter, routes};

mod post {
    use crate::routes::{ApiError, GetState, api::servers::_server_::GetServer};
    use axum::http::StatusCode;
    use serde::Serialize;
    use utoipa::ToSchema;

    #[derive(ToSchema, Serialize)]
    struct Response {}

    #[utoipa::path(post, path = "/", responses(
        (status = OK, body = inline(Response)),
        (status = CONFLICT, body = inline(ApiError)),
    ), params(
        (
            "server" = uuid::Uuid,
            description = "The server uuid",
            example = "123e4567-e89b-12d3-a456-426614174000",
        ),
    ))]
    pub async fn route(
        state: GetState,
        server: GetServer,
    ) -> (StatusCode, axum::Json<serde_json::Value>) {
        if server.is_locked_state() {
            return (
                StatusCode::CONFLICT,
                axum::Json(serde_json::to_value(ApiError::new("server is locked")).unwrap()),
            );
        }

        server
            .stop_with_kill_timeout(&state.docker, std::time::Duration::from_secs(30))
            .await;
        server.sync_configuration(&state.docker).await;

        tokio::spawn(async move {
            if let Err(err) =
                crate::server::installation::install_server(&server, &state.docker, true).await
            {
                tracing::error!(
                    server = %server.uuid,
                    "failed to reinstall server: {:#?}",
                    err
                );
            }
        });

        (
            StatusCode::OK,
            axum::Json(serde_json::to_value(Response {}).unwrap()),
        )
    }
}

pub fn router(state: &State) -> OpenApiRouter<State> {
    OpenApiRouter::new()
        .routes(routes!(post::route))
        .with_state(state.clone())
}
