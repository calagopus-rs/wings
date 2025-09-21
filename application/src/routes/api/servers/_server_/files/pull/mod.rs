use super::State;
use utoipa_axum::{router::OpenApiRouter, routes};

mod _pull_;

mod get {
    use crate::{
        response::{ApiResponse, ApiResponseResult},
        routes::api::servers::_server_::GetServer,
    };
    use serde::Serialize;
    use utoipa::ToSchema;

    #[derive(ToSchema, Serialize)]
    struct Response {
        downloads: Vec<crate::models::Download>,
    }

    #[utoipa::path(get, path = "/", responses(
        (status = OK, body = inline(Response)),
    ), params(
        (
            "server" = uuid::Uuid,
            description = "The server uuid",
            example = "123e4567-e89b-12d3-a456-426614174000",
        ),
    ))]
    #[deprecated(
        note = "This endpoint is purely for pterodactyl compatibility. Use the operations system instead."
    )]
    pub async fn route(server: GetServer) -> ApiResponseResult {
        let values = server.filesystem.pulls().await;
        let mut downloads = Vec::new();
        downloads.reserve_exact(values.len());

        for download in values.values() {
            downloads.push(download.read().await.to_api_response());
        }

        ApiResponse::json(Response { downloads }).ok()
    }
}

mod post {
    use crate::{
        response::{ApiResponse, ApiResponseResult},
        routes::{ApiError, GetState, api::servers::_server_::GetServer},
    };
    use axum::http::StatusCode;
    use serde::{Deserialize, Serialize};
    use std::{path::PathBuf, sync::Arc};
    use tokio::sync::RwLock;
    use utoipa::ToSchema;

    #[derive(ToSchema, Deserialize)]
    pub struct Payload {
        #[serde(default, alias = "directory")]
        root: String,

        url: String,
        file_name: Option<String>,

        #[serde(default)]
        use_header: bool,
        #[serde(default)]
        foreground: bool,
    }

    #[derive(ToSchema, Serialize)]
    struct Response {}

    #[derive(ToSchema, Serialize)]
    struct ResponsePending {
        identifier: uuid::Uuid,
    }

    #[utoipa::path(post, path = "/", responses(
        (status = OK, body = inline(Response)),
        (status = ACCEPTED, body = inline(ResponsePending)),
        (status = EXPECTATION_FAILED, body = ApiError),
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
        let path = match server.filesystem.async_canonicalize(&data.root).await {
            Ok(path) => path,
            Err(_) => PathBuf::from(data.root),
        };

        let metadata = server.filesystem.async_symlink_metadata(&path).await;
        if !metadata.map(|m| m.is_dir()).unwrap_or(true) {
            return ApiResponse::error("root is not a directory")
                .with_status(StatusCode::EXPECTATION_FAILED)
                .ok();
        }

        if let Some(file_name) = &data.file_name {
            let metadata = server.filesystem.async_metadata(path.join(file_name)).await;
            if !metadata.map(|m| m.is_file()).unwrap_or(true) {
                return ApiResponse::error("file is not a file")
                    .with_status(StatusCode::EXPECTATION_FAILED)
                    .ok();
            }
        }

        if state.config.api.disable_remote_download {
            return ApiResponse::error("remote pulling is disabled")
                .with_status(StatusCode::EXPECTATION_FAILED)
                .ok();
        }

        if server.filesystem.pulls().await.len() >= state.config.api.server_remote_download_limit {
            return ApiResponse::error("too many concurrent pulls")
                .with_status(StatusCode::EXPECTATION_FAILED)
                .ok();
        }

        server.filesystem.async_create_dir_all(&path).await?;
        let download = Arc::new(RwLock::new(
            match crate::server::filesystem::pull::Download::new(
                server.0.clone(),
                &path,
                data.file_name,
                data.url,
                data.use_header,
            )
            .await
            {
                Ok(download) => download,
                Err(err) => {
                    tracing::error!("failed to create pull: {:#?}", err);

                    return ApiResponse::error(&format!("failed to create pull: {err}"))
                        .with_status(StatusCode::EXPECTATION_FAILED)
                        .ok();
                }
            },
        ));

        let (identifier, task) = download.write().await.start().await;
        server
            .filesystem
            .pulls
            .write()
            .await
            .insert(identifier, Arc::clone(&download));

        if data.foreground {
            match task.await {
                Ok(Some(Ok(_))) => {}
                Ok(Some(Err(err))) => {
                    tracing::error!(
                        server = %server.uuid,
                        "failed to pull file: {:#?}",
                        err,
                    );

                    return ApiResponse::error(&err.to_string())
                        .with_status(StatusCode::EXPECTATION_FAILED)
                        .ok();
                }
                Ok(None) => {
                    return ApiResponse::error("pull aborted by another source")
                        .with_status(StatusCode::EXPECTATION_FAILED)
                        .ok();
                }
                Err(err) => {
                    tracing::error!(
                        server = %server.uuid,
                        "failed to pull file: {:#?}",
                        err,
                    );

                    return ApiResponse::error("failed to pull file")
                        .with_status(StatusCode::EXPECTATION_FAILED)
                        .ok();
                }
            }

            ApiResponse::json(Response {}).ok()
        } else {
            ApiResponse::json(ResponsePending { identifier })
                .with_status(StatusCode::ACCEPTED)
                .ok()
        }
    }
}

#[allow(deprecated)]
pub fn router(state: &State) -> OpenApiRouter<State> {
    OpenApiRouter::new()
        .nest("/{pull}", _pull_::router(state))
        .routes(routes!(get::route))
        .routes(routes!(post::route))
        .with_state(state.clone())
}
