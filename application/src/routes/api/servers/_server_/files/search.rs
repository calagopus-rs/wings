use super::State;
use utoipa_axum::{router::OpenApiRouter, routes};

mod post {
    use crate::{
        response::{ApiResponse, ApiResponseResult},
        routes::{ApiError, GetState, api::servers::_server_::GetServer},
    };
    use axum::http::StatusCode;
    use serde::{Deserialize, Serialize};
    use std::{path::PathBuf, sync::Arc};
    use tokio::{io::AsyncReadExt, sync::RwLock};
    use utoipa::ToSchema;

    #[derive(ToSchema, Deserialize)]
    pub struct Payload {
        #[serde(default)]
        root: String,
        query: String,
        #[serde(default)]
        include_content: bool,

        limit: Option<usize>,
        max_size: Option<u64>,
    }

    #[derive(ToSchema, Serialize)]
    struct Response<'a> {
        results: &'a [crate::models::DirectoryEntry],
    }

    #[utoipa::path(post, path = "/", responses(
        (status = OK, body = inline(Response)),
        (status = NOT_FOUND, body = ApiError),
    ), params(
        (
            "server" = uuid::Uuid,
            description = "The server uuid",
            example = "123e4567-e89b-12d3-a456-426614174000",
        ),
    ), request_body = inline(Payload))]
    pub async fn route(
        state: GetState,
        server: GetServer,
        axum::Json(data): axum::Json<Payload>,
    ) -> ApiResponseResult {
        let limit = data.limit.unwrap_or(100).min(500);
        let max_size = data.max_size.unwrap_or(512 * 1024);

        let root = match server
            .filesystem
            .async_canonicalize(PathBuf::from(data.root))
            .await
        {
            Ok(path) => path,
            Err(_) => {
                return ApiResponse::error("root not found")
                    .with_status(StatusCode::NOT_FOUND)
                    .ok();
            }
        };

        let metadata = server.filesystem.async_metadata(&root).await;
        if !metadata.map(|m| m.is_dir()).unwrap_or(true) {
            return ApiResponse::error("root is not a directory")
                .with_status(StatusCode::NOT_FOUND)
                .ok();
        }

        let results = Arc::new(RwLock::new(Vec::new()));

        let ignored = &[server.filesystem.get_ignored().await];
        let mut walker = server
            .filesystem
            .async_walk_dir(&root)
            .await?
            .with_ignored(ignored);

        walker
            .run_multithreaded(
                state.config.api.file_search_threads,
                Arc::new({
                    let server = server.clone();
                    let results = Arc::clone(&results);
                    let query = Arc::new(data.query.clone());
                    let root = Arc::new(root.clone());

                    move |is_dir, path: PathBuf| {
                        let server = server.clone();
                        let results = Arc::clone(&results);
                        let query = Arc::clone(&query);
                        let root = Arc::clone(&root);
                        let mut buffer = vec![0; crate::BUFFER_SIZE];

                        async move {
                            if is_dir || results.read().await.len() >= limit {
                                return Ok(());
                            }

                            let metadata =
                                match server.filesystem.async_symlink_metadata(&path).await {
                                    Ok(metadata) => metadata,
                                    Err(_) => return Ok(()),
                                };

                            if !metadata.is_file() {
                                return Ok(());
                            }

                            if path.to_string_lossy().contains(query.as_ref()) {
                                let mut entry = server
                                    .filesystem
                                    .to_api_entry(path.to_path_buf(), metadata)
                                    .await;
                                entry.name = match path.strip_prefix(root.as_ref()) {
                                    Ok(path) => path.to_string_lossy().to_string(),
                                    Err(_) => return Ok(()),
                                };

                                results.write().await.push(entry);
                                return Ok(());
                            }

                            if data.include_content && metadata.len() <= max_size {
                                let mut file = match server.filesystem.async_open(&path).await {
                                    Ok(file) => file,
                                    Err(_) => return Ok(()),
                                };
                                let mut bytes_read = match file.read(&mut buffer).await {
                                    Ok(bytes_read) => bytes_read,
                                    Err(_) => return Ok(()),
                                };

                                if !crate::is_valid_utf8_slice(&buffer[..bytes_read.min(128)]) {
                                    return Ok(());
                                }

                                let mut last_content = String::with_capacity(8192 * 2);
                                loop {
                                    let content = String::from_utf8_lossy(&buffer[..bytes_read]);
                                    last_content.push_str(&content);

                                    if last_content.contains(query.as_ref()) {
                                        let mut entry = server
                                            .filesystem
                                            .to_api_entry_buffer(
                                                path.to_path_buf(),
                                                &metadata,
                                                false,
                                                Some(&buffer[..bytes_read]),
                                                None,
                                                None,
                                            )
                                            .await;
                                        entry.name = match path.strip_prefix(root.as_ref()) {
                                            Ok(path) => path.to_string_lossy().to_string(),
                                            Err(_) => return Ok(()),
                                        };

                                        results.write().await.push(entry);
                                        break;
                                    }

                                    last_content.clear();
                                    last_content.push_str(&content);

                                    bytes_read = match file.read(&mut buffer).await {
                                        Ok(bytes_read) => bytes_read,
                                        Err(_) => break,
                                    };

                                    if bytes_read == 0 {
                                        break;
                                    }
                                }
                            }

                            Ok(())
                        }
                    }
                }),
            )
            .await?;

        ApiResponse::json(Response {
            results: &results.read().await,
        })
        .ok()
    }
}

pub fn router(state: &State) -> OpenApiRouter<State> {
    OpenApiRouter::new()
        .routes(routes!(post::route))
        .with_state(state.clone())
}
