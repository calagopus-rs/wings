use super::State;
use utoipa_axum::{router::OpenApiRouter, routes};

mod put {
    use crate::{
        response::{ApiResponse, ApiResponseResult},
        routes::{ApiError, api::servers::_server_::GetServer},
    };
    use axum::http::StatusCode;
    use serde::{Deserialize, Serialize};
    use std::path::Path;
    use utoipa::ToSchema;

    #[derive(ToSchema, Deserialize)]
    pub struct RenameFile {
        from: String,
        to: String,
    }

    #[derive(ToSchema, Deserialize)]
    pub struct Payload {
        #[serde(default)]
        root: String,

        #[schema(inline)]
        files: Vec<RenameFile>,
    }

    #[derive(ToSchema, Serialize)]
    struct Response {
        renamed: usize,
    }

    #[utoipa::path(put, path = "/", responses(
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
        server: GetServer,
        axum::Json(data): axum::Json<Payload>,
    ) -> ApiResponseResult {
        let root = Path::new(&data.root);

        let metadata = server.filesystem.async_metadata(&root).await;
        if !metadata.map(|m| m.is_dir()).unwrap_or(true) {
            return ApiResponse::error("root is not a directory")
                .with_status(StatusCode::EXPECTATION_FAILED)
                .ok();
        }

        let mut renamed_count = 0;
        for file in data.files {
            let from = root.join(file.from);
            if from == root {
                continue;
            }

            let to = root.join(file.to);
            if to == root {
                continue;
            }

            if from == to {
                continue;
            }

            let from_metadata = match server.filesystem.async_metadata(&from).await {
                Ok(metadata) => metadata,
                Err(_) => continue,
            };

            if server.filesystem.async_metadata(&to).await.is_ok()
                || server
                    .filesystem
                    .is_ignored(&from, from_metadata.is_dir())
                    .await
                || server
                    .filesystem
                    .is_ignored(&to, from_metadata.is_dir())
                    .await
            {
                continue;
            }

            if let Err(err) = server.filesystem.rename_path(from, to).await {
                tracing::debug!(
                    server = %server.uuid,
                    "failed to rename file: {:#?}",
                    err
                );
            } else {
                renamed_count += 1;
            }
        }

        ApiResponse::json(Response {
            renamed: renamed_count,
        })
        .ok()
    }
}

pub fn router(state: &State) -> OpenApiRouter<State> {
    OpenApiRouter::new()
        .routes(routes!(put::route))
        .with_state(state.clone())
}
