use super::State;
use utoipa_axum::{router::OpenApiRouter, routes};

mod post {
    use crate::routes::{ApiError, api::servers::_server_::GetServer};
    use axum::http::StatusCode;
    use serde::{Deserialize, Serialize};
    use std::{fs::Permissions, os::unix::fs::PermissionsExt};
    use utoipa::ToSchema;

    #[derive(ToSchema, Deserialize)]
    pub struct ChmodFile {
        pub file: String,
        pub mode: String,
    }

    #[derive(ToSchema, Deserialize)]
    pub struct Payload {
        #[serde(default)]
        pub root: String,

        #[schema(inline)]
        pub files: Vec<ChmodFile>,
    }

    #[derive(ToSchema, Serialize)]
    struct Response {
        pub updated: usize,
    }

    #[utoipa::path(post, path = "/", responses(
        (status = OK, body = inline(Response)),
        (status = NOT_FOUND, body = inline(ApiError)),
        (status = EXPECTATION_FAILED, body = inline(ApiError)),
    ), request_body = inline(Payload))]
    pub async fn route(
        server: GetServer,
        axum::Json(data): axum::Json<Payload>,
    ) -> (StatusCode, axum::Json<serde_json::Value>) {
        let root = match server.filesystem.safe_path(&data.root).await {
            Some(path) => path,
            None => {
                return (
                    StatusCode::NOT_FOUND,
                    axum::Json(ApiError::new("root not found").to_json()),
                );
            }
        };

        let metadata = root.symlink_metadata();
        if !metadata.map(|m| m.is_dir()).unwrap_or(true) {
            return (
                StatusCode::EXPECTATION_FAILED,
                axum::Json(ApiError::new("root is not a directory").to_json()),
            );
        }

        let mut updated_count = 0;
        for file in data.files {
            let source = root.join(file.file);
            if !server.filesystem.is_safe_path(&source).await {
                continue;
            }
            let metadata = match tokio::fs::symlink_metadata(&source).await {
                Ok(metadata) => metadata,
                Err(_) => {
                    continue;
                }
            };

            if server
                .filesystem
                .is_ignored(&source, metadata.is_dir())
                .await
            {
                continue;
            }

            let mode = match u32::from_str_radix(&file.mode, 8) {
                Ok(mode) => mode,
                Err(_) => {
                    continue;
                }
            };

            if tokio::fs::set_permissions(&source, Permissions::from_mode(mode))
                .await
                .is_ok()
            {
                updated_count += 1;
            }
        }

        (
            StatusCode::OK,
            axum::Json(
                serde_json::to_value(&Response {
                    updated: updated_count,
                })
                .unwrap(),
            ),
        )
    }
}

pub fn router(state: &State) -> OpenApiRouter<State> {
    OpenApiRouter::new()
        .routes(routes!(post::route))
        .with_state(state.clone())
}
