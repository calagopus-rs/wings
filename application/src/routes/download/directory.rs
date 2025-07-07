use super::State;
use utoipa_axum::{router::OpenApiRouter, routes};

mod get {
    use crate::routes::GetState;
    use axum::{
        body::Body,
        extract::Query,
        http::{HeaderMap, StatusCode},
    };
    use cap_std::fs::PermissionsExt;
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
    pub async fn route(
        state: GetState,
        Query(data): Query<Params>,
    ) -> (StatusCode, HeaderMap, Body) {
        let payload: FolderJwtPayload = match state.config.jwt.verify(&data.token) {
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
            .parse()
            .unwrap(),
        );
        headers.insert("Content-Type", "application/gzip".parse().unwrap());

        if let Some((backup, path)) = server.filesystem.backup_fs(&server, &path).await {
            match crate::server::filesystem::backup::directory_reader(backup, &server, &path).await
            {
                Ok(reader) => {
                    return (
                        StatusCode::OK,
                        headers,
                        Body::from_stream(tokio_util::io::ReaderStream::new(Box::pin(reader))),
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

        let (writer, reader) = tokio::io::duplex(65536);

        tokio::task::spawn_blocking(move || {
            let writer = tokio_util::io::SyncIoBridge::new(writer);
            let writer = flate2::write::GzEncoder::new(writer, flate2::Compression::default());

            let mut tar = tar::Builder::new(writer);
            tar.mode(tar::HeaderMode::Complete);
            tar.follow_symlinks(false);

            let (mut walker, strip_path) = server.filesystem.walk_dir(path).unwrap();
            let filesystem = server.filesystem.sync_base_dir().unwrap();

            for entry in walker
                .git_ignore(false)
                .ignore(false)
                .git_exclude(false)
                .follow_links(false)
                .hidden(false)
                .build()
                .flatten()
            {
                let display_path = entry
                    .path()
                    .strip_prefix(&strip_path)
                    .unwrap_or(entry.path());
                if display_path.display().to_string().is_empty() {
                    continue;
                }

                let path = server.filesystem.relative_path(entry.path());

                let metadata = match filesystem.symlink_metadata(&path) {
                    Ok(metadata) => metadata,
                    Err(_) => continue,
                };

                if server
                    .filesystem
                    .is_ignored_sync(entry.path(), metadata.is_dir())
                {
                    continue;
                }

                if metadata.is_dir() {
                    tar.append_dir(display_path, entry.path()).ok();
                } else if metadata.is_file() {
                    let file = match filesystem.open(&path) {
                        Ok(file) => file,
                        Err(_) => continue,
                    };

                    let mut header = tar::Header::new_gnu();
                    header.set_size(metadata.len());
                    header.set_mode(metadata.permissions().mode());
                    header.set_mtime(
                        metadata
                            .modified()
                            .map(|t| {
                                t.into_std()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .unwrap_or_default()
                            })
                            .unwrap_or_default()
                            .as_secs() as u64,
                    );

                    tar.append_data(&mut header, display_path, file).ok();
                } else if let Ok(link_target) = filesystem.read_link(&path) {
                    let mut header = tar::Header::new_gnu();
                    header.set_size(0);
                    header.set_mode(metadata.permissions().mode());
                    header.set_mtime(
                        metadata
                            .modified()
                            .map(|t| {
                                t.into_std()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .unwrap_or_default()
                            })
                            .unwrap_or_default()
                            .as_secs() as u64,
                    );
                    header.set_entry_type(tar::EntryType::Symlink);

                    tar.append_link(&mut header, display_path, link_target)
                        .unwrap();
                }
            }

            tar.finish().ok();
        });

        (
            StatusCode::OK,
            headers,
            Body::from_stream(tokio_util::io::ReaderStream::new(
                tokio::io::BufReader::new(reader),
            )),
        )
    }
}

pub fn router(state: &State) -> OpenApiRouter<State> {
    OpenApiRouter::new()
        .routes(routes!(get::route))
        .with_state(state.clone())
}
