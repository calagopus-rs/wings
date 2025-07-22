use super::State;
use utoipa_axum::{router::OpenApiRouter, routes};

mod post {
    use crate::routes::{ApiError, GetState, api::servers::_server_::GetServer};
    use axum::http::StatusCode;
    use ignore::{WalkBuilder, WalkState};
    use serde::{Deserialize, Serialize};
    use std::{
        fs::File,
        io::Read,
        path::PathBuf,
        sync::{Arc, Mutex},
    };
    use utoipa::ToSchema;

    #[derive(ToSchema, Deserialize)]
    pub struct Payload {
        #[serde(default)]
        pub root: String,
        pub query: String,
        #[serde(default)]
        pub include_content: bool,

        pub limit: Option<usize>,
        pub max_size: Option<u64>,
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
    ) -> (StatusCode, axum::Json<serde_json::Value>) {
        let limit = data.limit.unwrap_or(100).min(500);
        let max_size = data.max_size.unwrap_or(512 * 1024);

        let root = match server
            .filesystem
            .canonicalize(PathBuf::from(data.root))
            .await
        {
            Ok(path) => path,
            Err(_) => {
                return (
                    StatusCode::NOT_FOUND,
                    axum::Json(ApiError::new("root not found").to_json()),
                );
            }
        };

        let metadata = server.filesystem.metadata(&root).await;
        if !metadata.map(|m| m.is_dir()).unwrap_or(true) {
            return (
                StatusCode::EXPECTATION_FAILED,
                axum::Json(ApiError::new("root is not a directory").to_json()),
            );
        }

        let results = Arc::new(Mutex::new(Vec::new()));
        let root = server.filesystem.base_path.join(&root);

        tokio::task::spawn_blocking({
            let results = Arc::clone(&results);
            let runtime = tokio::runtime::Handle::current();

            move || {
                WalkBuilder::new(&root)
                    .hidden(false)
                    .git_ignore(false)
                    .ignore(false)
                    .git_exclude(false)
                    .follow_links(false)
                    .threads(state.config.api.file_search_threads)
                    .build_parallel()
                    .run(move || {
                        let server = Arc::clone(&server);
                        let results = Arc::clone(&results);
                        let query = data.query.clone();
                        let root = root.clone();
                        let runtime = runtime.clone();

                        Box::new(move |entry| {
                            let entry = match entry {
                                Ok(entry) => entry,
                                Err(_) => return WalkState::Continue,
                            };
                            let path = entry.path();

                            let metadata = match entry.metadata() {
                                Ok(metadata) => metadata,
                                Err(_) => return WalkState::Continue,
                            };

                            if server.filesystem.is_ignored_sync(path, metadata.is_dir()) {
                                return WalkState::Continue;
                            }

                            if !metadata.is_file() {
                                return WalkState::Continue;
                            }

                            if path.to_str().is_some_and(|s| s.contains(&query)) {
                                let mut buffer = [0; 128];
                                let mut file = match File::open(path) {
                                    Ok(file) => file,
                                    Err(_) => return WalkState::Continue,
                                };
                                let bytes_read = match file.read(&mut buffer) {
                                    Ok(bytes_read) => bytes_read,
                                    Err(_) => return WalkState::Continue,
                                };

                                let mut results = results.lock().unwrap();
                                if results.len() >= limit {
                                    return WalkState::Quit;
                                }

                                let mut entry =
                                    runtime.block_on(server.filesystem.to_api_entry_buffer(
                                        path.to_path_buf(),
                                        &cap_std::fs::Metadata::from_just_metadata(metadata),
                                        Some(&buffer[..bytes_read]),
                                        None,
                                        None,
                                    ));
                                entry.name = match path.strip_prefix(&root) {
                                    Ok(path) => path.to_string_lossy().to_string(),
                                    Err(_) => return WalkState::Continue,
                                };

                                results.push(entry);
                                return WalkState::Continue;
                            }

                            if data.include_content && metadata.len() <= max_size {
                                let mut buffer = [0; 8192];
                                let mut file = match File::open(path) {
                                    Ok(file) => file,
                                    Err(_) => return WalkState::Continue,
                                };
                                let mut bytes_read = match file.read(&mut buffer) {
                                    Ok(bytes_read) => bytes_read,
                                    Err(_) => return WalkState::Continue,
                                };

                                if std::str::from_utf8(&buffer[..bytes_read.min(128)]).is_err() {
                                    return WalkState::Continue;
                                }

                                let mut last_content = String::with_capacity(8192 * 2);
                                loop {
                                    let content = String::from_utf8_lossy(&buffer[..bytes_read]);
                                    last_content.push_str(&content);

                                    if last_content.contains(&query) {
                                        let mut results = results.lock().unwrap();
                                        if results.len() >= limit {
                                            return WalkState::Quit;
                                        }

                                        let mut entry = runtime.block_on(
                                            server.filesystem.to_api_entry_buffer(
                                                path.to_path_buf(),
                                                &cap_std::fs::Metadata::from_just_metadata(
                                                    metadata,
                                                ),
                                                Some(&buffer[..bytes_read]),
                                                None,
                                                None,
                                            ),
                                        );
                                        entry.name = match path.strip_prefix(&root) {
                                            Ok(path) => path.to_string_lossy().to_string(),
                                            Err(_) => return WalkState::Continue,
                                        };

                                        results.push(entry);
                                        break;
                                    }

                                    last_content.clear();
                                    last_content.push_str(&content);

                                    bytes_read = match file.read(&mut buffer) {
                                        Ok(bytes_read) => bytes_read,
                                        Err(_) => break,
                                    };

                                    if bytes_read == 0 {
                                        break;
                                    }
                                }
                            }

                            WalkState::Continue
                        })
                    });
            }
        })
        .await
        .unwrap();

        (
            StatusCode::OK,
            axum::Json(
                serde_json::to_value(Response {
                    results: &results.lock().unwrap(),
                })
                .unwrap(),
            ),
        )
    }
}

pub fn router(state: &State) -> OpenApiRouter<State> {
    OpenApiRouter::new()
        .routes(routes!(post::route))
        .with_state(state.clone())
}
