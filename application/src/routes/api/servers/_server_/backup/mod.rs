use super::State;
use utoipa_axum::{router::OpenApiRouter, routes};

mod _backup_;

mod post {
    use crate::{
        response::{ApiResponse, ApiResponseResult},
        routes::{ApiError, GetState, api::servers::_server_::GetServer},
        server::backup::adapters::BackupAdapter,
    };
    use axum::http::StatusCode;
    use serde::{Deserialize, Serialize};
    use utoipa::ToSchema;

    #[derive(ToSchema, Deserialize)]
    pub struct Payload {
        adapter: BackupAdapter,
        uuid: uuid::Uuid,
        ignore: String,
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
        axum::Json(data): axum::Json<Payload>,
    ) -> ApiResponseResult {
        if state.backup_manager.fast_contains(&server, data.uuid).await {
            return ApiResponse::error("backup already exists")
                .with_status(StatusCode::CONFLICT)
                .ok();
        }

        tokio::spawn(async move {
            if let Err(err) = state
                .backup_manager
                .create(data.adapter, &server, data.uuid, data.ignore)
                .await
            {
                tracing::error!(
                    "failed to create backup {} (adapter = {:?}) for {}: {}",
                    data.uuid,
                    data.adapter,
                    server.uuid,
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
        .nest("/{backup}", _backup_::router(state))
        .routes(routes!(post::route))
        .with_state(state.clone())
}
