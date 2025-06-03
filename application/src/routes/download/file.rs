use super::State;
use utoipa_axum::{router::OpenApiRouter, routes};

mod get {
    use crate::routes::GetState;
    use axum::{
        body::Body,
        extract::Query,
        http::{HeaderMap, StatusCode},
    };
    use serde::Deserialize;
    use tokio::{fs::File, io::BufReader};
    use utoipa::ToSchema;

    #[derive(ToSchema, Deserialize)]
    pub struct Params {
        token: String,
    }

    #[derive(Deserialize)]
    pub struct FileJwtPayload {
        #[serde(flatten)]
        pub base: crate::remote::jwt::BasePayload,

        pub file_path: String,
        pub server_uuid: uuid::Uuid,
        pub unique_id: String,
    }

    #[utoipa::path(get, path = "/", responses(
        (status = OK, body = String),
        (status = UNAUTHORIZED, body = String),
        (status = NOT_FOUND, body = String),
        (status = EXPECTATION_FAILED, body = String),
    ), params(
        (
            "token" = String, Query,
            description = "The JWT token to use for authentication",
        ),
    ))]
    pub async fn route(
        state: GetState,
        Query(data): Query<Params>,
    ) -> (StatusCode, HeaderMap, Body) {
        let payload: FileJwtPayload = match state.config.jwt.verify(&data.token) {
            Ok(payload) => payload,
            Err(_) => {
                return (
                    StatusCode::UNAUTHORIZED,
                    HeaderMap::new(),
                    Body::from("Invalid token"),
                );
            }
        };

        if !payload.base.validate(&state.config.jwt) {
            return (
                StatusCode::UNAUTHORIZED,
                HeaderMap::new(),
                Body::from("Invalid token"),
            );
        }

        if !state.config.jwt.one_time_id(&payload.unique_id) {
            return (
                StatusCode::UNAUTHORIZED,
                HeaderMap::new(),
                Body::from("Token has already been used"),
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
                    HeaderMap::new(),
                    Body::from("Server not found"),
                );
            }
        };

        let path = match server.filesystem.safe_path(&payload.file_path).await {
            Some(path) => path,
            None => {
                return (
                    StatusCode::NOT_FOUND,
                    HeaderMap::new(),
                    Body::from("File not found"),
                );
            }
        };

        let metadata = match tokio::fs::symlink_metadata(&path).await {
            Ok(metadata) => {
                if !metadata.is_file()
                    || server.filesystem.is_ignored(&path, metadata.is_dir()).await
                {
                    return (
                        StatusCode::NOT_FOUND,
                        HeaderMap::new(),
                        Body::from("File not found"),
                    );
                } else {
                    metadata
                }
            }
            Err(_) => {
                if let Some((adapter, uuid, path)) =
                    server.filesystem.backup_fs(&server, &path).await
                {
                    match crate::server::filesystem::backup::reader(adapter, &server, uuid, &path)
                        .await
                    {
                        Ok((mut reader, size)) => {
                            let mut headers = HeaderMap::new();

                            headers.insert("Content-Length", size.into());
                            headers.insert(
                                "Content-Disposition",
                                format!(
                                    "attachment; filename={}",
                                    serde_json::Value::String(
                                        path.file_name().unwrap().to_str().unwrap().to_string(),
                                    )
                                )
                                .parse()
                                .unwrap(),
                            );
                            headers.insert(
                                "Content-Type",
                                "application/octet-stream".parse().unwrap(),
                            );

                            let (writer, async_reader) = tokio::io::duplex(8192);
                            tokio::task::spawn_blocking(move || {
                                let mut sync_writer = tokio_util::io::SyncIoBridge::new(writer);

                                std::io::copy(&mut reader, &mut sync_writer).ok();
                            });

                            return (
                                StatusCode::OK,
                                headers,
                                Body::from_stream(tokio_util::io::ReaderStream::new(Box::pin(
                                    async_reader,
                                ))),
                            );
                        }
                        Err(err) => {
                            tracing::error!(
                                server = %server.uuid,
                                path = %path.display(),
                                error = %err,
                                "failed to get backup file contents",
                            );

                            return (
                                StatusCode::EXPECTATION_FAILED,
                                HeaderMap::new(),
                                Body::from("Failed to retrieve file contents from backup"),
                            );
                        }
                    }
                }

                return (
                    StatusCode::NOT_FOUND,
                    HeaderMap::new(),
                    Body::from("File not found"),
                );
            }
        };

        let file = match File::open(&path).await {
            Ok(file) => file,
            Err(_) => {
                return (
                    StatusCode::NOT_FOUND,
                    HeaderMap::new(),
                    Body::from("File not found"),
                );
            }
        };

        let mut headers = HeaderMap::new();
        headers.insert("Content-Length", metadata.len().into());
        headers.insert(
            "Content-Disposition",
            format!(
                "attachment; filename={}",
                serde_json::Value::String(path.file_name().unwrap().to_str().unwrap().to_string())
            )
            .parse()
            .unwrap(),
        );
        headers.insert("Content-Type", "application/octet-stream".parse().unwrap());

        (
            StatusCode::OK,
            headers,
            Body::from_stream(tokio_util::io::ReaderStream::new(BufReader::new(file))),
        )
    }
}

pub fn router(state: &State) -> OpenApiRouter<State> {
    OpenApiRouter::new()
        .routes(routes!(get::route))
        .with_state(state.clone())
}
