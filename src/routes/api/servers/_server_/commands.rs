use super::State;
use utoipa_axum::{router::OpenApiRouter, routes};

mod post {
    use crate::routes::{ApiError, api::servers::_server_::GetServer};
    use axum::http::StatusCode;
    use serde::{Deserialize, Serialize};
    use utoipa::ToSchema;

    #[derive(ToSchema, Deserialize)]
    pub struct Payload {
        commands: Vec<String>,
    }

    #[derive(ToSchema, Serialize)]
    struct Response {}

    #[utoipa::path(post, path = "/", responses(
        (status = OK, body = inline(Response)),
        (status = EXPECTATION_FAILED, body = inline(ApiError)),
    ))]
    pub async fn route(
        server: GetServer,
        axum::Json(data): axum::Json<Payload>,
    ) -> (StatusCode, axum::Json<serde_json::Value>) {
        if server.state.get_state() == crate::server::state::ServerState::Offline {
            return (
                StatusCode::EXPECTATION_FAILED,
                axum::Json(ApiError::new("server is offline").to_json()),
            );
        }

        if let Some(stdin) = server.container_stdin().await {
            for mut command in data.commands {
                command.push('\n');

                stdin.send(command).await.ok();
            }
        } else {
            return (
                StatusCode::EXPECTATION_FAILED,
                axum::Json(ApiError::new("failed to get stdin (is server offline?)").to_json()),
            );
        }

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
