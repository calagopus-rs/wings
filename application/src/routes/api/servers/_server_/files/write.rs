use super::State;
use utoipa_axum::{router::OpenApiRouter, routes};

mod post {
    use crate::routes::{ApiError, api::servers::_server_::GetServer};
    use axum::{
        body::Body,
        extract::Query,
        http::{HeaderMap, StatusCode},
    };
    use futures_util::StreamExt;
    use serde::{Deserialize, Serialize};
    use std::path::PathBuf;
    use tokio::io::AsyncWriteExt;
    use utoipa::ToSchema;

    #[derive(ToSchema, Deserialize)]
    pub struct Params {
        file: String,
    }

    #[derive(ToSchema, Serialize)]
    struct Response {}

    #[utoipa::path(post, path = "/", responses(
        (status = OK, body = inline(Response)),
        (status = NOT_FOUND, body = inline(ApiError)),
        (status = EXPECTATION_FAILED, body = inline(ApiError)),
    ), request_body = String)]
    pub async fn route(
        server: GetServer,
        headers: HeaderMap,
        Query(data): Query<Params>,
        body: Body,
    ) -> (StatusCode, axum::Json<serde_json::Value>) {
        let path = match server.filesystem.canonicalize(&data.file).await {
            Ok(path) => path,
            Err(_) => PathBuf::from(data.file),
        };

        let content_size: i64 = headers
            .get("Content-Length")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        let metadata = server.filesystem.metadata(&path).await;

        if server
            .filesystem
            .is_ignored(
                &path,
                metadata.as_ref().map(|m| m.is_dir()).unwrap_or(false),
            )
            .await
        {
            return (
                StatusCode::NOT_FOUND,
                axum::Json(ApiError::new("file not found").to_json()),
            );
        }

        let old_content_size = if let Ok(metadata) = metadata {
            if !metadata.is_file() {
                return (
                    StatusCode::NOT_FOUND,
                    axum::Json(ApiError::new("file is not a file").to_json()),
                );
            }

            metadata.len() as i64
        } else {
            0
        };

        let parent = path.parent().unwrap();
        server.filesystem.create_dir_all(parent).await.unwrap();

        if !server
            .filesystem
            .allocate_in_path(parent, content_size - old_content_size)
            .await
        {
            return (
                StatusCode::EXPECTATION_FAILED,
                axum::Json(ApiError::new("failed to allocate space").to_json()),
            );
        }

        let mut file = server.filesystem.create(&path).await.unwrap();
        let mut stream = body.into_data_stream();

        while let Some(Ok(chunk)) = stream.next().await {
            file.write_all(&chunk).await.unwrap();
        }

        file.flush().await.unwrap();
        file.sync_all().await.unwrap();

        server.filesystem.chown_path(&path).await;

        (
            StatusCode::OK,
            axum::Json(serde_json::to_value(&Response {}).unwrap()),
        )
    }
}

pub fn router(state: &State) -> OpenApiRouter<State> {
    OpenApiRouter::new()
        .routes(routes!(post::route))
        .with_state(state.clone())
}
