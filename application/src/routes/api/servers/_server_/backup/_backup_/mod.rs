use super::State;
use utoipa_axum::{router::OpenApiRouter, routes};

mod restore;

mod delete {
    use crate::routes::{ApiError, api::servers::_server_::GetServer};
    use axum::{extract::Path, http::StatusCode};
    use serde::Serialize;
    use utoipa::ToSchema;

    #[derive(ToSchema, Serialize)]
    struct Response {}

    #[utoipa::path(delete, path = "/", responses(
        (status = OK, body = inline(Response)),
        (status = NOT_FOUND, body = inline(ApiError)),
    ))]
    pub async fn route(
        server: GetServer,
        Path((_server, backup_id)): Path<(uuid::Uuid, uuid::Uuid)>,
    ) -> (StatusCode, axum::Json<serde_json::Value>) {
        let backup = match crate::server::backup::InternalBackup::find(&server, backup_id).await {
            Some(backup) => backup,
            None => {
                return (
                    StatusCode::NOT_FOUND,
                    axum::Json(ApiError::new("backup not found").to_json()),
                );
            }
        };

        tokio::spawn(async move {
            if let Err(err) = backup.delete(&server).await {
                tracing::error!(
                    server = %server.uuid,
                    backup = %backup.uuid,
                    adapter = ?backup.adapter,
                    "failed to delete backup: {:#?}",
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
