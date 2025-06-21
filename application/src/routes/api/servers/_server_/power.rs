use super::State;
use utoipa_axum::{router::OpenApiRouter, routes};

mod post {
    use crate::routes::{GetState, api::servers::_server_::GetServer};
    use axum::http::StatusCode;
    use serde::{Deserialize, Serialize};
    use utoipa::ToSchema;

    #[derive(ToSchema, Deserialize)]
    pub struct Payload {
        action: crate::models::ServerPowerAction,
        wait_seconds: Option<u64>,
    }

    #[derive(ToSchema, Serialize)]
    struct Response {}

    #[utoipa::path(post, path = "/", responses(
        (status = ACCEPTED, body = inline(Response)),
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
        axum::Json(data): axum::Json<Payload>,
    ) -> (StatusCode, axum::Json<serde_json::Value>) {
        let aquire_timeout = data.wait_seconds.map(std::time::Duration::from_secs);

        tokio::spawn(async move {
            match data.action {
                crate::models::ServerPowerAction::Start => {
                    if let Err(err) = server.start(&state.docker, aquire_timeout).await {
                        tracing::error!(
                            server = %server.uuid,
                            "failed to start server: {:#?}",
                            err
                        );
                    }
                }
                crate::models::ServerPowerAction::Stop => {
                    if let Err(err) = server.stop(&state.docker, aquire_timeout).await {
                        tracing::error!(
                            server = %server.uuid,
                            "failed to stop server: {:#?}",
                            err
                        );
                    }
                }
                crate::models::ServerPowerAction::Restart => {
                    if let Err(err) = server.restart(&state.docker, aquire_timeout).await {
                        tracing::error!(
                            server = %server.uuid,
                            "failed to restart server: {:#?}",
                            err
                        );
                    }
                }
                crate::models::ServerPowerAction::Kill => {
                    if let Err(err) = server.kill(&state.docker).await {
                        tracing::error!(
                            server = %server.uuid,
                            "failed to kill server: {:#?}",
                            err
                        );
                    }
                }
            }
        });

        (
            StatusCode::ACCEPTED,
            axum::Json(serde_json::to_value(&Response {}).unwrap()),
        )
    }
}

pub fn router(state: &State) -> OpenApiRouter<State> {
    OpenApiRouter::new()
        .routes(routes!(post::route))
        .with_state(state.clone())
}
