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
    #[deprecated(
        note = "This endpoint is purely for pterodactyl compatibility. Use `/files/list` instead."
    )]
    pub async fn route(
        state: GetState,
        server: GetServer,
        Query(data): Query<Params>,
    ) -> (StatusCode, axum::Json<serde_json::Value>) {
        let path = match server.filesystem.canonicalize(&data.directory).await {
            Ok(path) => path,
            Err(_) => PathBuf::from(data.directory),
        };

        if let Some((backup, path)) = server.filesystem.backup_fs(&server, &path).await {
            let mut entries = match crate::server::filesystem::backup::list(
                backup,
                &server,
                &path,
                Some(state.config.api.directory_entry_limit),
                1,
                |_, _| false,
            )
            .await
            {
                Ok((_, entries)) => entries,
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

        while let Some(Ok((_, entry))) = directory.next_entry().await {
            let path = path.join(entry);
            let metadata = match server.filesystem.symlink_metadata(&path).await {
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

#[allow(deprecated)]
pub fn router(state: &State) -> OpenApiRouter<State> {
    OpenApiRouter::new()
        .routes(routes!(get::route))
        .with_state(state.clone())
}
