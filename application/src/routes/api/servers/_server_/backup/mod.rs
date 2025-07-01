use super::State;
use utoipa_axum::{router::OpenApiRouter, routes};

mod _backup_;

mod post {
    use crate::routes::{ApiError, api::servers::_server_::GetServer};
    use axum::http::StatusCode;
    use serde::{Deserialize, Serialize};
    use utoipa::ToSchema;

    #[derive(ToSchema, Deserialize)]
    pub struct Payload {
        adapter: crate::server::backup::BackupAdapter,
        uuid: uuid::Uuid,
        ignore: String,
    }

    #[derive(ToSchema, Serialize)]
    struct Response {}

    #[utoipa::path(post, path = "/", responses(
        (status = OK, body = inline(Response)),
        (status = CONFLICT, body = inline(ApiError)),
    ), params(
        (
            "server" = uuid::Uuid,
            description = "The server uuid",
            example = "123e4567-e89b-12d3-a456-426614174000",
        ),
    ), request_body = inline(Payload))]
    pub async fn route(
        server: GetServer,
        axum::Json(data): axum::Json<Payload>,
    ) -> (StatusCode, axum::Json<serde_json::Value>) {
        if crate::server::backup::InternalBackup::list_for_adapter(&server, data.adapter)
            .await
            .is_ok_and(|backups| backups.contains(&data.uuid))
        {
            return (
                StatusCode::CONFLICT,
                axum::Json(ApiError::new("backup already exists").to_json()),
            );
        }

        tokio::spawn(async move {
            if let Err(err) = crate::server::backup::InternalBackup::create(
                data.adapter,
                &server,
                data.uuid,
                data.ignore,
            )
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

        (
            StatusCode::OK,
            axum::Json(serde_json::to_value(Response {}).unwrap()),
        )
    }
}

pub fn router(state: &State) -> OpenApiRouter<State> {
    OpenApiRouter::new()
        .nest("/{backup}", _backup_::router(state))
        .routes(routes!(post::route))
        .with_state(state.clone())
}
