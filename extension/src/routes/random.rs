use utoipa_axum::{router::OpenApiRouter, routes};
use wings_rs::routes::State;

mod get {
    use axum::{extract::Query, http::StatusCode};
    use rand::Rng;
    use serde::Deserialize;
    use std::path::PathBuf;
    use utoipa::ToSchema;
    use wings_rs::routes::{ApiError, GetState, api::servers::_server_::GetServer};

    #[derive(ToSchema, Deserialize)]
    pub struct Params {
        #[serde(default)]
        pub directory: String,
    }

    #[utoipa::path(get, path = "/api/servers/{server}/files/random", responses(
        (status = OK, body = wings_rs::models::DirectoryEntry),
        (status = EXPECTATION_FAILED, body = ApiError),
    ), params(
        (
            "server" = uuid::Uuid,
            description = "The server uuid",
            example = "123e4567-e89b-12d3-a456-426614174000",
        ),
        (
            "directory" = String, Query,
            description = "The directory to get a random file from",
        ),
    ))]
    pub async fn route(
        state: GetState,
        server: GetServer,
        Query(data): Query<Params>,
    ) -> (StatusCode, axum::Json<serde_json::Value>) {
        let mut entries = Vec::new();

        let path = match server.filesystem.async_canonicalize(&data.directory).await {
            Ok(path) => path,
            Err(_) => PathBuf::from(data.directory),
        };

        let metadata = server.filesystem.async_metadata(&path).await;
        if let Ok(metadata) = metadata {
            if !metadata.is_dir() || server.filesystem.is_ignored(&path, metadata.is_dir()).await {
                return (
                    StatusCode::EXPECTATION_FAILED,
                    axum::Json(ApiError::new("path not a directory").to_json()),
                );
            }
        } else {
            return (
                StatusCode::NOT_FOUND,
                axum::Json(ApiError::new("path not found").to_json()),
            );
        }

        let mut directory = server.filesystem.async_read_dir(&path).await.unwrap();
        while let Some(Ok((_, entry))) = directory.next_entry().await {
            let path = path.join(entry);
            let metadata = match server.filesystem.async_symlink_metadata(&path).await {
                Ok(metadata) => metadata,
                Err(_) => continue,
            };

            if server.filesystem.is_ignored(&path, metadata.is_dir()).await {
                continue;
            }

            entries.push(server.filesystem.to_api_entry(path, metadata).await);

            if entries.len() >= state.config.api.directory_entry_limit {
                break;
            }
        }

        if entries.is_empty() {
            return (
                StatusCode::EXPECTATION_FAILED,
                axum::Json(ApiError::new("no files found in directory").to_json()),
            );
        }

        let entry = entries.get(rand::rng().random_range(0..entries.len()));

        if let Some(entry) = entry {
            (
                StatusCode::OK,
                axum::Json(serde_json::to_value(entry).unwrap()),
            )
        } else {
            (
                StatusCode::EXPECTATION_FAILED,
                axum::Json(ApiError::new("no files found in directory").to_json()),
            )
        }
    }
}

pub fn router(state: &State) -> OpenApiRouter<State> {
    OpenApiRouter::new()
        .routes(routes!(get::route))
        .with_state(state.clone())
}
