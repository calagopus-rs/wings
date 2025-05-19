use super::State;
use utoipa_axum::{router::OpenApiRouter, routes};

mod restore;

mod delete {
    use crate::routes::api::servers::_server_::GetServer;
    use axum::{extract::Path, http::StatusCode};
    use serde::Serialize;
    use utoipa::ToSchema;

    #[derive(ToSchema, Serialize)]
    struct Response {}

    #[utoipa::path(delete, path = "/", responses(
        (status = OK, body = inline(Response)),
    ))]
    pub async fn route(
        server: GetServer,
        Path((_server, backup_id)): Path<(uuid::Uuid, uuid::Uuid)>,
    ) -> (StatusCode, axum::Json<serde_json::Value>) {
        tokio::spawn(async move {
            if let Err(err) = crate::server::backup::delete_backup(
                crate::server::backup::BackupAdapter::Wings,
                &server,
                backup_id,
            )
            .await
            {
                tracing::error!(
                    "failed to delete backup {} (adapter = {:?}) for {}: {}",
                    backup_id,
                    crate::server::backup::BackupAdapter::Wings,
                    server.uuid,
                    err
                );
            }

            if let Err(err) = crate::server::backup::delete_backup(
                crate::server::backup::BackupAdapter::DdupBak,
                &server,
                backup_id,
            )
            .await
            {
                tracing::error!(
                    "failed to delete backup {} (adapter = {:?}) for {}: {}",
                    backup_id,
                    crate::server::backup::BackupAdapter::DdupBak,
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
        .nest("/restore", restore::router(state))
        .routes(routes!(delete::route))
        .with_state(state.clone())
}
