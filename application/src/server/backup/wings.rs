use crate::remote::backups::RawServerBackup;
use axum::{
    body::Body,
    http::{HeaderMap, StatusCode},
};
use chrono::{Datelike, Timelike};
use futures::StreamExt;
use ignore::WalkBuilder;
use sha1::Digest;
use std::{
    fs::Permissions,
    io::Write,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::{Arc, RwLock, atomic::AtomicUsize},
};
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader};

#[inline]
fn get_tar_file_name(server: &crate::server::Server, uuid: uuid::Uuid) -> PathBuf {
    Path::new(&server.config.system.backup_directory).join(format!("{uuid}.tar"))
}

#[inline]
fn get_tar_gz_file_name(server: &crate::server::Server, uuid: uuid::Uuid) -> PathBuf {
    Path::new(&server.config.system.backup_directory).join(format!("{uuid}.tar.gz"))
}

#[inline]
fn get_tar_zstd_file_name(server: &crate::server::Server, uuid: uuid::Uuid) -> PathBuf {
    Path::new(&server.config.system.backup_directory).join(format!("{uuid}.tar.zst"))
}

#[inline]
fn get_zip_file_name(server: &crate::server::Server, uuid: uuid::Uuid) -> PathBuf {
    Path::new(&server.config.system.backup_directory).join(format!("{uuid}.zip"))
}

#[inline]
fn get_file_name(server: &crate::server::Server, uuid: uuid::Uuid) -> PathBuf {
    match server.config.system.backups.wings.archive_format {
        crate::config::SystemBackupsWingsArchiveFormat::Tar => get_tar_file_name(server, uuid),
        crate::config::SystemBackupsWingsArchiveFormat::TarGz => get_tar_gz_file_name(server, uuid),
        crate::config::SystemBackupsWingsArchiveFormat::TarZstd => {
            get_tar_zstd_file_name(server, uuid)
        }
        crate::config::SystemBackupsWingsArchiveFormat::Zip => get_zip_file_name(server, uuid),
    }
}

#[inline]
pub async fn get_first_file_name(
    server: &crate::server::Server,
    uuid: uuid::Uuid,
) -> Result<(crate::config::SystemBackupsWingsArchiveFormat, PathBuf), anyhow::Error> {
    let file_name = get_tar_file_name(server, uuid);
    if tokio::fs::metadata(&file_name).await.is_ok() {
        return Ok((
            crate::config::SystemBackupsWingsArchiveFormat::Tar,
            file_name,
        ));
    }

    let file_name = get_tar_gz_file_name(server, uuid);
    if tokio::fs::metadata(&file_name).await.is_ok() {
        return Ok((
            crate::config::SystemBackupsWingsArchiveFormat::TarGz,
            file_name,
        ));
    }

    let file_name = get_tar_zstd_file_name(server, uuid);
    if tokio::fs::metadata(&file_name).await.is_ok() {
        return Ok((
            crate::config::SystemBackupsWingsArchiveFormat::TarZstd,
            file_name,
        ));
    }

    let file_name = get_zip_file_name(server, uuid);
    if tokio::fs::metadata(&file_name).await.is_ok() {
        return Ok((
            crate::config::SystemBackupsWingsArchiveFormat::Zip,
            file_name,
        ));
    }

    Err(anyhow::anyhow!("No backup file found for UUID: {}", uuid))
}

pub async fn create_backup(
    server: crate::server::Server,
    uuid: uuid::Uuid,
    overrides: ignore::overrides::Override,
) -> Result<RawServerBackup, anyhow::Error> {
    let file_name = get_file_name(&server, uuid);
    let writer = tokio::fs::File::create(&file_name).await?.into_std().await;

    let archive_format = server.config.system.backups.wings.archive_format;
    let compression_level = server.config.system.backups.compression_level;
    tokio::task::spawn_blocking(move || -> Result<(), anyhow::Error> {
        match archive_format {
            crate::config::SystemBackupsWingsArchiveFormat::Tar
            | crate::config::SystemBackupsWingsArchiveFormat::TarGz
            | crate::config::SystemBackupsWingsArchiveFormat::TarZstd => {
                let writer: Box<dyn std::io::Write> = match archive_format {
                    crate::config::SystemBackupsWingsArchiveFormat::Tar => Box::new(writer),
                    crate::config::SystemBackupsWingsArchiveFormat::TarGz => {
                        Box::new(flate2::write::GzEncoder::new(
                            writer,
                            compression_level.flate2_compression_level(),
                        ))
                    }
                    crate::config::SystemBackupsWingsArchiveFormat::TarZstd => Box::new(
                        zstd::Encoder::new(writer, compression_level.zstd_compression_level())
                            .unwrap(),
                    ),
                    _ => unreachable!(),
                };
                let mut tar = tar::Builder::new(writer);

                tar.mode(tar::HeaderMode::Complete);
                tar.follow_symlinks(false);

                for entry in WalkBuilder::new(&server.filesystem.base_path)
                    .overrides(overrides)
                    .add_custom_ignore_filename(".pteroignore")
                    .follow_links(false)
                    .git_global(false)
                    .hidden(false)
                    .build()
                    .flatten()
                {
                    let path = entry.path().canonicalize()?;
                    let metadata = entry.metadata()?;

                    if let Ok(relative) = path.strip_prefix(&server.filesystem.base_path) {
                        if metadata.is_dir() {
                            tar.append_dir(relative, &path).ok();
                        } else {
                            tar.append_path_with_name(&path, relative).ok();
                        }
                    }
                }

                tar.finish()?;
                let mut inner = tar.into_inner()?;
                inner.flush()?;
            }
            crate::config::SystemBackupsWingsArchiveFormat::Zip => {
                let mut zip = zip::ZipWriter::new(writer);

                for entry in WalkBuilder::new(&server.filesystem.base_path)
                    .overrides(overrides)
                    .add_custom_ignore_filename(".pteroignore")
                    .follow_links(false)
                    .git_global(false)
                    .hidden(false)
                    .build()
                    .flatten()
                {
                    let path = entry.path().canonicalize()?;
                    let metadata = entry.metadata()?;

                    if let Ok(relative) = path.strip_prefix(&server.filesystem.base_path) {
                        let mut options: zip::write::FileOptions<'_, ()> =
                            zip::write::FileOptions::default()
                                .compression_level(Some(
                                    compression_level.flate2_compression_level().level() as i64,
                                ))
                                .unix_permissions(metadata.permissions().mode());

                        if let Ok(mtime) = metadata.modified() {
                            let mtime: chrono::DateTime<chrono::Local> =
                                chrono::DateTime::from(mtime);

                            options =
                                options.last_modified_time(zip::DateTime::from_date_and_time(
                                    mtime.year() as u16,
                                    mtime.month() as u8,
                                    mtime.day() as u8,
                                    mtime.hour() as u8,
                                    mtime.minute() as u8,
                                    mtime.second() as u8,
                                )?);
                        }

                        if metadata.is_dir() {
                            zip.add_directory(relative.to_string_lossy(), options).ok();
                        } else if metadata.is_file() {
                            zip.start_file(relative.to_string_lossy(), options)?;
                            let mut file = std::fs::File::open(&path)?;
                            std::io::copy(&mut file, &mut zip)?;
                        } else if metadata.is_symlink() {
                            let link = std::fs::read_link(&path)?;
                            zip.add_symlink(
                                relative.to_string_lossy(),
                                link.to_string_lossy(),
                                options,
                            )?;
                        }
                    }
                }

                let mut inner = zip.finish()?;
                inner.flush()?;
            }
        }

        Ok(())
    })
    .await??;

    let mut sha1 = sha1::Sha1::new();
    let mut file = tokio::fs::File::open(&file_name).await?;

    let mut buffer = [0; 8192];
    loop {
        let bytes_read = file.read(&mut buffer).await?;
        if bytes_read == 0 {
            break;
        }

        sha1.update(&buffer[..bytes_read]);
    }

    Ok(RawServerBackup {
        checksum: format!("{:x}", sha1.finalize()),
        checksum_type: "sha1".to_string(),
        size: file.metadata().await?.len(),
        successful: true,
        parts: vec![],
    })
}

pub async fn restore_backup(
    server: crate::server::Server,
    uuid: uuid::Uuid,
) -> Result<(), anyhow::Error> {
    let (file_format, file_name) = get_first_file_name(&server, uuid).await?;
    let file = tokio::fs::File::open(&file_name).await?;

    match file_format {
        crate::config::SystemBackupsWingsArchiveFormat::Tar
        | crate::config::SystemBackupsWingsArchiveFormat::TarGz
        | crate::config::SystemBackupsWingsArchiveFormat::TarZstd => {
            let reader: Box<dyn tokio::io::AsyncRead + Send> = match file_format {
                crate::config::SystemBackupsWingsArchiveFormat::Tar => Box::new(file),
                crate::config::SystemBackupsWingsArchiveFormat::TarGz => Box::new(
                    async_compression::tokio::bufread::GzipDecoder::new(BufReader::new(file)),
                ),
                crate::config::SystemBackupsWingsArchiveFormat::TarZstd => Box::new(
                    async_compression::tokio::bufread::ZstdDecoder::new(BufReader::new(file)),
                ),
                _ => unreachable!(),
            };
            let mut archive = tokio_tar::Archive::new(Box::into_pin(reader));

            let mut entries = archive.entries()?;
            while let Some(entry) = entries.next().await {
                let mut entry = entry?;
                let path = entry.path()?;

                if path.is_absolute() {
                    continue;
                }

                if server.filesystem.is_ignored_sync(
                    &path,
                    entry.header().entry_type() == tokio_tar::EntryType::Directory,
                ) {
                    continue;
                }

                let header = entry.header();
                match header.entry_type() {
                    tokio_tar::EntryType::Directory => {
                        server.filesystem.create_dir_all(path.as_ref()).await?;
                        server
                            .filesystem
                            .set_permissions(
                                path.as_ref(),
                                cap_std::fs::Permissions::from_std(Permissions::from_mode(
                                    header.mode().unwrap_or(0o755),
                                )),
                            )
                            .await?;
                    }
                    tokio_tar::EntryType::Regular => {
                        server
                            .log_daemon(format!("(restoring): {}", path.display()))
                            .await;

                        server
                            .filesystem
                            .create_dir_all(path.parent().unwrap())
                            .await?;

                        let mut writer =
                            crate::server::filesystem::writer::AsyncFileSystemWriter::new(
                                server.clone(),
                                path.to_path_buf(),
                                Some(Permissions::from_mode(header.mode().unwrap_or(0o644))),
                                header
                                    .mtime()
                                    .map(|t| {
                                        std::time::UNIX_EPOCH + std::time::Duration::from_secs(t)
                                    })
                                    .ok(),
                            )
                            .await?;

                        tokio::io::copy(&mut entry, &mut writer).await?;
                        writer.flush().await?;
                    }
                    tokio_tar::EntryType::Symlink => {
                        let link = entry.link_name().unwrap_or_default().unwrap_or_default();

                        server
                            .filesystem
                            .symlink(link, path)
                            .await
                            .unwrap_or_else(|err| {
                                tracing::debug!(
                                    "failed to create symlink from archive: {:#?}",
                                    err
                                );
                            });
                    }
                    _ => {}
                }
            }
        }
        crate::config::SystemBackupsWingsArchiveFormat::Zip => {
            let file = Arc::new(file.into_std().await);
            let filesystem = server.filesystem.base_dir().await?;
            let runtime = tokio::runtime::Handle::current();

            tokio::task::spawn_blocking(move || -> Result<(), anyhow::Error> {
                let archive = zip::ZipArchive::new(crate::server::filesystem::archive::multi_reader::MultiReader::new(file)?)?;
                let entry_index = Arc::new(AtomicUsize::new(0));

                let pool = rayon::ThreadPoolBuilder::new()
                    .num_threads(server.config.system.backups.wings.restore_threads)
                    .build()?;

                let error = Arc::new(RwLock::new(None));

                pool.in_place_scope(|scope| {
                    let error_clone = Arc::clone(&error);

                    scope.spawn_broadcast(move |_, _| {
                        let mut archive = archive.clone();
                        let runtime = runtime.clone();
                        let entry_index = Arc::clone(&entry_index);
                        let filesystem = Arc::clone(&filesystem);
                        let error_clone2 = Arc::clone(&error_clone);
                        let server = server.clone();

                        let mut run = move || -> Result<(), anyhow::Error> {
                            loop {
                                if error_clone2.read().unwrap().is_some() {
                                    return Ok(());
                                }

                                let i =
                                    entry_index.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                                if i >= archive.len() {
                                    return Ok(());
                                }

                                let mut entry = archive.by_index(i)?;
                                let path = match entry.enclosed_name() {
                                    Some(path) => path,
                                    None => continue,
                                };

                                if path.is_absolute() {
                                    continue;
                                }

                                if server
                                    .filesystem
                                    .is_ignored_sync(&path, entry.is_dir())
                                {
                                    continue;
                                }

                                if entry.is_dir() {
                                    filesystem.create_dir_all(&path)?;
                                    filesystem.set_permissions(
                                        &path,
                                        cap_std::fs::Permissions::from_std(Permissions::from_mode(
                                            entry.unix_mode().unwrap_or(0o755),
                                        )),
                                    )?;
                                } else if entry.is_file() {
                                    runtime.block_on(
                                        server
                                            .log_daemon(format!("(restoring): {}", path.display())),
                                    );

                                    if let Some(parent) = path.parent() {
                                        filesystem.create_dir_all(parent)?;
                                    }

                                    let mut writer = crate::server::filesystem::writer::FileSystemWriter::new(
                                        server.clone(),
                                        path,
                                        entry.unix_mode().map(Permissions::from_mode),
                                        crate::server::filesystem::archive::zip_entry_get_modified_time(&entry),
                                    )?;

                                    std::io::copy(&mut entry, &mut writer)?;
                                    writer.flush()?;
                                } else if entry.is_symlink() && (1..=2048).contains(&entry.size()) {
                                    let link = std::io::read_to_string(entry).unwrap_or_default();
                                    filesystem.symlink(link, path).unwrap_or_else(
                                        |err| {
                                            tracing::debug!(
                                                "failed to create symlink from archive: {:#?}",
                                                err
                                            );
                                        },
                                    );
                                }
                            }
                        };

                        if let Err(err) = run() {
                            error_clone.write().unwrap().replace(err);
                        }
                    });
                });

                if let Some(err) = error.write().unwrap().take() {
                    Err(err)
                } else {
                    Ok(())
                }
            })
            .await??;
        }
    };

    Ok(())
}

pub async fn download_backup(
    server: &crate::server::Server,
    uuid: uuid::Uuid,
) -> Result<(StatusCode, HeaderMap, Body), anyhow::Error> {
    let (file_format, file_name) = get_first_file_name(server, uuid).await?;
    let file = tokio::fs::File::open(&file_name).await?;

    let mut headers = HeaderMap::new();

    match file_format {
        crate::config::SystemBackupsWingsArchiveFormat::Tar => {
            headers.insert(
                "Content-Disposition",
                format!("attachment; filename={uuid}.tar").parse().unwrap(),
            );
            headers.insert("Content-Type", "application/x-tar".parse().unwrap());
        }
        crate::config::SystemBackupsWingsArchiveFormat::TarGz => {
            headers.insert(
                "Content-Disposition",
                format!("attachment; filename={uuid}.tar.gz")
                    .parse()
                    .unwrap(),
            );
            headers.insert("Content-Type", "application/gzip".parse().unwrap());
        }
        crate::config::SystemBackupsWingsArchiveFormat::TarZstd => {
            headers.insert(
                "Content-Disposition",
                format!("attachment; filename={uuid}.tar.zst")
                    .parse()
                    .unwrap(),
            );
            headers.insert("Content-Type", "application/zstd".parse().unwrap());
        }
        crate::config::SystemBackupsWingsArchiveFormat::Zip => {
            headers.insert(
                "Content-Disposition",
                format!("attachment; filename={uuid}.zip").parse().unwrap(),
            );
            headers.insert("Content-Type", "application/zip".parse().unwrap());
        }
    };

    headers.insert("Content-Length", file.metadata().await?.len().into());

    Ok((
        StatusCode::OK,
        headers,
        Body::from_stream(tokio_util::io::ReaderStream::new(
            tokio::io::BufReader::new(file),
        )),
    ))
}

pub async fn delete_backup(
    server: &crate::server::Server,
    uuid: uuid::Uuid,
) -> Result<(), anyhow::Error> {
    let (_, file_name) = get_first_file_name(server, uuid).await?;

    tokio::fs::remove_file(file_name).await?;

    Ok(())
}

pub async fn list_backups(
    server: &crate::server::Server,
) -> Result<Vec<uuid::Uuid>, anyhow::Error> {
    let mut backups = Vec::new();
    let path = Path::new(&server.config.system.backup_directory);

    let mut entries = tokio::fs::read_dir(path).await?;
    while let Some(entry) = entries.next_entry().await? {
        let file_name = entry.file_name();

        if let Ok(uuid) = uuid::Uuid::parse_str(
            file_name
                .to_str()
                .unwrap_or_default()
                .trim_end_matches(".tar.gz")
                .trim_end_matches(".tar.zst")
                .trim_end_matches(".tar")
                .trim_end_matches(".zip"),
        ) {
            backups.push(uuid);
        }
    }

    Ok(backups)
}
