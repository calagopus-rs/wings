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
    use std::path::PathBuf;
    use utoipa::ToSchema;

    #[derive(ToSchema, Deserialize)]
    pub struct Params {
        token: String,
    }

    #[derive(Deserialize)]
    pub struct FilesJwtPayload {
        #[serde(flatten)]
        pub base: crate::remote::jwt::BasePayload,

        pub file_path: String,
        pub file_paths: Vec<String>,
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
        let payload: FilesJwtPayload = match state.config.jwt.verify(&data.token) {
            Ok(payload) => payload,
            Err(_) => {
                return (
                    StatusCode::UNAUTHORIZED,
                    HeaderMap::new(),
                    Body::from("Invalid token"),
                );
            }
        };

        if !payload.base.validate(&state.config.jwt).await {
            return (
                StatusCode::UNAUTHORIZED,
                HeaderMap::new(),
                Body::from("Invalid token"),
            );
        }

        if !state.config.jwt.one_time_id(&payload.unique_id).await {
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

        let path = PathBuf::from(payload.file_path);

        let mut folder_ascii = "".to_string();
        for (i, file_path) in payload.file_paths.iter().enumerate() {
            let file_name = PathBuf::from(file_path)
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();

            for c in file_name.chars() {
                if c.is_ascii() {
                    folder_ascii.push(c);
                } else {
                    folder_ascii.push('_');
                }
            }

            if i < payload.file_paths.len() - 1 {
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
            .parse()
            .unwrap(),
        );
        headers.insert("Content-Type", "application/gzip".parse().unwrap());

        if let Some((backup, path)) = server.filesystem.backup_fs(&server, &path).await {
            match crate::server::filesystem::backup::files_reader(
                backup,
                &server,
                &path,
                payload.file_paths.into_iter().map(PathBuf::from).collect(),
            )
            .await
            {
                Ok(reader) => {
                    return (
                        StatusCode::OK,
                        headers,
                        Body::from_stream(tokio_util::io::ReaderStream::with_capacity(
                            reader,
                            crate::BUFFER_SIZE,
                        )),
                    );
                }
                Err(err) => {
                    tracing::error!(
                        server = %server.uuid,
                        path = %path.display(),
                        error = %err,
                        "failed to get backup directory contents",
                    );

                    return (
                        StatusCode::EXPECTATION_FAILED,
                        HeaderMap::new(),
                        Body::from("Failed to retrieve backup folder contents"),
                    );
                }
            }
        }

        let metadata = server.filesystem.symlink_metadata(&path).await;
        if let Ok(metadata) = metadata {
            if !metadata.is_dir() || server.filesystem.is_ignored(&path, metadata.is_dir()).await {
                return (
                    StatusCode::NOT_FOUND,
                    HeaderMap::new(),
                    Body::from("Folder not found"),
                );
            }
        } else {
            return (
                StatusCode::NOT_FOUND,
                HeaderMap::new(),
                Body::from("Folder not found"),
            );
        }

        let (writer, reader) = tokio::io::duplex(crate::BUFFER_SIZE);

        tokio::spawn({
            let path = path.clone();
            let server = server.clone();

            async move {
                let ignored = server.filesystem.get_ignored().await;
                crate::server::filesystem::archive::Archive::create_tar(
                    server,
                    writer,
                    &path,
                    payload.file_paths.into_iter().map(PathBuf::from).collect(),
                    crate::server::filesystem::archive::CompressionType::Gz,
                    state.config.system.backups.compression_level,
                    None,
                    &[ignored],
                )
                .await
                .ok();
            }
        });

        (
            StatusCode::OK,
            headers,
            Body::from_stream(tokio_util::io::ReaderStream::with_capacity(
                reader,
                crate::BUFFER_SIZE,
            )),
        )
    }
}

pub fn router(state: &State) -> OpenApiRouter<State> {
    OpenApiRouter::new()
        .routes(routes!(get::route))
        .with_state(state.clone())
}
