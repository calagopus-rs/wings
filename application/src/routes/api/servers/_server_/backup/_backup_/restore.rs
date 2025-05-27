use super::State;
use utoipa_axum::{router::OpenApiRouter, routes};

mod post {
    use crate::routes::{ApiError, GetState, api::servers::_server_::GetServer};
    use axum::{extract::Path, http::StatusCode};
    use serde::{Deserialize, Serialize};
    use utoipa::ToSchema;

    #[derive(ToSchema, Deserialize)]
    pub struct Payload {
        adapter: crate::server::backup::BackupAdapter,
        truncate_directory: bool,
        download_url: Option<String>,
    }

    #[derive(ToSchema, Serialize)]
    struct Response {}

    #[utoipa::path(post, path = "/", responses(
        (status = OK, body = inline(Response)),
        (status = NOT_FOUND, body = inline(ApiError)),
    ), request_body = inline(Payload))]
    pub async fn route(
        state: GetState,
        server: GetServer,
        Path((_server, backup_id)): Path<(uuid::Uuid, uuid::Uuid)>,
        axum::Json(data): axum::Json<Payload>,
    ) -> (StatusCode, axum::Json<serde_json::Value>) {
        if data.adapter == crate::server::backup::BackupAdapter::S3 && data.download_url.is_none() {
            return (
                StatusCode::BAD_REQUEST,
                axum::Json(
                    ApiError::new("unable to restore s3 backup without download_url").to_json(),
                ),
            );
        }

        if data.adapter != crate::server::backup::BackupAdapter::S3
            && !crate::server::backup::list_backups(data.adapter, &server)
                .await
                .unwrap()
                .iter()
                .copied()
                .any(|b| b == backup_id)
        {
            return (
                StatusCode::NOT_FOUND,
                axum::Json(ApiError::new("backup not found").to_json()),
            );
        }

        tokio::spawn(async move {
            if let Err(err) = crate::server::backup::restore_backup(
                data.adapter,
                &state.docker,
                &server,
                backup_id,
                data.truncate_directory,
                data.download_url,
            )
            .await
            {
                tracing::error!(
                    "failed to restore backup {} (adapter = {:?}) for {}: {}",
                    backup_id,
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
        .routes(routes!(post::route))
        .with_state(state.clone())
}
