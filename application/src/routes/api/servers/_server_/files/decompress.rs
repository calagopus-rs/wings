use super::State;
use utoipa_axum::{router::OpenApiRouter, routes};

mod post {
    use crate::{
        response::{ApiResponse, ApiResponseResult},
        routes::{ApiError, api::servers::_server_::GetServer},
    };
    use axum::http::StatusCode;
    use serde::{Deserialize, Serialize};
    use utoipa::ToSchema;

    #[derive(ToSchema, Deserialize)]
    pub struct Payload {
        #[serde(default)]
        pub root: String,

        pub file: String,
    }

    #[derive(ToSchema, Serialize)]
    struct Response {}

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

        let metadata = server.filesystem.async_metadata(&root).await;
        if !metadata.map(|m| m.is_dir()).unwrap_or(true) {
            return ApiResponse::error("root is not a directory")
                .with_status(StatusCode::EXPECTATION_FAILED)
                .ok();
        }

        let source = root.join(data.file);

        if server
            .filesystem
            .is_ignored(
                &source,
                server
                    .filesystem
                    .async_metadata(&source)
                    .await
                    .is_ok_and(|m| m.is_dir()),
            )
            .await
        {
            return ApiResponse::error("file not found")
                .with_status(StatusCode::NOT_FOUND)
                .ok();
        }

        let archive =
            match crate::server::filesystem::archive::Archive::open(server.0.clone(), source).await
            {
                Some(archive) => archive,
                None => {
                    return ApiResponse::error("failed to open archive")
                        .with_status(StatusCode::EXPECTATION_FAILED)
                        .ok();
                }
            };

        match tokio::spawn(archive.extract(root.clone())).await {
            Ok(Ok(())) => {}
            Ok(Err(err)) => {
                tracing::error!(
                    server = %server.uuid,
                    root = %root.display(),
                    "failed to decompress archive: {:#?}",
                    err,
                );

                return ApiResponse::error(&format!("failed to decompress archive: {err}"))
                    .with_status(StatusCode::EXPECTATION_FAILED)
                    .ok();
            }
            Err(err) => {
                tracing::error!(
                    server = %server.uuid,
                    root = %root.display(),
                    "failed to decompress archive: {:#?}",
                    err,
                );

                return ApiResponse::error("failed to decompress archive")
                    .with_status(StatusCode::EXPECTATION_FAILED)
                    .ok();
            }
        }

        server.filesystem.chown_path(&root).await?;

        ApiResponse::json(Response {}).ok()
    }
}

pub fn router(state: &State) -> OpenApiRouter<State> {
    OpenApiRouter::new()
        .routes(routes!(post::route))
        .with_state(state.clone())
}
