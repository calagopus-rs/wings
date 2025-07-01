use super::State;
use utoipa_axum::{router::OpenApiRouter, routes};

mod post {
    use crate::routes::{ApiError, GetState, api::servers::_server_::GetServer};
    use axum::http::StatusCode;
    use cap_std::fs::PermissionsExt;
    use serde::Deserialize;
    use std::io::Write;
    use utoipa::ToSchema;

    #[derive(ToSchema, Deserialize)]
    pub struct Payload {
        #[serde(default)]
        pub root: String,

        pub files: Vec<String>,
    }

    #[utoipa::path(post, path = "/", responses(
        (status = OK, body = crate::models::DirectoryEntry),
        (status = NOT_FOUND, body = inline(ApiError)),
        (status = EXPECTATION_FAILED, body = inline(ApiError)),
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
    ) -> (StatusCode, axum::Json<serde_json::Value>) {
        let root = match server.filesystem.canonicalize(data.root).await {
            Ok(path) => path,
            Err(_) => {
                return (
                    StatusCode::NOT_FOUND,
                    axum::Json(ApiError::new("root not found").to_json()),
                );
            }
        };

        let metadata = server.filesystem.symlink_metadata(&root).await;
        if !metadata.map(|m| m.is_dir()).unwrap_or(true) {
            return (
                StatusCode::EXPECTATION_FAILED,
                axum::Json(ApiError::new("root is not a directory").to_json()),
            );
        }

        let file_name = format!(
            "archive-{}.tar.gz",
            chrono::Local::now().format("%Y-%m-%dT%H%M%S%z")
        );
        let file_name = root.join(file_name);

        if server.filesystem.is_ignored(&file_name, false).await {
            return (
                StatusCode::EXPECTATION_FAILED,
                axum::Json(ApiError::new("file not found").to_json()),
            );
        }

        match tokio::task::spawn_blocking({
            let filesystem = server.filesystem.base_dir().await.unwrap();
            let file_name = file_name.clone();
            let server = server.0.clone();
            let root = root.clone();

            move || -> Result<(), anyhow::Error> {
                let writer = crate::server::filesystem::writer::FileSystemWriter::new(
                    server.clone(),
                    file_name,
                    None,
                    None,
                )?;

                let mut archive = tar::Builder::new(flate2::write::GzEncoder::new(
                    writer,
                    state
                        .config
                        .system
                        .backups
                        .compression_level
                        .flate2_compression_level(),
                ));

                for file in data.files {
                    let source = server.filesystem.relative_path(&root.join(file));

                    let relative = match source.strip_prefix(&root) {
                        Ok(path) => path,
                        Err(_) => continue,
                    };

                    let source_metadata = match filesystem.symlink_metadata(&source) {
                        Ok(metadata) => metadata,
                        Err(_) => continue,
                    };
                    if server
                        .filesystem
                        .is_ignored_sync(&source, source_metadata.is_dir())
                    {
                        continue;
                    }

                    if source_metadata.is_dir() {
                        let mut header = tar::Header::new_gnu();
                        header.set_size(0);
                        header.set_mode(source_metadata.permissions().mode());
                        header.set_mtime(
                            source_metadata
                                .modified()
                                .map(|t| {
                                    t.into_std()
                                        .duration_since(std::time::UNIX_EPOCH)
                                        .unwrap_or_default()
                                })
                                .unwrap_or_default()
                                .as_secs() as u64,
                        );

                        archive
                            .append_data(&mut header, relative, std::io::empty())
                            .ok();

                        let (mut walker, strip_path) = server.filesystem.walk_dir(&source)?;

                        for entry in walker
                            .git_ignore(false)
                            .ignore(false)
                            .git_exclude(false)
                            .follow_links(false)
                            .hidden(false)
                            .build()
                            .flatten()
                        {
                            let path = entry
                                .path()
                                .strip_prefix(&strip_path)
                                .unwrap_or(entry.path());
                            if path.display().to_string().is_empty() {
                                continue;
                            }

                            let display_path = relative.join(path);
                            let path = server.filesystem.relative_path(&source.join(path));

                            let metadata = match filesystem.symlink_metadata(&path) {
                                Ok(metadata) => metadata,
                                Err(_) => continue,
                            };

                            if server.filesystem.is_ignored_sync(&path, metadata.is_dir()) {
                                continue;
                            }

                            if metadata.is_dir() {
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

                                archive
                                    .append_data(&mut header, display_path, std::io::empty())
                                    .ok();
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

                                archive.append_data(&mut header, display_path, file).ok();
                            } else if let Ok(link_target) = filesystem.read_link_contents(path) {
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

                                archive.append_link(&mut header, relative, link_target)?;
                            }
                        }
                    } else if source_metadata.is_file() {
                        let mut header = tar::Header::new_gnu();
                        header.set_size(source_metadata.len());
                        header.set_mode(source_metadata.permissions().mode());
                        header.set_mtime(
                            source_metadata
                                .modified()
                                .map(|t| {
                                    t.into_std()
                                        .duration_since(std::time::UNIX_EPOCH)
                                        .unwrap_or_default()
                                })
                                .unwrap_or_default()
                                .as_secs() as u64,
                        );

                        archive.append_data(&mut header, relative, filesystem.open(&source)?)?;
                    } else if let Ok(link_target) = filesystem.read_link_contents(&source) {
                        let mut header = tar::Header::new_gnu();
                        header.set_size(0);
                        header.set_mode(source_metadata.permissions().mode());
                        header.set_mtime(
                            source_metadata
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

                        archive.append_link(&mut header, relative, link_target)?;
                    }
                }

                archive.finish()?;
                let mut inner = archive.into_inner()?;
                inner.flush()?;

                Ok(())
            }
        })
        .await
        {
            Ok(Ok(())) => {}
            Ok(Err(err)) => {
                tracing::error!(
                    server = %server.uuid,
                    root = %root.display(),
                    "failed to compress files: {:#?}",
                    err,
                );

                return (
                    StatusCode::EXPECTATION_FAILED,
                    axum::Json(
                        ApiError::new(&format!("failed to compress files: {err}")).to_json(),
                    ),
                );
            }
            Err(err) => {
                tracing::error!(
                    server = %server.uuid,
                    root = %root.display(),
                    "failed to compress files: {:#?}",
                    err,
                );

                return (
                    StatusCode::EXPECTATION_FAILED,
                    axum::Json(ApiError::new("failed to compress files").to_json()),
                );
            }
        }

        let metadata = server
            .filesystem
            .symlink_metadata(&file_name)
            .await
            .unwrap();

        (
            StatusCode::OK,
            axum::Json(
                serde_json::to_value(server.filesystem.to_api_entry(file_name, metadata).await)
                    .unwrap(),
            ),
        )
    }
}

pub fn router(state: &State) -> OpenApiRouter<State> {
    OpenApiRouter::new()
        .routes(routes!(post::route))
        .with_state(state.clone())
}
