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
