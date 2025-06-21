use super::State;
use utoipa_axum::{router::OpenApiRouter, routes};

mod get {
    use crate::routes::{ApiError, api::servers::_server_::GetServer};
    use axum::{
        body::Body,
        extract::Query,
        http::{HeaderMap, StatusCode},
    };
    use serde::Deserialize;
    use std::path::PathBuf;
    use utoipa::ToSchema;

    #[derive(ToSchema, Deserialize)]
    pub struct Params {
        file: String,

        #[schema(default = "false")]
        #[serde(default)]
        download: bool,
    }

    #[utoipa::path(get, path = "/", responses(
        (status = OK, body = String),
        (status = NOT_FOUND, body = inline(ApiError)),
        (status = EXPECTATION_FAILED, body = inline(ApiError)),
    ), params(
        (
            "server" = uuid::Uuid,
            description = "The server uuid",
            example = "123e4567-e89b-12d3-a456-426614174000",
        ),
        (
            "file" = String, Query,
            description = "The file to view contents of",
        ),
        (
            "download" = bool, Query,
            description = "Whether to add 'download headers' to the file",
        ),
    ))]
    pub async fn route(
        server: GetServer,
        Query(data): Query<Params>,
    ) -> (StatusCode, HeaderMap, Body) {
        let path = match server.filesystem.canonicalize(&data.file).await {
            Ok(path) => path,
            Err(_) => PathBuf::from(data.file),
        };

        if let Some((backup, path)) = server.filesystem.backup_fs(&server, &path).await {
            match crate::server::filesystem::backup::reader(backup, &server, &path).await {
                Ok((reader, size)) => {
                    let mut headers = HeaderMap::new();

                    headers.insert("Content-Length", size.into());
                    if data.download {
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
                        headers.insert("Content-Type", "application/octet-stream".parse().unwrap());
                    }

                    return (
                        StatusCode::OK,
                        headers,
                        Body::from_stream(tokio_util::io::ReaderStream::new(Box::into_pin(reader))),
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
                        HeaderMap::from_iter([(
                            "Content-Type".parse().unwrap(),
                            "application/json".parse().unwrap(),
                        )]),
                        Body::from(
                            serde_json::to_string(&ApiError::new(
                                "failed to get backup file contents",
                            ))
                            .unwrap(),
                        ),
                    );
                }
            }
        }

        let metadata = server.filesystem.metadata(&path).await;
        if let Ok(metadata) = metadata {
            if !metadata.is_file() || server.filesystem.is_ignored(&path, metadata.is_dir()).await {
                return (
                    StatusCode::NOT_FOUND,
                    HeaderMap::from_iter([(
                        "Content-Type".parse().unwrap(),
                        "application/json".parse().unwrap(),
                    )]),
                    Body::from(serde_json::to_string(&ApiError::new("file not found")).unwrap()),
                );
            }
        }

        let mut file =
            match crate::server::filesystem::archive::Archive::open(server.0.clone(), path.clone())
                .await
            {
                Some(file) => file,
                None => {
                    return (
                        StatusCode::NOT_FOUND,
                        HeaderMap::from_iter([(
                            "Content-Type".parse().unwrap(),
                            "application/json".parse().unwrap(),
                        )]),
                        Body::from(
                            serde_json::to_string(&ApiError::new("file not found")).unwrap(),
                        ),
                    );
                }
            };

        let size = match file.estimated_size().await {
            Some(size) => size,
            None => {
                return (
                    StatusCode::EXPECTATION_FAILED,
                    HeaderMap::from_iter([(
                        "Content-Type".parse().unwrap(),
                        "application/json".parse().unwrap(),
                    )]),
                    Body::from(
                        serde_json::to_string(&ApiError::new(
                            "unable to retrieve estimated file size",
                        ))
                        .unwrap(),
                    ),
                );
            }
        };

        let reader = match file.reader().await {
            Some(reader) => reader,
            None => {
                return (
                    StatusCode::EXPECTATION_FAILED,
                    HeaderMap::from_iter([(
                        "Content-Type".parse().unwrap(),
                        "application/json".parse().unwrap(),
                    )]),
                    Body::from(
                        serde_json::to_string(&ApiError::new("unable to open file for reading"))
                            .unwrap(),
                    ),
                );
            }
        };

        let mut headers = HeaderMap::new();

        headers.insert("Content-Length", size.into());
        if data.download {
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
            headers.insert("Content-Type", "application/octet-stream".parse().unwrap());
        }

        (
            StatusCode::OK,
            headers,
            Body::from_stream(tokio_util::io::ReaderStream::new(Box::pin(reader))),
        )
    }
}

pub fn router(state: &State) -> OpenApiRouter<State> {
    OpenApiRouter::new()
        .routes(routes!(get::route))
        .with_state(state.clone())
}
