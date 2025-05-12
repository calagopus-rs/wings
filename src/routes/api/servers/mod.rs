use super::State;
use utoipa_axum::{router::OpenApiRouter, routes};

mod _server_;

mod get {
    use crate::routes::GetState;

    #[utoipa::path(get, path = "/", responses(
        (status = OK, body = Vec<crate::models::Server>),
    ))]
    pub async fn route(state: GetState) -> axum::Json<serde_json::Value> {
        let mut servers = Vec::new();

        for server in state.server_manager.get_servers().await.iter() {
            servers.push(server.to_api_response().await);
        }

        axum::Json(serde_json::to_value(&servers).unwrap())
    }
}

mod post {
    use crate::routes::{ApiError, GetState};
    use axum::http::StatusCode;
    use serde::{Deserialize, Serialize};
    use utoipa::ToSchema;

    #[derive(ToSchema, Deserialize)]
    pub struct Payload {
        uuid: uuid::Uuid,
        start_on_completion: bool,
    }

    #[derive(ToSchema, Serialize)]
    struct Response {}

    #[utoipa::path(post, path = "/", responses(
        (status = OK, body = inline(Response)),
        (status = CONFLICT, body = inline(ApiError))
    ), request_body = inline(Payload))]
    pub async fn route(
        state: GetState,
        axum::Json(data): axum::Json<Payload>,
    ) -> (StatusCode, axum::Json<serde_json::Value>) {
        if state
            .server_manager
            .get_servers()
            .await
            .iter()
            .any(|s| s.uuid == data.uuid)
        {
            return (
                StatusCode::CONFLICT,
                axum::Json(
                    serde_json::to_value(ApiError::new("server with this uuid already exists"))
                        .unwrap(),
                ),
            );
        }

        let mut server_data = state.config.client.server(data.uuid).await.unwrap();
        server_data.settings.start_on_completion = Some(data.start_on_completion);

        state.server_manager.create_server(server_data).await;

        (
            StatusCode::OK,
            axum::Json(serde_json::to_value(&Response {}).unwrap()),
        )
    }
}

pub fn router(state: &State) -> OpenApiRouter<State> {
    OpenApiRouter::new()
        .nest("/{server}", _server_::router(state))
        .routes(routes!(get::route))
        .routes(routes!(post::route))
        .with_state(state.clone())
}
