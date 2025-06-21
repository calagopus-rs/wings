use super::State;
use utoipa_axum::{router::OpenApiRouter, routes};

mod post {
    use crate::routes::{GetState, api::servers::_server_::GetServer};
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
    pub async fn route(state: GetState, server: GetServer) -> axum::Json<serde_json::Value> {
        if let Ok(configuration) = state.config.client.server(server.uuid).await {
            server
                .update_configuration(
                    configuration.settings,
                    configuration.process_configuration,
                    &state.docker,
                )
                .await;
        }

        axum::Json(serde_json::to_value(&Response {}).unwrap())
    }
}

pub fn router(state: &State) -> OpenApiRouter<State> {
    OpenApiRouter::new()
        .routes(routes!(post::route))
        .with_state(state.clone())
}
