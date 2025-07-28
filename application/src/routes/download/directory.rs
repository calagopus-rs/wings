use super::State;
use utoipa_axum::{router::OpenApiRouter, routes};

mod get {
    use crate::{
        response::{ApiResponse, ApiResponseResult},
        routes::GetState,
    };
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
        token: String,
    }

    #[derive(Deserialize)]
    pub struct FolderJwtPayload {
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
    pub async fn route(state: GetState, Query(data): Query<Params>) -> ApiResponseResult {
        let payload: FolderJwtPayload = match state.config.jwt.verify(&data.token) {
            Ok(payload) => payload,
            Err(_) => {
                return ApiResponse::error("invalid token")
                    .with_status(StatusCode::UNAUTHORIZED)
                    .ok();
            }
        };

        if !payload.base.validate(&state.config.jwt).await {
            return ApiResponse::error("invalid token")
                .with_status(StatusCode::UNAUTHORIZED)
                .ok();
        }

        if !state.config.jwt.one_time_id(&payload.unique_id).await {
            return ApiResponse::error("token has already been used")
                .with_status(StatusCode::UNAUTHORIZED)
                .ok();
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
                return ApiResponse::error("server not found")
                    .with_status(StatusCode::NOT_FOUND)
                    .ok();
            }
        };

        let path = PathBuf::from(payload.file_path);
        let file_name = path.file_name().unwrap().to_string_lossy().to_string();

        let mut folder_ascii = "".to_string();
        for c in file_name.chars() {
            if c.is_ascii() {
                folder_ascii.push(c);
            } else {
                folder_ascii.push('_');
            }
        }

        folder_ascii.push_str(".tar.gz");

        let mut headers = HeaderMap::new();
        headers.insert(
            "Content-Disposition",
            format!(
                "attachment; filename={}",
                serde_json::Value::String(folder_ascii)
            )
            .parse()?,
        );
        headers.insert("Content-Type", "application/gzip".parse()?);

        if let Some((backup, path)) = server.filesystem.backup_fs(&server, &path).await {
            match crate::server::filesystem::backup::directory_reader(backup, &server, &path).await
            {
                Ok(reader) => {
                    return ApiResponse::new(Body::from_stream(
                        tokio_util::io::ReaderStream::with_capacity(reader, crate::BUFFER_SIZE),
                    ))
                    .with_headers(headers)
                    .ok();
                }
                Err(err) => {
                    tracing::error!(
                        server = %server.uuid,
                        path = %path.display(),
                        error = %err,
                        "failed to get backup directory contents",
                    );

                    return ApiResponse::error("failed to retrieve backup folder contents")
                        .with_status(StatusCode::EXPECTATION_FAILED)
                        .ok();
                }
            }
        }

        let metadata = server.filesystem.symlink_metadata(&path).await;
        if let Ok(metadata) = metadata {
            if !metadata.is_dir() || server.filesystem.is_ignored(&path, metadata.is_dir()).await {
                return ApiResponse::error("directory not found")
                    .with_status(StatusCode::NOT_FOUND)
                    .ok();
            }
        } else {
            return ApiResponse::error("directory not found")
                .with_status(StatusCode::NOT_FOUND)
                .ok();
        }

        let (writer, reader) = tokio::io::duplex(crate::BUFFER_SIZE);

        tokio::spawn({
            let path = path.clone();
            let server = server.clone();

            async move {
                let mut read_dir = match server.filesystem.read_dir(&path).await {
                    Ok(dir) => dir,
                    Err(err) => {
                        tracing::error!(
                            server = %server.uuid,
                            path = %path.display(),
                            error = %err,
                            "failed to read directory"
                        );

                        return;
                    }
                };
                let mut names = Vec::new();

                while let Some(Ok((_, name))) = read_dir.next_entry().await {
                    names.push(PathBuf::from(name));
                }

                let ignored = server.filesystem.get_ignored().await;
                crate::server::filesystem::archive::Archive::create_tar(
                    server,
                    writer,
                    &path,
                    names,
                    crate::server::filesystem::archive::CompressionType::Gz,
                    state.config.system.backups.compression_level,
                    None,
                    &[ignored],
                )
                .await
                .ok();
            }
        });

        ApiResponse::new(Body::from_stream(
            tokio_util::io::ReaderStream::with_capacity(reader, crate::BUFFER_SIZE),
        ))
        .with_headers(headers)
        .ok()
    }
}

pub fn router(state: &State) -> OpenApiRouter<State> {
    OpenApiRouter::new()
        .routes(routes!(get::route))
        .with_state(state.clone())
}
