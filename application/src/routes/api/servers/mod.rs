use super::State;
use utoipa_axum::{router::OpenApiRouter, routes};

pub mod _server_;

mod get {
    use crate::{
        response::{ApiResponse, ApiResponseResult},
        routes::GetState,
    };

    #[utoipa::path(get, path = "/", responses(
        (status = OK, body = Vec<crate::models::Server>),
    ))]
    pub async fn route(state: GetState) -> ApiResponseResult {
        let mut servers = Vec::new();

        for server in state.server_manager.get_servers().await.iter() {
            servers.push(server.to_api_response().await);
        }

        ApiResponse::json(servers).ok()
    }
}

mod post {
    use crate::{
        response::{ApiResponse, ApiResponseResult},
        routes::{ApiError, GetState},
    };
    use axum::http::StatusCode;
    use serde::{Deserialize, Serialize};
    use utoipa::ToSchema;

    #[derive(ToSchema, Deserialize)]
    pub struct Payload {
        uuid: uuid::Uuid,
        #[serde(default)]
        start_on_completion: bool,
        #[serde(default)]
        skip_scripts: bool,
    }

    #[derive(ToSchema, Serialize)]
    struct Response {}

    #[utoipa::path(post, path = "/", responses(
        (status = OK, body = inline(Response)),
        (status = CONFLICT, body = ApiError)
    ), request_body = inline(Payload))]
    pub async fn route(
        state: GetState,
        axum::Json(data): axum::Json<Payload>,
    ) -> ApiResponseResult {
        if state
            .server_manager
            .get_servers()
            .await
            .iter()
            .any(|s| s.uuid == data.uuid)
        {
            return ApiResponse::error("server with this uuid already exists")
                .with_status(StatusCode::CONFLICT)
                .ok();
        }

        let mut server_data = state.config.client.server(data.uuid).await?;
        server_data.settings.start_on_completion = Some(data.start_on_completion);

        state
            .server_manager
            .create_server(&state, server_data, !data.skip_scripts)
            .await;

        ApiResponse::json(Response {}).ok()
    }
}

pub fn router(state: &State) -> OpenApiRouter<State> {
    OpenApiRouter::new()
        .nest("/{server}", _server_::router(state))
        .routes(routes!(get::route))
        .routes(routes!(post::route))
        .with_state(state.clone())
}
