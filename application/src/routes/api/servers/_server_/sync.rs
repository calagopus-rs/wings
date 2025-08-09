use super::State;
use utoipa_axum::{router::OpenApiRouter, routes};

mod post {
    use crate::{
        response::{ApiResponse, ApiResponseResult},
        routes::{GetState, api::servers::_server_::GetServer},
        server::state::ServerState,
    };
    use serde::Serialize;
    use utoipa::ToSchema;

    #[derive(ToSchema, Serialize)]
    struct Response {}

    #[utoipa::path(post, path = "/", responses(
        (status = OK, body = inline(Response)),
    ), params(
        (
            "server" = uuid::Uuid,
            description = "The server uuid",
            example = "123e4567-e89b-12d3-a456-426614174000",
        ),
    ))]
    pub async fn route(state: GetState, server: GetServer) -> ApiResponseResult {
        if let Ok(configuration) = state.config.client.server(server.uuid).await {
            let suspended = configuration.settings.suspended;

            server
                .update_configuration(
                    configuration.settings,
                    configuration.process_configuration,
                    &state.docker,
                )
                .await;

            if suspended && server.state.get_state() != ServerState::Offline {
                tokio::spawn(async move {
                    if let Err(err) = server
                        .stop_with_kill_timeout(&state.docker, std::time::Duration::from_secs(30))
                        .await
                    {
                        tracing::error!(%err, "failed to stop server after being suspended");
                    }
                });
            }
        }

        ApiResponse::json(Response {}).ok()
    }
}

pub fn router(state: &State) -> OpenApiRouter<State> {
    OpenApiRouter::new()
        .routes(routes!(post::route))
        .with_state(state.clone())
}
