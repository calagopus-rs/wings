use super::State;
use utoipa_axum::{router::OpenApiRouter, routes};

mod post {
    use crate::{
        response::{ApiResponse, ApiResponseResult},
        routes::{ApiError, GetState, api::servers::_server_::GetServer},
    };
    use axum::{extract::rejection::JsonRejection, http::StatusCode};
    use serde::{Deserialize, Serialize};
    use utoipa::ToSchema;

    #[derive(ToSchema, Deserialize)]
    pub struct Payload {
        #[serde(default)]
        truncate_directory: bool,
    }

    #[derive(ToSchema, Serialize)]
    struct Response {}

    #[utoipa::path(post, path = "/", responses(
        (status = ACCEPTED, body = inline(Response)),
        (status = CONFLICT, body = ApiError),
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
        data: Result<axum::Json<Payload>, JsonRejection>,
    ) -> ApiResponseResult {
        let data = match data {
            Ok(data) => data.0,
            Err(_) => Payload {
                truncate_directory: false,
            },
        };

        if server.is_locked_state() {
            return ApiResponse::error("server is locked")
                .with_status(StatusCode::CONFLICT)
                .ok();
        }

        server
            .stop_with_kill_timeout(std::time::Duration::from_secs(30), false)
            .await?;
        server.sync_configuration().await;

        tokio::spawn(async move {
            if data.truncate_directory
                && let Err(err) = server.filesystem.truncate_root().await
            {
                tracing::error!(
                    server = %server.uuid,
                    "failed to truncate root directory before reinstalling server: {:#?}",
                    err
                );
            }

            if let Err(err) =
                crate::server::installation::install_server(&server, &state.docker, true, false)
                    .await
            {
                tracing::error!(
                    server = %server.uuid,
                    "failed to reinstall server: {:#?}",
                    err
                );
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
