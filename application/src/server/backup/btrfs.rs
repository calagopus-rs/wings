use crate::remote::backups::RawServerBackup;
use axum::{
    body::Body,
    http::{HeaderMap, StatusCode},
};
use ignore::{WalkBuilder, WalkState, overrides::OverrideBuilder};
use std::{
    io::Write,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};
use tokio::process::Command;

#[inline]
fn get_backup_path(server: &crate::server::Server, uuid: uuid::Uuid) -> PathBuf {
    Path::new(&server.config.system.backup_directory)
        .join("btrfs")
        .join(uuid.to_string())
}

#[inline]
fn get_subvolume_path(server: &crate::server::Server, uuid: uuid::Uuid) -> PathBuf {
    get_backup_path(server, uuid).join("subvolume")
}

#[inline]
fn get_ignored(server: &crate::server::Server, uuid: uuid::Uuid) -> PathBuf {
    get_backup_path(server, uuid).join("ignored")
}

pub async fn create_backup(
    server: crate::server::Server,
    uuid: uuid::Uuid,
    overrides: ignore::overrides::Override,
    overrides_raw: String,
) -> Result<RawServerBackup, anyhow::Error> {
    let subvolume_path = get_subvolume_path(&server, uuid);
    let ignored_path = get_ignored(&server, uuid);

    tokio::fs::create_dir_all(get_backup_path(&server, uuid)).await?;

    let output = Command::new("btrfs")
        .arg("subvolume")
        .arg("snapshot")
        .arg(&server.filesystem.base_path)
        .arg(&subvolume_path)
        .output()
        .await?;

    if !output.status.success() {
        return Err(anyhow::anyhow!(
            "Failed to create Btrfs subvolume snapshot for {}: {}",
            server.filesystem.base_path.display(),
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    let output = Command::new("btrfs")
        .arg("subvolume")
        .arg("show")
        .arg(&subvolume_path)
        .output()
        .await?;

    let mut generation = None;
    if output.status.success() {
        let output_str = String::from_utf8_lossy(&output.stdout);
        for line in output_str.lines() {
            let mut whitespace = line.split_whitespace();

            if let Some(label) = line.split_whitespace().next() {
                if label == "Generation:" {
                    if let Some(parsed_generation) = whitespace.next() {
                        if let Ok(parsed_generation) = parsed_generation.parse::<u64>() {
                            generation = Some(parsed_generation);
                        }

                        break;
                    }
                }
            }
        }
    }

    tokio::fs::write(&ignored_path, overrides_raw).await?;

    let total_size = tokio::task::spawn_blocking(move || {
        WalkBuilder::new(&subvolume_path)
            .overrides(overrides)
            .git_ignore(false)
            .ignore(false)
            .git_exclude(false)
            .follow_links(false)
            .hidden(false)
            .build()
            .flatten()
            .fold(0u64, |acc, entry| {
                let metadata = match entry.metadata() {
                    Ok(metadata) => metadata,
                    Err(_) => return acc,
                };

                if metadata.is_file() {
                    acc + metadata.len()
                } else {
                    acc
                }
            })
    })
    .await?;

    Ok(RawServerBackup {
        checksum: format!("generation-{}", generation.unwrap_or(0)),
        checksum_type: "btrfs-subvolume".to_string(),
        size: total_size,
        successful: true,
        parts: vec![],
    })
}

pub async fn restore_backup(
    server: crate::server::Server,
    uuid: uuid::Uuid,
) -> Result<(), anyhow::Error> {
    let subvolume_path = get_subvolume_path(&server, uuid);
    let ignored_path = get_ignored(&server, uuid);

    let server = server.clone();
    let runtime = tokio::runtime::Handle::current();
    tokio::task::spawn_blocking(move || -> Result<(), anyhow::Error> {
        let mut override_builder = OverrideBuilder::new(&subvolume_path);

        for line in std::fs::read_to_string(&ignored_path)?.lines() {
            override_builder.add(line).ok();
        }

        WalkBuilder::new(&subvolume_path)
            .overrides(override_builder.build()?)
            .add_custom_ignore_filename(".pteroignore")
            .git_ignore(false)
            .ignore(false)
            .git_exclude(false)
            .follow_links(false)
            .hidden(false)
            .threads(4)
            .build_parallel()
            .run(move || {
                let server = server.clone();
                let runtime = runtime.clone();
                let subvolume_path = subvolume_path.clone();

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

                    let destination_path = server
                        .filesystem
                        .base_path
                        .join(path.strip_prefix(&subvolume_path).unwrap_or(path));
                    if !server.filesystem.is_safe_path_sync(&destination_path) {
                        return WalkState::Continue;
                    }

                    if metadata.is_file() {
                        runtime.block_on(
                            server.log_daemon(format!("(restoring): {}", path.display())),
                        );

                        std::fs::create_dir_all(destination_path.parent().unwrap()).ok();

                        let mut writer = crate::server::filesystem::writer::FileSystemWriter::new(
                            server.clone(),
                            destination_path,
                            Some(metadata.permissions()),
                            metadata.modified().ok(),
                        )
                        .unwrap();

                        let mut file = std::fs::File::open(path).unwrap();
                        std::io::copy(&mut file, &mut writer).unwrap();
                        writer.flush().unwrap();
                    } else if metadata.is_dir() {
                        std::fs::create_dir_all(&destination_path).ok();
                        std::fs::set_permissions(&destination_path, metadata.permissions()).ok();
                        std::os::unix::fs::chown(
                            &destination_path,
                            Some(metadata.uid()),
                            Some(metadata.gid()),
                        )
                        .ok();
                    } else if metadata.is_symlink() {
                        if let Ok(target) = std::fs::read_link(path) {
                            let destination_path = &destination_path;
                            if !server.filesystem.is_safe_path_sync(destination_path) {
                                return WalkState::Continue;
                            }

                            std::os::unix::fs::symlink(target, destination_path).ok();
                            std::fs::set_permissions(destination_path, metadata.permissions()).ok();
                            std::os::unix::fs::chown(
                                destination_path,
                                Some(metadata.uid()),
                                Some(metadata.gid()),
                            )
                            .ok();
                        }
                    }

                    WalkState::Continue
                })
            });

        Ok(())
    })
    .await??;

    Ok(())
}

pub async fn download_backup(
    server: &crate::server::Server,
    uuid: uuid::Uuid,
) -> Result<(StatusCode, HeaderMap, Body), anyhow::Error> {
    let subvolume_path = get_subvolume_path(server, uuid);
    let ignored_path = get_ignored(server, uuid);

    let (writer, reader) = tokio::io::duplex(65536);

    let server = server.clone();
    tokio::task::spawn_blocking(move || {
        let writer = tokio_util::io::SyncIoBridge::new(writer);
        let writer = flate2::write::GzEncoder::new(writer, flate2::Compression::default());

        let mut tar = tar::Builder::new(writer);
        tar.mode(tar::HeaderMode::Complete);
        tar.follow_symlinks(false);

        let mut override_builder = OverrideBuilder::new(&subvolume_path);

        for line in std::fs::read_to_string(&ignored_path).unwrap().lines() {
            override_builder.add(line).ok();
        }

        for entry in WalkBuilder::new(&subvolume_path)
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
                .strip_prefix(&subvolume_path)
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

    let mut headers = HeaderMap::new();
    headers.insert(
        "Content-Disposition",
        format!("attachment; filename={}.tar.gz", uuid)
            .parse()
            .unwrap(),
    );
    headers.insert("Content-Type", "application/gzip".parse().unwrap());

    Ok((
        StatusCode::OK,
        headers,
        Body::from_stream(tokio_util::io::ReaderStream::new(
            tokio::io::BufReader::new(reader),
        )),
    ))
}

pub async fn delete_backup(
    server: &crate::server::Server,
    uuid: uuid::Uuid,
) -> Result<(), anyhow::Error> {
    let subvolume_path = get_subvolume_path(server, uuid);

    let output = Command::new("btrfs")
        .arg("qgroup")
        .arg("show")
        .arg(&subvolume_path)
        .output()
        .await?;

    if output.status.success() {
        let uuid_str = uuid.to_string();
        let output_str = String::from_utf8_lossy(&output.stdout);

        for line in output_str.lines() {
            if line.ends_with(&uuid_str) {
                if let Some(qgroup_id) = line.split_whitespace().next() {
                    let output = Command::new("btrfs")
                        .arg("qgroup")
                        .arg("destroy")
                        .arg(qgroup_id)
                        .arg(&subvolume_path)
                        .output()
                        .await?;

                    if !output.status.success() {
                        return Err(anyhow::anyhow!(
                            "Failed to destroy Btrfs qgroup for {}: {}",
                            subvolume_path.display(),
                            String::from_utf8_lossy(&output.stderr)
                        ));
                    }
                }
            }
        }
    }

    tokio::fs::remove_dir_all(get_backup_path(server, uuid)).await?;

    Ok(())
}

pub async fn list_backups(
    server: &crate::server::Server,
) -> Result<Vec<uuid::Uuid>, anyhow::Error> {
    let mut backups = Vec::new();
    let path = Path::new(&server.config.system.backup_directory).join("btrfs");

    if !path.exists() {
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
