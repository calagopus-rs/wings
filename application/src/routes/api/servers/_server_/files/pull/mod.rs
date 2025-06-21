use super::State;
use utoipa_axum::{router::OpenApiRouter, routes};

mod _pull_;

mod get {
    use crate::routes::api::servers::_server_::GetServer;
    use serde::Serialize;
    use utoipa::ToSchema;

    #[derive(ToSchema, Serialize)]
    struct Response {
        downloads: Vec<crate::models::Download>,
    }

    #[utoipa::path(get, path = "/", responses(
        (status = OK, body = inline(Response)),
    ))]
    pub async fn route(server: GetServer) -> axum::Json<serde_json::Value> {
        let mut downloads = Vec::new();

        for download in server.filesystem.pulls().await.values() {
            downloads.push(download.read().await.to_api_response());
        }

        axum::Json(serde_json::to_value(&Response { downloads }).unwrap())
    }
}

mod post {
    use crate::routes::{ApiError, GetState, api::servers::_server_::GetServer};
    use axum::http::StatusCode;
    use serde::{Deserialize, Serialize};
    use std::{path::PathBuf, sync::Arc};
    use tokio::sync::RwLock;
    use utoipa::ToSchema;

    #[derive(ToSchema, Deserialize)]
    pub struct Payload {
        #[serde(default, alias = "directory")]
        root: String,

        url: String,
        file_name: Option<String>,

        #[serde(default)]
        use_header: bool,
        #[serde(default)]
        foreground: bool,
    }

    #[derive(ToSchema, Serialize)]
    struct Response {
        identifier: uuid::Uuid,
    }

    #[utoipa::path(post, path = "/", responses(
        (status = OK, body = inline(Response)),
        (status = EXPECTATION_FAILED, body = inline(ApiError)),
    ), request_body = inline(Payload))]
    pub async fn route(
        state: GetState,
        server: GetServer,
        axum::Json(data): axum::Json<Payload>,
    ) -> (StatusCode, axum::Json<serde_json::Value>) {
        let path = match server.filesystem.canonicalize(&data.root).await {
            Ok(path) => path,
            Err(_) => PathBuf::from(data.root),
        };

        let metadata = server.filesystem.symlink_metadata(&path).await;
        if !metadata.map(|m| m.is_dir()).unwrap_or(true) {
            return (
                StatusCode::EXPECTATION_FAILED,
                axum::Json(ApiError::new("root is not a directory").to_json()),
            );
        }

        if let Some(file_name) = &data.file_name {
            let metadata = server.filesystem.metadata(path.join(file_name)).await;
            if !metadata.map(|m| m.is_file()).unwrap_or(true) {
                return (
                    StatusCode::EXPECTATION_FAILED,
                    axum::Json(ApiError::new("file is not a file").to_json()),
                );
            }
        } else if !data.use_header {
            return (
                StatusCode::EXPECTATION_FAILED,
                axum::Json(
                    ApiError::new("file name is required when not using use_header").to_json(),
                ),
            );
        }

        if state.config.api.disable_remote_download {
            return (
                StatusCode::EXPECTATION_FAILED,
                axum::Json(ApiError::new("remote download is disabled").to_json()),
            );
        }

        if server.filesystem.pulls().await.len() >= 3 {
            return (
                StatusCode::EXPECTATION_FAILED,
                axum::Json(ApiError::new("too many concurrent downloads").to_json()),
            );
        }

        server.filesystem.create_dir_all(&path).await.unwrap();
        let download = Arc::new(RwLock::new(
            crate::server::filesystem::pull::Download::new(
                server.0.clone(),
                &path,
                data.file_name,
                data.url,
                data.use_header,
            )
            .await
            .unwrap(),
        ));

        let identifier = download.read().await.identifier;

        server
            .filesystem
            .pulls
            .write()
            .await
            .insert(identifier, Arc::clone(&download));

        download.write().await.start();

        if data.foreground {
            while download
                .read()
                .await
                .task
                .as_ref()
                .is_some_and(|t| !t.is_finished())
            {
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
        }

        (
            StatusCode::OK,
            axum::Json(serde_json::to_value(&Response { identifier }).unwrap()),
        )
    }
}

pub fn router(state: &State) -> OpenApiRouter<State> {
    OpenApiRouter::new()
        .nest("/{pull}", _pull_::router(state))
        .routes(routes!(get::route))
        .routes(routes!(post::route))
        .with_state(state.clone())
}
