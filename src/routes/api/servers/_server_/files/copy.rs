use super::State;
use utoipa_axum::{router::OpenApiRouter, routes};

mod post {
    use crate::routes::{ApiError, api::servers::_server_::GetServer};
    use axum::http::StatusCode;
    use serde::Deserialize;
    use tokio::io::AsyncWriteExt;
    use utoipa::ToSchema;

    #[derive(ToSchema, Deserialize)]
    pub struct Payload {
        location: String,
    }

    #[utoipa::path(post, path = "/", responses(
        (status = OK, body = crate::models::DirectoryEntry),
        (status = NOT_FOUND, body = inline(ApiError)),
        (status = EXPECTATION_FAILED, body = inline(ApiError)),
    ), request_body = inline(Payload))]
    pub async fn route(
        server: GetServer,
        axum::Json(data): axum::Json<Payload>,
    ) -> (StatusCode, axum::Json<serde_json::Value>) {
        let location = match server.filesystem.safe_path(&data.location).await {
            Some(path) => path,
            None => {
                return (
                    StatusCode::NOT_FOUND,
                    axum::Json(ApiError::new("file not found").to_json()),
                );
            }
        };

        let metadata = match tokio::fs::symlink_metadata(&location).await {
            Ok(metadata) => {
                if !metadata.is_file()
                    || server
                        .filesystem
                        .is_ignored(&location, metadata.is_dir())
                        .await
                {
                    return (
                        StatusCode::NOT_FOUND,
                        axum::Json(ApiError::new("file not found").to_json()),
                    );
                } else {
                    metadata
                }
            }
            Err(_) => {
                return (
                    StatusCode::NOT_FOUND,
                    axum::Json(ApiError::new("file not found").to_json()),
                );
            }
        };

        if !server
            .filesystem
            .allocate_in_path(location.parent().unwrap(), metadata.len() as i64)
            .await
        {
            return (
                StatusCode::EXPECTATION_FAILED,
                axum::Json(ApiError::new("failed to allocate space").to_json()),
            );
        }

        let mut extension = location
            .extension()
            .and_then(|ext| ext.to_str())
            .unwrap_or("")
            .to_string();
        let mut base_name = location
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("")
            .to_string();

        if base_name.ends_with(".tar") {
            extension = format!("tar.{}", extension);
            base_name = base_name.trim_end_matches(".tar").to_string();
        }

        #[inline]
        fn find_copy_suffix(
            location: &std::path::Path,
            base_name: &str,
            extension: &str,
        ) -> String {
            let parent = location.parent().unwrap_or(std::path::Path::new(""));
            let mut suffix = " copy".to_string();

            for i in 0..51 {
                if i > 0 {
                    suffix = format!(" copy {}", i);
                }

                let new_name = format!("{}{}.{}", base_name, suffix, extension);
                let new_path = parent.join(&new_name);

                if !new_path.exists() {
                    return new_name;
                }

                if i == 50 {
                    use chrono::prelude::*;
                    let timestamp = Utc::now().to_rfc3339();
                    suffix = format!("copy.{}", timestamp);
                    let final_name = format!("{}{}.{}", base_name, suffix, extension);
                    return final_name;
                }
            }

            format!("{}{}.{}", base_name, suffix, extension)
        }

        let new_name = find_copy_suffix(&location, &base_name, &extension);
        let file_name = location.parent().unwrap().join(&new_name);

        let mut file = tokio::fs::File::open(&location).await.unwrap();
        let mut new_file = tokio::fs::File::create(&file_name).await.unwrap();

        tokio::io::copy(&mut file, &mut new_file).await.unwrap();

        new_file.flush().await.unwrap();
        new_file.sync_all().await.unwrap();

        server.filesystem.chown_path(&file_name).await;

        (
            StatusCode::OK,
            axum::Json(
                serde_json::to_value(
                    server
                        .filesystem
                        .to_api_entry(file_name, new_file.metadata().await.unwrap())
                        .await,
                )
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
