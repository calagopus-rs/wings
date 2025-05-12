use super::State;
use utoipa_axum::{router::OpenApiRouter, routes};

mod get {
    use crate::routes::{ApiError, api::servers::_server_::GetServer};
    use axum::{extract::Query, http::StatusCode};
    use serde::Deserialize;
    use utoipa::ToSchema;

    #[derive(ToSchema, Deserialize)]
    pub struct Params {
        #[serde(default)]
        pub directory: String,
    }

    #[utoipa::path(get, path = "/", responses(
        (status = OK, body = Vec<crate::models::DirectoryEntry>),
        (status = NOT_FOUND, body = inline(ApiError)),
        (status = EXPECTATION_FAILED, body = inline(ApiError)),
    ), params(
        (
            "directory" = String, Query,
            description = "The directory to list files from",
        ),
    ))]
    pub async fn route(
        server: GetServer,
        Query(data): Query<Params>,
    ) -> (StatusCode, axum::Json<serde_json::Value>) {
        let mut entries = Vec::new();

        let path = match server.filesystem.safe_path(&data.directory) {
            Some(path) => path,
            None => {
                return (
                    StatusCode::NOT_FOUND,
                    axum::Json(ApiError::new("path not found").to_json()),
                );
            }
        };

        if !path.symlink_metadata().unwrap().is_dir() {
            return (
                StatusCode::EXPECTATION_FAILED,
                axum::Json(ApiError::new("path is not a directory").to_json()),
            );
        }

        let mut directory = tokio::fs::read_dir(path).await.unwrap();
        while let Ok(Some(entry)) = directory.next_entry().await {
            entries.push(
                server
                    .filesystem
                    .to_api_entry(entry.path(), entry.metadata().await.unwrap())
                    .await,
            );
        }

        (
            StatusCode::OK,
            axum::Json(serde_json::to_value(&entries).unwrap()),
        )
    }
}

pub fn router(state: &State) -> OpenApiRouter<State> {
    OpenApiRouter::new()
        .routes(routes!(get::route))
        .with_state(state.clone())
}
