use super::State;
use utoipa_axum::{router::OpenApiRouter, routes};

mod get {
    use crate::{
        response::{ApiResponse, ApiResponseResult},
        routes::{ApiError, GetState, api::servers::_server_::GetServer},
    };
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
    ) -> ApiResponseResult {
        let path = match server.filesystem.async_canonicalize(&data.directory).await {
            Ok(path) => path,
            Err(_) => PathBuf::from(data.directory),
        };

        if let Some((backup, path)) = server
            .filesystem
            .backup_fs(&server, &state.backup_manager, &path)
            .await
        {
            let mut entries = match backup
                .read_dir(
                    path.clone(),
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

                    return ApiResponse::error("failed to list backup directory")
                        .with_status(StatusCode::EXPECTATION_FAILED)
                        .ok();
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

            return ApiResponse::json(entries).ok();
        }

        let metadata = server.filesystem.async_metadata(&path).await;
        if let Ok(metadata) = metadata {
            if !metadata.is_dir() || server.filesystem.is_ignored(&path, metadata.is_dir()).await {
                return ApiResponse::error("path not a directory")
                    .with_status(StatusCode::EXPECTATION_FAILED)
                    .ok();
            }
        } else {
            return ApiResponse::error("path not found")
                .with_status(StatusCode::NOT_FOUND)
                .ok();
        }

        let mut directory = server.filesystem.async_read_dir(&path).await?;

        let mut entries = Vec::new();

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

        entries.sort_by(|a, b| {
            if a.directory && !b.directory {
                std::cmp::Ordering::Less
            } else if !a.directory && b.directory {
                std::cmp::Ordering::Greater
            } else {
                a.name.cmp(&b.name)
            }
        });

        ApiResponse::json(entries).ok()
    }
}

#[allow(deprecated)]
pub fn router(state: &State) -> OpenApiRouter<State> {
    OpenApiRouter::new()
        .routes(routes!(get::route))
        .with_state(state.clone())
}
