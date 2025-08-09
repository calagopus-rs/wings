use super::State;
use utoipa_axum::{router::OpenApiRouter, routes};

mod post {
    use crate::{
        response::{ApiResponse, ApiResponseResult},
        routes::{ApiError, api::servers::_server_::GetServer},
    };
    use axum::http::StatusCode;
    use serde::Deserialize;
    use std::path::Path;
    use utoipa::ToSchema;

    #[derive(ToSchema, Deserialize)]
    pub struct Payload {
        location: String,
        name: Option<String>,
    }

    #[utoipa::path(post, path = "/", responses(
        (status = OK, body = crate::models::DirectoryEntry),
        (status = NOT_FOUND, body = ApiError),
        (status = EXPECTATION_FAILED, body = ApiError),
    ), params(
        (
            "server" = uuid::Uuid,
            description = "The server uuid",
            example = "123e4567-e89b-12d3-a456-426614174000",
        ),
    ), request_body = inline(Payload))]
    pub async fn route(
        server: GetServer,
        axum::Json(data): axum::Json<Payload>,
    ) -> ApiResponseResult {
        let location = match server.filesystem.async_canonicalize(data.location).await {
            Ok(path) => path,
            Err(_) => {
                return ApiResponse::error("file not found")
                    .with_status(StatusCode::NOT_FOUND)
                    .ok();
            }
        };

        let metadata = match server.filesystem.async_metadata(&location).await {
            Ok(metadata) => {
                if !metadata.is_file()
                    || server
                        .filesystem
                        .is_ignored(&location, metadata.is_dir())
                        .await
                {
                    return ApiResponse::error("file not found")
                        .with_status(StatusCode::NOT_FOUND)
                        .ok();
                } else {
                    metadata
                }
            }
            Err(_) => {
                return ApiResponse::error("file not found")
                    .with_status(StatusCode::NOT_FOUND)
                    .ok();
            }
        };

        #[inline]
        async fn generate_new_name(server: &GetServer, location: &Path) -> String {
            let mut extension = location
                .extension()
                .and_then(|ext| ext.to_str())
                .map(|ext| format!(".{ext}"))
                .unwrap_or("".to_string());
            let mut base_name = location
                .file_stem()
                .and_then(|stem| stem.to_str())
                .unwrap_or("")
                .to_string();

            if base_name.ends_with(".tar") {
                extension = format!(".tar{extension}");
                base_name.truncate(base_name.len() - 4);
            }

            let parent = location.parent().unwrap_or(Path::new(""));
            let mut suffix = " copy".to_string();

            for i in 0..51 {
                if i > 0 {
                    suffix = format!(" copy {i}");
                }

                let new_name = format!("{base_name}{suffix}{extension}");
                let new_path = parent.join(&new_name);

                if server
                    .filesystem
                    .async_symlink_metadata(&new_path)
                    .await
                    .is_err()
                {
                    return new_name;
                }

                if i == 50 {
                    let timestamp = chrono::Utc::now().to_rfc3339();
                    suffix = format!("copy.{timestamp}");

                    let final_name = format!("{base_name}{suffix}{extension}");
                    return final_name;
                }
            }

            format!("{base_name}{suffix}{extension}")
        }

        let parent = match location.parent() {
            Some(parent) => parent,
            None => {
                return ApiResponse::error("file has no parent")
                    .with_status(StatusCode::EXPECTATION_FAILED)
                    .ok();
            }
        };

        if server.filesystem.is_ignored(parent, true).await {
            return ApiResponse::error("parent directory not found")
                .with_status(StatusCode::EXPECTATION_FAILED)
                .ok();
        }

        let new_name = if let Some(name) = data.name {
            name
        } else {
            generate_new_name(&server, &location).await
        };
        let file_name = parent.join(&new_name);

        if !server
            .filesystem
            .async_allocate_in_path(parent, metadata.len() as i64, false)
            .await
        {
            return ApiResponse::error("failed to allocate space")
                .with_status(StatusCode::EXPECTATION_FAILED)
                .ok();
        }

        server
            .filesystem
            .async_copy(&location, &server.filesystem, &file_name)
            .await?;
        let metadata = server.filesystem.async_metadata(&file_name).await?;

        ApiResponse::json(server.filesystem.to_api_entry(file_name, metadata).await).ok()
    }
}

pub fn router(state: &State) -> OpenApiRouter<State> {
    OpenApiRouter::new()
        .routes(routes!(post::route))
        .with_state(state.clone())
}
