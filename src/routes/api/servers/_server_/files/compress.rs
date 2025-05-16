use super::State;
use utoipa_axum::{router::OpenApiRouter, routes};

mod post {
    use crate::routes::{ApiError, GetState, api::servers::_server_::GetServer};
    use axum::http::StatusCode;
    use serde::Deserialize;
    use std::sync::Arc;
    use utoipa::ToSchema;

    #[derive(ToSchema, Deserialize)]
    pub struct Payload {
        #[serde(default)]
        pub root: String,

        pub files: Vec<String>,
    }

    #[utoipa::path(post, path = "/", responses(
        (status = OK, body = crate::models::DirectoryEntry),
        (status = NOT_FOUND, body = inline(ApiError)),
        (status = EXPECTATION_FAILED, body = inline(ApiError)),
    ), request_body = inline(Payload))]
    pub async fn route(
        state: GetState,
        server: GetServer,
        axum::Json(data): axum::Json<Payload>,
    ) -> (StatusCode, axum::Json<serde_json::Value>) {
        let root = match server.filesystem.safe_path(&data.root) {
            Some(path) => path,
            None => {
                return (
                    StatusCode::NOT_FOUND,
                    axum::Json(ApiError::new("root not found").to_json()),
                );
            }
        };

        let metadata = root.symlink_metadata();
        if !metadata.map(|m| m.is_dir()).unwrap_or(true) {
            return (
                StatusCode::EXPECTATION_FAILED,
                axum::Json(ApiError::new("root is not a directory").to_json()),
            );
        }

        let file_name = format!(
            "archive-{}.tar.gz",
            chrono::Local::now().format("%Y-%m-%dT%H%M%S%z")
        );
        let file_name = root.join(file_name);
        let writer = crate::server::filesystem::writer::FileSystemWriter::new(
            Arc::clone(&server.filesystem),
            file_name.clone(),
            None,
            None,
        )
        .unwrap();

        tokio::task::spawn_blocking({
            let server = Arc::clone(&server);

            move || {
                let mut archive = tar::Builder::new(flate2::write::GzEncoder::new(
                    writer,
                    flate2::Compression::new(state.config.system.backups.compression_level.into()),
                ));

                for file in data.files {
                    let source = match root.join(file).canonicalize() {
                        Ok(path) => path,
                        Err(_) => {
                            continue;
                        }
                    };

                    if !server.filesystem.is_safe_path(&source) {
                        continue;
                    }

                    let relative = match source.strip_prefix(&root) {
                        Ok(path) => path,
                        Err(_) => {
                            continue;
                        }
                    };

                    let source_metadata = source.symlink_metadata().unwrap();
                    if source_metadata.is_dir() {
                        archive.append_dir_all(relative, &source).unwrap();
                    } else {
                        archive.append_path_with_name(&source, relative).unwrap();
                    }
                }

                archive.finish().unwrap();
            }
        })
        .await
        .unwrap();

        let dir_entry = file_name.symlink_metadata().unwrap();

        (
            StatusCode::OK,
            axum::Json(
                serde_json::to_value(server.filesystem.to_api_entry(file_name, dir_entry).await)
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
