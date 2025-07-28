use super::State;
use utoipa_axum::{router::OpenApiRouter, routes};

mod post {
    use crate::{
        response::{ApiResponse, ApiResponseResult},
        routes::{GetState, api::servers::_server_::GetServer},
        server::installation::InstallationScript,
    };
    use axum::http::StatusCode;
    use serde::Serialize;
    use utoipa::ToSchema;

    #[derive(ToSchema, Serialize)]
    struct Response {
        stdout: String,
        stderr: String,
    }

    #[utoipa::path(post, path = "/", responses(
        (status = OK, body = inline(Response)),
    ), params(
        (
            "server" = uuid::Uuid,
            description = "The server uuid",
            example = "123e4567-e89b-12d3-a456-426614174000",
        ),
    ), request_body = inline(InstallationScript))]
    pub async fn route(
        state: GetState,
        server: GetServer,
        axum::Json(data): axum::Json<InstallationScript>,
    ) -> ApiResponseResult {
        match crate::server::script::script_server(&server, &state.docker, data).await {
            Ok((stdout, stderr)) => ApiResponse::json(Response { stdout, stderr }).ok(),
            Err(err) => {
                tracing::error!(
                    server = %server.uuid,
                    "failed to run server script: {:#?}",
                    err
                );

                ApiResponse::error("failed to run server script")
                    .with_status(StatusCode::INTERNAL_SERVER_ERROR)
                    .ok()
            }
        }
    }
}

pub fn router(state: &State) -> OpenApiRouter<State> {
    OpenApiRouter::new()
        .routes(routes!(post::route))
        .with_state(state.clone())
}
