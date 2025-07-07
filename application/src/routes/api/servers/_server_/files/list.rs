use super::State;
use utoipa_axum::{router::OpenApiRouter, routes};

mod get {
    use crate::routes::{ApiError, GetState, api::servers::_server_::GetServer};
    use axum::{extract::Query, http::StatusCode};
    use serde::{Deserialize, Serialize};
    use std::path::PathBuf;
    use utoipa::ToSchema;

    #[derive(ToSchema, Deserialize)]
    pub struct Params {
        #[serde(default)]
        pub directory: String,

        pub per_page: Option<usize>,
        pub page: Option<usize>,
    }

    #[derive(ToSchema, Serialize)]
    struct Response {
        total: usize,
        entries: Vec<crate::models::DirectoryEntry>,
    }

    #[utoipa::path(get, path = "/", responses(
        (status = OK, body = inline(Response)),
        (status = NOT_FOUND, body = ApiError),
        (status = EXPECTATION_FAILED, body = ApiError),
    ), params(
        (
            "server" = uuid::Uuid,
            description = "The server uuid",
            example = "123e4567-e89b-12d3-a456-426614174000",
        ),
        (
            "directory" = String, Query,
            description = "The directory to list files from",
        ),
        (
            "per_page" = usize, Query,
            description = "The number of entries to return per page",
        ),
        (
            "page" = usize, Query,
            description = "The page number to return",
        ),
    ))]
    pub async fn route(
        state: GetState,
        server: GetServer,
        Query(data): Query<Params>,
    ) -> (StatusCode, axum::Json<serde_json::Value>) {
        let per_page = match data.per_page {
            Some(per_page) => Some(per_page),
            None => match state.config.api.directory_entry_limit {
                0 => None,
                limit => Some(limit),
            },
        };
        let page = data.page.unwrap_or(1);

        let path = match server.filesystem.canonicalize(&data.directory).await {
            Ok(path) => path,
            Err(_) => PathBuf::from(data.directory),
        };

        if let Some((backup, path)) = server.filesystem.backup_fs(&server, &path).await {
            let (total, entries) = match crate::server::filesystem::backup::list(
                backup, &server, &path, per_page, page,
            )
            .await
            {
                Ok((total, entries)) => (total, entries),
                Err(err) => {
                    tracing::error!(
                        server = %server.uuid,
                        path = %path.display(),
                        error = %err,
                        "failed to list backup directory",
                    );

                    return (
                        StatusCode::EXPECTATION_FAILED,
                        axum::Json(ApiError::new("failed to list backup directory").to_json()),
                    );
                }
            };

            return (
                StatusCode::OK,
                axum::Json(serde_json::to_value(Response { total, entries }).unwrap()),
            );
        }

        let metadata = server.filesystem.metadata(&path).await;
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

        let mut directory = server.filesystem.read_dir(&path).await.unwrap();

        let mut directory_entries = Vec::new();
        let mut other_entries = Vec::new();

        while let Some(Ok((is_dir, entry))) = directory.next_entry().await {
            let path = path.join(&entry);

            if server.filesystem.is_ignored(&path, is_dir).await {
                continue;
            }

            if is_dir {
                directory_entries.push(entry);
            } else {
                other_entries.push(entry);
            }
        }

        directory_entries.sort_unstable();
        other_entries.sort_unstable();

        let total_entries = directory_entries.len() + other_entries.len();
        let mut entries = Vec::new();

        if let Some(per_page) = per_page {
            let start = (page - 1) * per_page;

            for entry in directory_entries
                .into_iter()
                .chain(other_entries.into_iter())
                .skip(start)
                .take(per_page)
            {
                let path = path.join(&entry);
                let metadata = match server.filesystem.symlink_metadata(&path).await {
                    Ok(metadata) => metadata,
                    Err(_) => continue,
                };

                entries.push(server.filesystem.to_api_entry(path, metadata).await);
            }
        } else {
            for entry in directory_entries
                .into_iter()
                .chain(other_entries.into_iter())
            {
                let path = path.join(&entry);
                let metadata = match server.filesystem.symlink_metadata(&path).await {
                    Ok(metadata) => metadata,
                    Err(_) => continue,
                };

                entries.push(server.filesystem.to_api_entry(path, metadata).await);
            }
        }

        (
            StatusCode::OK,
            axum::Json(
                serde_json::to_value(Response {
                    total: total_entries,
                    entries,
                })
                .unwrap(),
            ),
        )
    }
}

pub fn router(state: &State) -> OpenApiRouter<State> {
    OpenApiRouter::new()
        .routes(routes!(get::route))
        .with_state(state.clone())
}
