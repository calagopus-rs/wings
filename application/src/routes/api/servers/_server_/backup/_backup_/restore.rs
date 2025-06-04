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

        let backup = if data.adapter != crate::server::backup::BackupAdapter::S3 {
            match crate::server::backup::InternalBackup::find(&server, backup_id).await {
                Some(backup) => backup,
                None => {
                    return (
                        StatusCode::NOT_FOUND,
                        axum::Json(ApiError::new("backup not found").to_json()),
                    );
                }
            }
        } else {
            crate::server::backup::InternalBackup {
                adapter: data.adapter,
                uuid: backup_id,
            }
        };

        tokio::spawn(async move {
            if let Err(err) = backup
                .restore(
                    &state.docker,
                    &server,
                    data.truncate_directory,
                    data.download_url,
                )
                .await
            {
                tracing::error!(
                    server = %server.uuid,
                    backup = %backup.uuid,
                    adapter = ?backup.adapter,
                    "failed to restore backup: {:#?}",
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
