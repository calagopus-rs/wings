use super::State;
use utoipa_axum::{router::OpenApiRouter, routes};

mod post {
    use crate::{
        response::{ApiResponse, ApiResponseResult},
        routes::{ApiError, api::servers::_server_::GetServer},
    };
    use axum::http::StatusCode;
    use cap_std::fs::{Permissions, PermissionsExt};
    use serde::{Deserialize, Serialize};
    use utoipa::ToSchema;

    #[derive(ToSchema, Deserialize)]
    pub struct ChmodFile {
        file: String,
        mode: String,
    }

    #[derive(ToSchema, Deserialize)]
    pub struct Payload {
        #[serde(default)]
        root: String,

        #[schema(inline)]
        files: Vec<ChmodFile>,
    }

    #[derive(ToSchema, Serialize)]
    struct Response {
        pub updated: usize,
    }

    #[utoipa::path(post, path = "/", responses(
        (status = OK, body = inline(Response)),
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
        let root = match server.filesystem.async_canonicalize(data.root).await {
            Ok(path) => path,
            Err(_) => {
                return ApiResponse::error("root not found")
                    .with_status(StatusCode::NOT_FOUND)
                    .ok();
            }
        };

        let metadata = server.filesystem.async_symlink_metadata(&root).await;
        if !metadata.map(|m| m.is_dir()).unwrap_or(true) {
            return ApiResponse::error("root is not a directory")
                .with_status(StatusCode::EXPECTATION_FAILED)
                .ok();
        }

        let mut updated_count = 0;
        for file in data.files {
            let source = root.join(file.file);
            if source == root {
                continue;
            }

            let metadata = match server.filesystem.async_symlink_metadata(&source).await {
                Ok(metadata) => metadata,
                Err(_) => continue,
            };

            if server
                .filesystem
                .is_ignored(&source, metadata.is_dir())
                .await
            {
                continue;
            }

            let mode = match u32::from_str_radix(&file.mode, 8) {
                Ok(mode) => mode,
                Err(_) => continue,
            };

            if server
                .filesystem
                .async_set_permissions(&source, Permissions::from_mode(mode))
                .await
                .is_ok()
            {
                updated_count += 1;
            }
        }

        ApiResponse::json(Response {
            updated: updated_count,
        })
        .ok()
    }
}

pub fn router(state: &State) -> OpenApiRouter<State> {
    OpenApiRouter::new()
        .routes(routes!(post::route))
        .with_state(state.clone())
}
