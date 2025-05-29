use super::State;
use utoipa_axum::{router::OpenApiRouter, routes};

mod post {
    use crate::routes::{ApiError, api::servers::_server_::GetServer};
    use axum::http::StatusCode;
    use serde::Deserialize;
    use utoipa::ToSchema;

    #[derive(ToSchema, Deserialize)]
    pub struct Payload {
        location: String,
        name: Option<String>,
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

        let new_name = data.name.unwrap_or_else(|| {
            let mut extension = location
                .extension()
                .and_then(|ext| ext.to_str())
                .map(|ext| format!(".{}", ext))
                .unwrap_or("".to_string());
            let mut base_name = location
                .file_stem()
                .and_then(|stem| stem.to_str())
                .unwrap_or("")
                .to_string();

            if base_name.ends_with(".tar") {
                extension = format!(".tar{}", extension);
                base_name.truncate(base_name.len() - 4);
            }

            let parent = location.parent().unwrap_or(std::path::Path::new(""));
            let mut suffix = " copy".to_string();

            for i in 0..51 {
                if i > 0 {
                    suffix = format!(" copy {}", i);
                }

                let new_name = format!("{}{}{}", base_name, suffix, extension);
                let new_path = parent.join(&new_name);

                if !new_path.exists() {
                    return new_name;
                }

                if i == 50 {
                    let timestamp = chrono::Utc::now().to_rfc3339();
                    suffix = format!("copy.{}", timestamp);

                    let final_name = format!("{}{}{}", base_name, suffix, extension);
                    return final_name;
                }
            }

            format!("{}{}{}", base_name, suffix, extension)
        });
        let file_name = location.parent().unwrap().join(&new_name);

        if !server.filesystem.is_safe_path(&file_name).await {
            return (
                StatusCode::EXPECTATION_FAILED,
                axum::Json(ApiError::new("invalid file name").to_json()),
            );
        }

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

        tokio::fs::copy(&location, &file_name).await.unwrap();
        let metadata = tokio::fs::symlink_metadata(&file_name).await.unwrap();

        (
            StatusCode::OK,
            axum::Json(
                serde_json::to_value(server.filesystem.to_api_entry(file_name, metadata).await)
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
