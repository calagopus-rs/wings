use crate::{io::counting_reader::CountingReader, remote::backups::RawServerBackup};
use axum::{
    body::Body,
    http::{HeaderMap, StatusCode},
};
use ignore::{WalkBuilder, WalkState, overrides::OverrideBuilder};
use std::{
    io::Write,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};
use tokio::process::Command;

#[inline]
fn get_backup_path(server: &crate::server::Server, uuid: uuid::Uuid) -> PathBuf {
    Path::new(&server.config.system.backup_directory)
        .join("zfs")
        .join(uuid.to_string())
}

#[inline]
fn get_snapshot_name(uuid: uuid::Uuid) -> String {
    format!("backup-{uuid}")
}

#[inline]
pub fn get_snapshot_path(server: &crate::server::Server, uuid: uuid::Uuid) -> PathBuf {
    server
        .filesystem
        .base_path
        .join(".zfs")
        .join("snapshot")
        .join(get_snapshot_name(uuid))
}

#[inline]
pub fn get_ignored(server: &crate::server::Server, uuid: uuid::Uuid) -> PathBuf {
    get_backup_path(server, uuid).join("ignored")
}

pub async fn create_backup(
    server: crate::server::Server,
    uuid: uuid::Uuid,
    ignore: ignore::gitignore::Gitignore,
    ignore_raw: String,
) -> Result<RawServerBackup, anyhow::Error> {
    let backup_path = get_backup_path(&server, uuid);
    let ignored_path = get_ignored(&server, uuid);
    let snapshot_name = get_snapshot_name(uuid);

    tokio::fs::create_dir_all(&backup_path).await?;

    let total_task = {
        let server = server.clone();
        let ignore = ignore.clone();

        async move {
            let ignored = [ignore];

            let mut walker = crate::server::filesystem::walker::AsyncWalkDir::new(
                server.clone(),
                PathBuf::from(""),
            )
            .await?
            .with_ignored(&ignored);
            let mut total = 0;
            while let Some(Ok((_, path))) = walker.next_entry().await {
                let metadata = match server.filesystem.symlink_metadata(&path).await {
                    Ok(metadata) => metadata,
                    Err(_) => continue,
                };

                total += metadata.len();
            }

            Ok::<u64, anyhow::Error>(total)
        }
    };

    let dataset_task = async {
        let output = Command::new("zfs")
            .arg("list")
            .arg("-o")
            .arg("name")
            .arg("-H")
            .arg(&server.filesystem.base_path)
            .output()
            .await?;

        if !output.status.success() {
            return Err(anyhow::anyhow!(
                "Failed to get ZFS dataset name for {}: {}",
                server.filesystem.base_path.display(),
                String::from_utf8_lossy(&output.stderr)
            ));
        }

        let dataset_name = String::from_utf8_lossy(&output.stdout).trim().to_string();

        let output = Command::new("zfs")
            .arg("snapshot")
            .arg(format!("{dataset_name}@{snapshot_name}"))
            .output()
            .await?;

        if !output.status.success() {
            return Err(anyhow::anyhow!(
                "Failed to create ZFS snapshot for {}: {}",
                server.filesystem.base_path.display(),
                String::from_utf8_lossy(&output.stderr)
            ));
        }

        tokio::fs::write(&ignored_path, ignore_raw).await?;
        tokio::fs::write(backup_path.join("dataset"), &dataset_name).await?;

        Ok::<_, anyhow::Error>(dataset_name)
    };

    let (total_size, dataset_name) = tokio::try_join!(total_task, dataset_task)?;

    Ok(RawServerBackup {
        checksum: dataset_name,
        checksum_type: "zfs-snapshot".to_string(),
        size: total_size,
        successful: true,
        parts: vec![],
    })
}

pub async fn restore_backup(
    server: crate::server::Server,
    uuid: uuid::Uuid,
    progress: Arc<AtomicU64>,
    total: Arc<AtomicU64>,
) -> Result<(), anyhow::Error> {
    let ignored_path = get_ignored(&server, uuid);
    let snapshot_path = get_snapshot_path(&server, uuid);

    let mut override_builder = OverrideBuilder::new(&snapshot_path);

    for line in tokio::fs::read_to_string(&ignored_path)
        .await
        .unwrap_or_default()
        .lines()
    {
        override_builder.add(line).ok();
    }

    let total_thread = tokio::task::spawn_blocking({
        let override_builder = override_builder.clone().build()?;
        let snapshot_path = snapshot_path.clone();
        let server = server.clone();

        move || {
            WalkBuilder::new(&snapshot_path)
                .overrides(override_builder)
                .add_custom_ignore_filename(".pteroignore")
                .git_ignore(false)
                .ignore(false)
                .git_exclude(false)
                .follow_links(false)
                .hidden(false)
                .threads(server.config.system.backups.btrfs.restore_threads)
                .build_parallel()
                .run(move || {
                    let total = Arc::clone(&total);
                    let server = server.clone();

                    Box::new(move |entry| {
                        let entry = match entry {
                            Ok(entry) => entry,
                            Err(_) => return WalkState::Continue,
                        };
                        let metadata = match entry.metadata() {
                            Ok(metadata) => metadata,
                            Err(_) => return WalkState::Continue,
                        };

                        if server
                            .filesystem
                            .is_ignored_sync(entry.path(), metadata.is_dir())
                        {
                            return WalkState::Continue;
                        }

                        if metadata.is_file() {
                            total.fetch_add(metadata.len(), Ordering::SeqCst);
                        }

                        WalkState::Continue
                    })
                });
        }
    });

    let server = server.clone();
    let runtime = tokio::runtime::Handle::current();
    let restore_thread = tokio::task::spawn_blocking(move || -> Result<(), anyhow::Error> {
        WalkBuilder::new(&snapshot_path)
            .overrides(override_builder.build()?)
            .add_custom_ignore_filename(".pteroignore")
            .git_ignore(false)
            .ignore(false)
            .git_exclude(false)
            .follow_links(false)
            .hidden(false)
            .threads(server.config.system.backups.zfs.restore_threads)
            .build_parallel()
            .run(move || {
                let server = server.clone();
                let runtime = runtime.clone();
                let snapshot_path = snapshot_path.clone();
                let filesystem = server.filesystem.sync_base_dir().unwrap();
                let progress = Arc::clone(&progress);

                Box::new(move |entry| {
                    let entry = match entry {
                        Ok(entry) => entry,
                        Err(_) => return WalkState::Continue,
                    };
                    let path = entry.path();

                    let metadata = match entry.metadata() {
                        Ok(metadata) => metadata,
                        Err(_) => return WalkState::Continue,
                    };

                    if server.filesystem.is_ignored_sync(path, metadata.is_dir()) {
                        return WalkState::Continue;
                    }

                    let destination_path = path.strip_prefix(&snapshot_path).unwrap_or(path);

                    if metadata.is_file() {
                        runtime.block_on(
                            server.log_daemon(format!("(restoring): {}", path.display())),
                        );

                        if let Some(parent) = destination_path.parent() {
                            filesystem.create_dir_all(parent).ok();
                        }

                        let file = std::fs::File::open(path).unwrap();

                        let mut writer = crate::server::filesystem::writer::FileSystemWriter::new(
                            server.clone(),
                            destination_path.to_path_buf(),
                            Some(metadata.permissions()),
                            metadata.modified().ok(),
                        )
                        .unwrap();
                        let mut reader =
                            CountingReader::new_with_bytes_read(file, Arc::clone(&progress));

                        std::io::copy(&mut reader, &mut writer).unwrap();
                        writer.flush().unwrap();
                    } else if metadata.is_dir() {
                        filesystem.create_dir_all(destination_path).ok();
                        filesystem
                            .set_permissions(
                                destination_path,
                                cap_std::fs::Permissions::from_std(metadata.permissions()),
                            )
                            .ok();
                    } else if metadata.is_symlink() {
                        if let Ok(target) = std::fs::read_link(path) {
                            filesystem.symlink(target, path).unwrap_or_else(|err| {
                                tracing::debug!("failed to create symlink from backup: {:#?}", err);
                            });
                        }
                    }

                    WalkState::Continue
                })
            });

        Ok(())
    });

    let (_, _) = tokio::try_join!(total_thread, restore_thread)?;

    Ok(())
}

pub async fn download_backup(
    server: &crate::server::Server,
    uuid: uuid::Uuid,
) -> Result<(StatusCode, HeaderMap, Body), anyhow::Error> {
    let ignored_path = get_ignored(server, uuid);
    let snapshot_path = get_snapshot_path(server, uuid);
    let snapshot_name = get_snapshot_name(uuid);

    if !Path::new(&snapshot_path).exists() {
        return Err(anyhow::anyhow!("Snapshot {} does not exist", snapshot_name));
    }

    let (writer, reader) = tokio::io::duplex(crate::BUFFER_SIZE);

    let server = server.clone();
    tokio::task::spawn_blocking(move || {
        let writer = tokio_util::io::SyncIoBridge::new(writer);
        let writer = flate2::write::GzEncoder::new(writer, flate2::Compression::default());

        let mut tar = tar::Builder::new(writer);
        tar.mode(tar::HeaderMode::Complete);
        tar.follow_symlinks(false);

        let mut override_builder = OverrideBuilder::new(&snapshot_path);

        for line in std::fs::read_to_string(&ignored_path).unwrap().lines() {
            override_builder.add(line).ok();
        }

        for entry in WalkBuilder::new(&snapshot_path)
            .overrides(override_builder.build().unwrap())
            .add_custom_ignore_filename(".pteroignore")
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
                .strip_prefix(&snapshot_path)
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

        if let Ok(inner) = tar.into_inner()
            && let Ok(mut inner) = inner.finish()
        {
            inner.flush().ok();
        }
    });

    let mut headers = HeaderMap::with_capacity(2);
    headers.insert(
        "Content-Disposition",
        format!("attachment; filename={uuid}.tar.gz")
            .parse()
            .unwrap(),
    );
    headers.insert("Content-Type", "application/gzip".parse().unwrap());

    Ok((
        StatusCode::OK,
        headers,
        Body::from_stream(tokio_util::io::ReaderStream::with_capacity(
            reader,
            crate::BUFFER_SIZE,
        )),
    ))
}

pub async fn delete_backup(
    server: &crate::server::Server,
    uuid: uuid::Uuid,
) -> Result<(), anyhow::Error> {
    let backup_path = get_backup_path(server, uuid);
    let snapshot_name = get_snapshot_name(uuid);

    if !backup_path.exists() {
        return Ok(());
    }

    if let Ok(dataset_name) = tokio::fs::read_to_string(backup_path.join("dataset")).await {
        let output = Command::new("zfs")
            .arg("destroy")
            .arg(format!("{}@{}", dataset_name.trim(), snapshot_name))
            .output()
            .await?;

        if !output.status.success() {
            return Err(anyhow::anyhow!(
                "Failed to destroy ZFS snapshot: {}",
                String::from_utf8_lossy(&output.stderr)
            ));
        }
    }

    tokio::fs::remove_dir_all(backup_path).await?;

    Ok(())
}

pub async fn list_backups(
    server: &crate::server::Server,
) -> Result<Vec<uuid::Uuid>, anyhow::Error> {
    let mut backups = Vec::new();
    let path = Path::new(&server.config.system.backup_directory).join("zfs");

    if tokio::fs::metadata(&path).await.is_err() {
        return Ok(backups);
    }

    let mut entries = tokio::fs::read_dir(path).await?;
    while let Some(entry) = entries.next_entry().await? {
        let file_name = entry.file_name();

        if let Ok(uuid) = uuid::Uuid::parse_str(file_name.to_str().unwrap_or_default()) {
            backups.push(uuid);
        }
    }

    Ok(backups)
}
