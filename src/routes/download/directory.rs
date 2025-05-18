use super::State;
use utoipa_axum::{router::OpenApiRouter, routes};

mod get {
    use crate::routes::GetState;
    use axum::{
        body::Body,
        extract::Query,
        http::{HeaderMap, StatusCode},
    };
    use ignore::WalkBuilder;
    use serde::Deserialize;
    use std::{fs::File, os::unix::fs::MetadataExt};
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

        let metadata = tokio::fs::symlink_metadata(&path).await;
        if !metadata.is_ok_and(|m| m.is_dir() && !server.filesystem.is_ignored(&path, m.is_dir())) {
            return (
                StatusCode::NOT_FOUND,
                HeaderMap::new(),
                Body::from("Folder not found"),
            );
        }

        let file_name = path.file_name().unwrap().to_string_lossy().to_string();
        let (writer, reader) = tokio::io::duplex(65536);

        tokio::task::spawn_blocking(move || {
            let writer = tokio_util::io::SyncIoBridge::new(writer);
            let writer = flate2::write::GzEncoder::new(writer, flate2::Compression::default());

            let mut tar = tar::Builder::new(writer);
            tar.mode(tar::HeaderMode::Complete);

            for entry in WalkBuilder::new(&path)
                .hidden(false)
                .git_ignore(false)
                .ignore(false)
                .git_exclude(false)
                .follow_links(false)
                .build()
                .flatten()
            {
                let path = entry.path().strip_prefix(&path).unwrap_or(entry.path());
                if path.display().to_string().is_empty() {
                    continue;
                }

                let metadata = match entry.metadata() {
                    Ok(metadata) => metadata,
                    Err(_) => {
                        continue;
                    }
                };

                if server
                    .filesystem
                    .is_ignored(entry.path(), metadata.is_dir())
                {
                    continue;
                }

                if metadata.is_dir() {
                    let mut entry_header = tar::Header::new_gnu();
                    entry_header.set_mode(metadata.mode());
                    entry_header.set_mtime(metadata.mtime() as u64);
                    entry_header.set_entry_type(tar::EntryType::Directory);

                    if tar
                        .append_data(&mut entry_header, path, std::io::empty())
                        .is_err()
                    {
                        break;
                    }
                } else if metadata.is_file() {
                    let mut entry_header = tar::Header::new_gnu();
                    entry_header.set_mode(metadata.mode());
                    entry_header.set_entry_type(tar::EntryType::Regular);
                    entry_header.set_mtime(metadata.mtime() as u64);
                    entry_header.set_size(metadata.len());

                    let file = File::open(entry.path()).unwrap();

                    if tar.append_data(&mut entry_header, path, file).is_err() {
                        break;
                    }
                } else {
                    let mut entry_header = tar::Header::new_gnu();
                    entry_header.set_mode(metadata.mode());
                    entry_header.set_mtime(metadata.mtime() as u64);
                    entry_header.set_entry_type(tar::EntryType::Symlink);

                    if tar
                        .append_link(&mut entry_header, path, entry.path())
                        .is_err()
                    {
                        break;
                    }
                }
            }

            tar.finish().ok();
        });

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
