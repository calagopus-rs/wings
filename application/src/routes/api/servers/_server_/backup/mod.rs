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
    ), request_body = inline(Payload))]
    pub async fn route(
        server: GetServer,
        axum::Json(data): axum::Json<Payload>,
    ) -> (StatusCode, axum::Json<serde_json::Value>) {
        let backups = crate::server::backup::InternalBackup::list(&server).await;
        if backups.into_iter().any(|b| b.uuid == data.uuid) {
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
            axum::Json(serde_json::to_value(&Response {}).unwrap()),
        )
    }
}

pub fn router(state: &State) -> OpenApiRouter<State> {
    OpenApiRouter::new()
        .nest("/{backup}", _backup_::router(state))
        .routes(routes!(post::route))
        .with_state(state.clone())
}
