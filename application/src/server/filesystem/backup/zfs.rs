use crate::models::DirectoryEntry;
use ignore::WalkBuilder;
use std::path::{Path, PathBuf};

#[inline]
fn get_base_path(server: &crate::server::Server, uuid: uuid::Uuid) -> PathBuf {
    Path::new(&server.filesystem.base_path)
        .join("zfs")
        .join(uuid.to_string())
        .join(format!("backup-{}", uuid))
}

pub async fn list(
    server: &crate::server::Server,
    uuid: uuid::Uuid,
    path: &Path,
) -> std::io::Result<Vec<DirectoryEntry>> {
    let full_path = tokio::fs::canonicalize(get_base_path(server, uuid).join(path)).await?;

    if !full_path.starts_with(get_base_path(server, uuid)) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "Access to this path is denied",
        ));
    }

    let mut entries = Vec::new();

    let mut directory = tokio::fs::read_dir(full_path).await?;
    while let Ok(Some(entry)) = directory.next_entry().await {
        let path = entry.path();
        let metadata = match entry.metadata().await {
            Ok(metadata) => metadata,
            Err(_) => continue,
        };

        if server.filesystem.is_ignored(&path, metadata.is_dir()).await {
            continue;
        }

        let mut entry = server
            .filesystem
            .to_api_entry(path, cap_std::fs::Metadata::from_just_metadata(metadata))
            .await;
        if entry.directory {
            entry.size = 0;
        }

        entries.push(entry);

        if entries.len() >= server.config.api.directory_entry_limit {
            break;
        }
    }

    Ok(entries)
}

pub async fn reader(
    server: &crate::server::Server,
    uuid: uuid::Uuid,
    path: &Path,
) -> std::io::Result<(Box<dyn std::io::Read + Send>, u64)> {
    let full_path = tokio::fs::canonicalize(get_base_path(server, uuid).join(path)).await?;

    if !full_path.starts_with(get_base_path(server, uuid)) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "Access to this path is denied",
        ));
    }

    let file = tokio::fs::File::open(full_path).await?;
    let metadata = file.metadata().await?;
    let file = file.into_std().await;

    Ok((Box::new(file), metadata.len()))
}

pub async fn directory_reader(
    server: &crate::server::Server,
    uuid: uuid::Uuid,
    path: &Path,
) -> std::io::Result<tokio::io::DuplexStream> {
    let full_path = tokio::fs::canonicalize(get_base_path(server, uuid).join(path)).await?;

    if !full_path.starts_with(get_base_path(server, uuid)) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "Access to this path is denied",
        ));
    }

    let (reader, writer) = tokio::io::duplex(65535);

    let server = server.clone();
    tokio::task::spawn_blocking(move || {
        let writer = tokio_util::io::SyncIoBridge::new(writer);
        let writer = flate2::write::GzEncoder::new(writer, flate2::Compression::default());

        let mut tar = tar::Builder::new(writer);
        tar.mode(tar::HeaderMode::Complete);
        tar.follow_symlinks(false);

        for entry in WalkBuilder::new(&full_path)
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
                .strip_prefix(&full_path)
                .unwrap_or(entry.path());
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
                .is_ignored_sync(entry.path(), metadata.is_dir())
            {
                continue;
            }

            if metadata.is_dir() {
                tar.append_dir(path, entry.path()).ok();
            } else {
                tar.append_path_with_name(entry.path(), path).ok();
            }
        }

        tar.finish().ok();
    });

    Ok(reader)
}
