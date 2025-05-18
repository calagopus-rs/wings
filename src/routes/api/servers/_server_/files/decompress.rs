use super::State;
use utoipa_axum::{router::OpenApiRouter, routes};

mod post {
    use crate::routes::{ApiError, api::servers::_server_::GetServer};
    use axum::http::StatusCode;
    use serde::{Deserialize, Serialize};
    use std::sync::Arc;
    use utoipa::ToSchema;

    #[derive(ToSchema, Deserialize)]
    pub struct Payload {
        #[serde(default)]
        pub root: String,

        pub file: String,
    }

    #[derive(ToSchema, Serialize)]
    struct Response {}

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

        let source = root.join(data.file);
        if !server.filesystem.is_safe_path(&source).await {
            return (
                StatusCode::NOT_FOUND,
                axum::Json(ApiError::new("file not found").to_json()),
            );
        }

        if server.filesystem.is_ignored(
            &source,
            tokio::fs::symlink_metadata(&source)
                .await
                .is_ok_and(|m| m.is_dir()),
        ) {
            return (
                StatusCode::NOT_FOUND,
                axum::Json(ApiError::new("file not found").to_json()),
            );
        }

        let mut archive = match crate::server::filesystem::archive::Archive::open(
            Arc::clone(&server.filesystem),
            source,
        )
        .await
        {
            Some(archive) => archive,
            None => {
                return (
                    StatusCode::EXPECTATION_FAILED,
                    axum::Json(ApiError::new("failed to open archive").to_json()),
                );
            }
        };

        let reader = archive.reader().await;
        archive.extract(root.clone(), reader).await.unwrap();
        server.filesystem.chown_path(&root).await;

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
