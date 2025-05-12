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
    ))]
    pub async fn route(
        state: GetState,
        server: GetServer,
    ) -> (StatusCode, axum::Json<serde_json::Value>) {
        if server.is_locked_state()
            || server.state.get_state() != crate::server::state::ServerState::Offline
        {
            return (
                StatusCode::CONFLICT,
                axum::Json(
                    serde_json::to_value(ApiError::new("server is not offline or is locked"))
                        .unwrap(),
                ),
            );
        }

        server.destroy_container(&state.docker).await;
        tokio::spawn(async move {
            if let Err(err) =
                crate::server::installation::install_server(&server, &state.docker, true).await
            {
                crate::logger::log(
                    crate::logger::LoggerLevel::Error,
                    format!("Failed to reinstall server: {}", err),
                );
            }
        });

        (
            StatusCode::OK,
            axum::Json(serde_json::to_value(&Response {}).unwrap()),
        )
    }
}

pub fn router(state: &State) -> OpenApiRouter<State> {
    OpenApiRouter::new()
        .routes(routes!(post::route))
        .with_state(state.clone())
}
