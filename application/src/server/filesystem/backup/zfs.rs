use crate::models::DirectoryEntry;
use ignore::WalkBuilder;
use std::path::{Path, PathBuf};

#[inline]
fn get_base_path(server: &crate::server::Server, uuid: uuid::Uuid) -> PathBuf {
    Path::new(&server.filesystem.base_path)
        .join("zfs")
        .join(uuid.to_string())
        .join(format!("backup-{uuid}"))
}

pub async fn list(
    server: &crate::server::Server,
    uuid: uuid::Uuid,
    path: PathBuf,
    per_page: Option<usize>,
    page: usize,
    is_ignored: impl Fn(&Path, bool) -> bool + Send + Sync + 'static,
) -> Result<(usize, Vec<DirectoryEntry>), anyhow::Error> {
    let full_path = tokio::fs::canonicalize(get_base_path(server, uuid).join(path)).await?;

    if !full_path.starts_with(get_base_path(server, uuid)) {
        return Err(anyhow::anyhow!("Access to this path is denied"));
    }

    let mut directory = tokio::fs::read_dir(&full_path).await?;

    let mut directory_entries = Vec::new();
    let mut other_entries = Vec::new();

    while let Ok(Some(entry)) = directory.next_entry().await {
        let is_dir = entry.file_type().await.is_ok_and(|ft| ft.is_dir());
        let path = entry.path();
        let path = match path.strip_prefix(get_base_path(server, uuid)) {
            Ok(path) => path,
            Err(_) => continue,
        };

        if is_ignored(path, is_dir) || server.filesystem.is_ignored(path, is_dir).await {
            continue;
        }

        if is_dir {
            directory_entries.push(entry.file_name());
        } else {
            other_entries.push(entry.file_name());
        }
    }

    directory_entries.sort_unstable();
    other_entries.sort_unstable();

    let total_entries = directory_entries.len() + other_entries.len();
    let mut entries = Vec::new();

    if let Some(per_page) = per_page {
        let start = (page - 1) * per_page;

        for entry in directory_entries
            .into_iter()
            .chain(other_entries.into_iter())
            .skip(start)
            .take(per_page)
        {
            let path = full_path.join(&entry);
            let metadata = match tokio::fs::symlink_metadata(&path).await {
                Ok(metadata) => metadata,
                Err(_) => continue,
            };

            entries.push(server.filesystem.to_api_entry_tokio(path, metadata).await);
        }
    } else {
        for entry in directory_entries
            .into_iter()
            .chain(other_entries.into_iter())
        {
            let path = full_path.join(&entry);
            let metadata = match tokio::fs::symlink_metadata(&path).await {
                Ok(metadata) => metadata,
                Err(_) => continue,
            };

            entries.push(server.filesystem.to_api_entry_tokio(path, metadata).await);
        }
    }

    Ok((total_entries, entries))
}

pub async fn reader(
    server: &crate::server::Server,
    uuid: uuid::Uuid,
    path: PathBuf,
) -> Result<(Box<dyn tokio::io::AsyncRead + Send>, u64), anyhow::Error> {
    let full_path = tokio::fs::canonicalize(get_base_path(server, uuid).join(path)).await?;

    if !full_path.starts_with(get_base_path(server, uuid)) {
        return Err(anyhow::anyhow!("Access to this path is denied"));
    }

    let file = tokio::fs::File::open(full_path).await?;
    let metadata = file.metadata().await?;

    Ok((Box::new(file), metadata.len()))
}

pub async fn directory_reader(
    server: &crate::server::Server,
    uuid: uuid::Uuid,
    path: PathBuf,
) -> Result<tokio::io::DuplexStream, anyhow::Error> {
    let full_path = tokio::fs::canonicalize(get_base_path(server, uuid).join(path)).await?;

    if !full_path.starts_with(get_base_path(server, uuid)) {
        return Err(anyhow::anyhow!("Access to this path is denied"));
    }

    let (reader, writer) = tokio::io::duplex(65535);

    let server = server.clone();
    tokio::task::spawn_blocking(move || {
        let writer = tokio_util::io::SyncIoBridge::new(writer);
        let writer = flate2::write::GzEncoder::new(
            writer,
            server
                .config
                .system
                .backups
                .compression_level
                .flate2_compression_level(),
        );

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
