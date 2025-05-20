use super::State;
use utoipa_axum::{router::OpenApiRouter, routes};

mod post {
    use crate::{
        routes::{ApiError, GetState},
        server::activity::{Activity, ActivityEvent},
    };
    use axum::{
        extract::{ConnectInfo, Multipart, Query},
        http::{HeaderMap, StatusCode},
    };
    use serde::{Deserialize, Serialize};
    use serde_json::json;
    use std::{net::SocketAddr, sync::Arc};
    use tokio::io::AsyncWriteExt;
    use utoipa::ToSchema;

    #[derive(ToSchema, Deserialize)]
    pub struct Params {
        token: String,
        #[serde(default)]
        directory: String,
    }

    #[derive(ToSchema, Serialize)]
    struct Response {}

    #[derive(Deserialize)]
    pub struct FileJwtPayload {
        #[serde(flatten)]
        pub base: crate::remote::jwt::BasePayload,

        pub server_uuid: uuid::Uuid,
        pub user_uuid: uuid::Uuid,
        pub unique_id: String,
    }

    #[utoipa::path(post, path = "/", responses(
        (status = OK, body = inline(Response)),
        (status = UNAUTHORIZED, body = inline(ApiError)),
        (status = NOT_FOUND, body = inline(ApiError)),
        (status = EXPECTATION_FAILED, body = inline(ApiError)),
    ), params(
        (
            "token" = String, Query,
            description = "The JWT token to use for authentication",
        ),
        (
            "directory" = String, Query,
            description = "The directory to upload the file to",
        ),
    ))]
    pub async fn route(
        state: GetState,
        headers: HeaderMap,
        connect_info: ConnectInfo<SocketAddr>,
        Query(data): Query<Params>,
        mut multipart: Multipart,
    ) -> (StatusCode, axum::Json<serde_json::Value>) {
        let payload: FileJwtPayload = match state.config.jwt.verify(&data.token) {
            Ok(payload) => payload,
            Err(_) => {
                return (
                    StatusCode::UNAUTHORIZED,
                    axum::Json(ApiError::new("invalid token").to_json()),
                );
            }
        };

        if !payload.base.validate(&state.config.jwt) {
            return (
                StatusCode::UNAUTHORIZED,
                axum::Json(ApiError::new("invalid token").to_json()),
            );
        }

        if !state.config.jwt.one_time_id(&payload.unique_id) {
            return (
                StatusCode::UNAUTHORIZED,
                axum::Json(ApiError::new("token has already been used").to_json()),
            );
        }

        let server = state
            .server_manager
            .get_servers()
            .await
            .iter()
            .find(|s| s.uuid == payload.server_uuid)
            .cloned();

        let server = match server {
            Some(server) => server,
            None => {
                return (
                    StatusCode::NOT_FOUND,
                    axum::Json(ApiError::new("server not found").to_json()),
                );
            }
        };

        let directory = match server.filesystem.safe_path(&data.directory).await {
            Some(path) => path,
            None => {
                return (
                    StatusCode::NOT_FOUND,
                    axum::Json(ApiError::new("directory not found").to_json()),
                );
            }
        };

        let metadata = directory.symlink_metadata();
        if !metadata.map(|m| m.is_dir()).unwrap_or(true) {
            return (
                StatusCode::EXPECTATION_FAILED,
                axum::Json(ApiError::new("directory is not a directory").to_json()),
            );
        }

        let user_ip = Some(state.config.find_ip(&headers, connect_info));

        while let Ok(Some(mut field)) = multipart.next_field().await {
            let filename = match field.file_name() {
                Some(name) => name,
                None => {
                    return (
                        StatusCode::EXPECTATION_FAILED,
                        axum::Json(ApiError::new("file name not found").to_json()),
                    );
                }
            };
            let file_path = directory.join(filename);

            if !server.filesystem.is_safe_path(&file_path).await {
                return (
                    StatusCode::EXPECTATION_FAILED,
                    axum::Json(ApiError::new("file path is not safe").to_json()),
                );
            }

            if server.filesystem.is_ignored(&file_path, false).await {
                return (
                    StatusCode::NOT_FOUND,
                    axum::Json(ApiError::new("file not found").to_json()),
                );
            }

            let mut written_size = 0;
            let mut writer = crate::server::filesystem::writer::AsyncFileSystemWriter::new(
                Arc::clone(&server.filesystem),
                file_path,
                None,
            )
            .await
            .unwrap();

            server
                .activity
                .log_activity(Activity {
                    event: ActivityEvent::FileUploaded,
                    user: Some(payload.user_uuid),
                    ip: user_ip,
                    metadata: Some(json!({
                        "file": filename,
                        "directory": server.filesystem.relative_path(&directory),
                    })),
                    timestamp: chrono::Utc::now(),
                })
                .await;

            while let Ok(Some(chunk)) = field.chunk().await {
                if written_size + chunk.len() > state.config.api.upload_limit * 1000 * 1000 {
                    return (
                        StatusCode::EXPECTATION_FAILED,
                        axum::Json(
                            ApiError::new(&format!(
                                "file size is larger than {}MB",
                                state.config.api.upload_limit
                            ))
                            .to_json(),
                        ),
                    );
                }

                writer.write_all(&chunk).await.unwrap();
                written_size += chunk.len();
            }

            writer.flush().await.unwrap();
        }

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
