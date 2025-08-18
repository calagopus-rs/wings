use super::State;
use utoipa_axum::{router::OpenApiRouter, routes};

mod post {
    use crate::{
        io::compression::CompressionLevel,
        response::{ApiResponse, ApiResponseResult},
        routes::{ApiError, GetState, api::servers::_server_::GetServer},
    };
    use axum::http::StatusCode;
    use serde::{Deserialize, Serialize};
    use utoipa::ToSchema;

    #[derive(ToSchema, Deserialize)]
    pub struct Payload {
        url: String,
        token: String,

        #[serde(default)]
        archive_format: crate::server::transfer::TransferArchiveFormat,
        #[serde(default, deserialize_with = "crate::deserialize::deserialize_optional")]
        compression_level: Option<CompressionLevel>,
        #[serde(
            default,
            deserialize_with = "crate::deserialize::deserialize_defaultable"
        )]
        backups: Vec<uuid::Uuid>,
        #[serde(default)]
        delete_backups: bool,
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
        if server.is_locked_state() {
            return ApiResponse::error("server is locked")
                .with_status(StatusCode::CONFLICT)
                .ok();
        }

        server
            .transferring
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let mut transfer = crate::server::transfer::OutgoingServerTransfer::new(
            &server,
            data.archive_format,
            data.compression_level
                .unwrap_or(state.config.system.backups.compression_level),
        );

        if transfer
            .start(
                &state.backup_manager,
                data.url,
                data.token,
                data.backups,
                data.delete_backups,
            )
            .is_ok()
        {
            server.outgoing_transfer.write().await.replace(transfer);
        }

        ApiResponse::json(Response {})
            .with_status(StatusCode::ACCEPTED)
            .ok()
    }
}

mod delete {
    use crate::{
        response::{ApiResponse, ApiResponseResult},
        routes::{ApiError, api::servers::_server_::GetServer},
    };
    use axum::http::StatusCode;
    use serde::Serialize;
    use utoipa::ToSchema;

    #[derive(ToSchema, Serialize)]
    struct Response {}

    #[utoipa::path(delete, path = "/", responses(
        (status = OK, body = inline(Response)),
        (status = EXPECTATION_FAILED, body = ApiError),
    ), params(
        (
            "server" = uuid::Uuid,
            description = "The server uuid",
            example = "123e4567-e89b-12d3-a456-426614174000",
        ),
    ))]
    pub async fn route(server: GetServer) -> ApiResponseResult {
        if !server
            .transferring
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            return ApiResponse::error("server is not transferring")
                .with_status(StatusCode::EXPECTATION_FAILED)
                .ok();
        }

        server
            .transferring
            .store(false, std::sync::atomic::Ordering::SeqCst);
        if let Some(transfer) = server.outgoing_transfer.write().await.take()
            && let Some(handle) = transfer.task.as_ref()
        {
            handle.abort();
        }

        ApiResponse::json(Response {}).ok()
    }
}

pub fn router(state: &State) -> OpenApiRouter<State> {
    OpenApiRouter::new()
        .routes(routes!(post::route))
        .routes(routes!(delete::route))
        .with_state(state.clone())
}
