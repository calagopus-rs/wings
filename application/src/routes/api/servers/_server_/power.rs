use super::State;
use utoipa_axum::{router::OpenApiRouter, routes};

mod post {
    use crate::{
        response::{ApiResponse, ApiResponseResult},
        routes::{GetState, api::servers::_server_::GetServer},
    };
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
    ), request_body = inline(Payload))]
    pub async fn route(
        state: GetState,
        server: GetServer,
        axum::Json(data): axum::Json<Payload>,
    ) -> ApiResponseResult {
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
                    let auto_kill = server.configuration.read().await.auto_kill;
                    if let Err(err) = if auto_kill.enabled && auto_kill.seconds > 0 {
                        server
                            .stop_with_kill_timeout(
                                &state.docker,
                                std::time::Duration::from_secs(auto_kill.seconds),
                            )
                            .await
                    } else {
                        server.stop(&state.docker, aquire_timeout).await
                    } {
                        tracing::error!(
                            server = %server.uuid,
                            "failed to stop server: {:#?}",
                            err
                        );
                    }
                }
                crate::models::ServerPowerAction::Restart => {
                    let auto_kill = server.configuration.read().await.auto_kill;
                    if let Err(err) = if auto_kill.enabled && auto_kill.seconds > 0 {
                        server
                            .restart_with_kill_timeout(
                                &state.docker,
                                aquire_timeout,
                                std::time::Duration::from_secs(auto_kill.seconds),
                            )
                            .await
                    } else {
                        server.restart(&state.docker, None).await
                    } {
                        tracing::error!(
                            server = %server.uuid,
                            "failed to auto kill server: {:#?}",
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

        ApiResponse::json(Response {})
            .with_status(StatusCode::ACCEPTED)
            .ok()
    }
}

pub fn router(state: &State) -> OpenApiRouter<State> {
    OpenApiRouter::new()
        .routes(routes!(post::route))
        .with_state(state.clone())
}
