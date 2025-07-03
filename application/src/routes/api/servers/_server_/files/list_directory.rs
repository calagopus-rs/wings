use super::State;
use utoipa_axum::{router::OpenApiRouter, routes};

mod get {
    use crate::routes::{ApiError, GetState, api::servers::_server_::GetServer};
    use axum::{extract::Query, http::StatusCode};
    use serde::Deserialize;
    use std::path::PathBuf;
    use utoipa::ToSchema;

    #[derive(ToSchema, Deserialize)]
    pub struct Params {
        #[serde(default)]
        pub directory: String,

        pub per_page: Option<usize>,
        pub page: Option<usize>,
    }

    #[utoipa::path(get, path = "/", responses(
        (status = OK, body = Vec<crate::models::DirectoryEntry>),
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
            let mut entries = match crate::server::filesystem::backup::list(
                backup, &server, &path, per_page, page,
            )
            .await
            {
                Ok(entries) => entries,
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

            entries.sort_by(|a, b| {
                if a.directory && !b.directory {
                    std::cmp::Ordering::Less
                } else if !a.directory && b.directory {
                    std::cmp::Ordering::Greater
                } else {
                    a.name.cmp(&b.name)
                }
            });

            return (
                StatusCode::OK,
                axum::Json(serde_json::to_value(&entries).unwrap()),
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

        let mut entries = Vec::new();
        let mut matched_entries = 0;

        while let Some(Ok(entry)) = directory.next_entry().await {
            let path = path.join(entry);
            let metadata = match server.filesystem.symlink_metadata(&path).await {
                Ok(metadata) => metadata,
                Err(_) => continue,
            };

            if server.filesystem.is_ignored(&path, metadata.is_dir()).await {
                continue;
            }

            matched_entries += 1;
            if let Some(per_page) = per_page {
                if matched_entries <= (page - 1) * per_page {
                    continue;
                }
            }

            entries.push(server.filesystem.to_api_entry(path, metadata).await);

            if let Some(per_page) = per_page {
                if entries.len() >= per_page {
                    break;
                }
            }
        }

        entries.sort_by(|a, b| {
            if a.directory && !b.directory {
                std::cmp::Ordering::Less
            } else if !a.directory && b.directory {
                std::cmp::Ordering::Greater
            } else {
                a.name.cmp(&b.name)
            }
        });

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
