use super::State;
use utoipa_axum::{router::OpenApiRouter, routes};

mod get {
    use crate::{
        response::{ApiResponse, ApiResponseResult},
        routes::{ApiError, GetState, api::servers::_server_::GetServer},
    };
    use axum::http::StatusCode;
    use axum_extra::extract::Query;
    use serde::{Deserialize, Serialize};
    use std::path::{Path, PathBuf};
    use utoipa::ToSchema;

    #[derive(ToSchema, Deserialize)]
    pub struct Params {
        #[serde(default)]
        pub directory: String,
        #[serde(default)]
        pub ignored: Vec<String>,

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
            "ignored" = Vec<String>, Query,
            description = "Additional ignored files",
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
    ) -> ApiResponseResult {
        let per_page = match data.per_page {
            Some(per_page) => Some(per_page),
            None => match state.config.api.directory_entry_limit {
                0 => None,
                limit => Some(limit),
            },
        };
        let page = data.page.unwrap_or(1);

        let path = match server.filesystem.async_canonicalize(&data.directory).await {
            Ok(path) => path,
            Err(_) => PathBuf::from(data.directory),
        };

        let overrides = if data.ignored.is_empty() {
            None
        } else {
            let mut override_builder = ignore::overrides::OverrideBuilder::new("/");

            for file in data.ignored {
                override_builder.add(&file).ok();
            }

            override_builder.build().ok()
        };

        let is_ignored = move |path: &Path, is_dir: bool| {
            if path == Path::new("/") || path == Path::new("") {
                return false;
            }

            overrides
                .as_ref()
                .map(|ig| ig.matched(path, is_dir).is_whitelist())
                .unwrap_or(false)
        };

        if let Some((backup, rel_path)) = server
            .filesystem
            .backup_fs(&server, &state.backup_manager, &path)
            .await
        {
            if is_ignored(&path, true) || server.filesystem.is_ignored(&path, true).await {
                return ApiResponse::error("path not a directory")
                    .with_status(StatusCode::EXPECTATION_FAILED)
                    .ok();
            }

            let (total, entries) = match backup
                .read_dir(rel_path, per_page, page, move |path, is_dir| {
                    is_ignored(&path, is_dir)
                })
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

                    return ApiResponse::error("failed to list backup directory")
                        .with_status(StatusCode::EXPECTATION_FAILED)
                        .ok();
                }
            };

            return ApiResponse::json(Response { total, entries }).ok();
        }

        let metadata = server.filesystem.async_metadata(&path).await;
        if let Ok(metadata) = metadata {
            if !metadata.is_dir()
                || is_ignored(&path, metadata.is_dir())
                || server.filesystem.is_ignored(&path, metadata.is_dir()).await
            {
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

        let mut directory_entries = Vec::new();
        let mut other_entries = Vec::new();

        while let Some(Ok((is_dir, entry))) = directory.next_entry().await {
            let path = path.join(&entry);

            if is_ignored(&path, is_dir) || server.filesystem.is_ignored(&path, is_dir).await {
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
                let metadata = match server.filesystem.async_symlink_metadata(&path).await {
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
                let metadata = match server.filesystem.async_symlink_metadata(&path).await {
                    Ok(metadata) => metadata,
                    Err(_) => continue,
                };

                entries.push(server.filesystem.to_api_entry(path, metadata).await);
            }
        }

        ApiResponse::json(Response {
            total: total_entries,
            entries,
        })
        .ok()
    }
}

pub fn router(state: &State) -> OpenApiRouter<State> {
    OpenApiRouter::new()
        .routes(routes!(get::route))
        .with_state(state.clone())
}
