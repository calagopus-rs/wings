use crate::{
    io::{
        counting_reader::{AsyncCountingReader, CountingReader},
        limited_reader::AsyncLimitedReader,
        limited_writer::{AsyncLimitedWriter, LimitedWriter},
    },
    remote::backups::RawServerBackup,
    response::ApiResponse,
    server::filesystem::archive::multi_reader::MultiReader,
};
use axum::{body::Body, http::HeaderMap};
use futures::StreamExt;
use sha1::Digest;
use std::{
    fs::Permissions,
    io::Write,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::{
        Arc, RwLock,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
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
    progress: Arc<AtomicU64>,
    total: Arc<AtomicU64>,
    ignore: ignore::gitignore::Gitignore,
) -> Result<RawServerBackup, anyhow::Error> {
    let file_name = get_file_name(&server, uuid);
    let writer = tokio::fs::File::create(&file_name).await?;

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
            while let Some(Ok((_, path))) = walker.next_entry().await {
                let metadata = match server.filesystem.symlink_metadata(&path).await {
                    Ok(metadata) => metadata,
                    Err(_) => continue,
                };

                total.fetch_add(metadata.len(), Ordering::Relaxed);
            }

            Ok::<(), anyhow::Error>(())
        }
    };

    let archive_task = async move {
        let mut directory = server.filesystem.read_dir("").await?;
        let mut sources = Vec::new();
        while let Some(Ok((_, name))) = directory.next_entry().await {
            sources.push(PathBuf::from(name));
        }

        match server.config.system.backups.wings.archive_format {
            crate::config::SystemBackupsWingsArchiveFormat::Tar
            | crate::config::SystemBackupsWingsArchiveFormat::TarGz
            | crate::config::SystemBackupsWingsArchiveFormat::TarZstd => {
                let writer = AsyncLimitedWriter::new_with_bytes_per_second(
                    writer,
                    server.config.system.backups.write_limit * 1024 * 1024,
                );

                crate::server::filesystem::archive::Archive::create_tar(
                    server.clone(),
                    writer,
                    Path::new(""),
                    sources,
                    match server.config.system.backups.wings.archive_format {
                        crate::config::SystemBackupsWingsArchiveFormat::Tar => {
                            crate::server::filesystem::archive::CompressionType::None
                        }
                        crate::config::SystemBackupsWingsArchiveFormat::TarGz => {
                            crate::server::filesystem::archive::CompressionType::Gz
                        }
                        crate::config::SystemBackupsWingsArchiveFormat::TarZstd => {
                            crate::server::filesystem::archive::CompressionType::Zstd
                        }
                        _ => unreachable!(),
                    },
                    server.config.system.backups.compression_level,
                    Some(progress),
                    &[ignore],
                )
                .await
            }
            crate::config::SystemBackupsWingsArchiveFormat::Zip => {
                let writer = writer.into_std().await;
                let writer = LimitedWriter::new_with_bytes_per_second(
                    writer,
                    server.config.system.backups.write_limit * 1024 * 1024,
                );

                crate::server::filesystem::archive::Archive::create_zip(
                    server,
                    writer,
                    PathBuf::from(""),
                    sources,
                    Some(progress),
                    vec![ignore],
                )
                .await
            }
        }
    };

    tokio::try_join!(total_task, archive_task)?;

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
    progress: Arc<AtomicU64>,
    total: Arc<AtomicU64>,
) -> Result<(), anyhow::Error> {
    let (file_format, file_name) = get_first_file_name(&server, uuid).await?;
    let file = tokio::fs::File::open(&file_name).await?;

    match file_format {
        crate::config::SystemBackupsWingsArchiveFormat::Tar
        | crate::config::SystemBackupsWingsArchiveFormat::TarGz
        | crate::config::SystemBackupsWingsArchiveFormat::TarZstd => {
            total.store(file.metadata().await?.len(), Ordering::SeqCst);

            let reader = AsyncLimitedReader::new_with_bytes_per_second(
                file,
                server.config.system.backups.read_limit * 1024 * 1024,
            );
            let reader = AsyncCountingReader::new_with_bytes_read(reader, progress);
            let reader: Box<dyn tokio::io::AsyncRead + Send + Unpin> = match file_format {
                crate::config::SystemBackupsWingsArchiveFormat::Tar => Box::new(reader),
                crate::config::SystemBackupsWingsArchiveFormat::TarGz => Box::new(
                    async_compression::tokio::bufread::GzipDecoder::new(BufReader::new(reader)),
                ),
                crate::config::SystemBackupsWingsArchiveFormat::TarZstd => Box::new(
                    async_compression::tokio::bufread::ZstdDecoder::new(BufReader::new(reader)),
                ),
                _ => unreachable!(),
            };
            let mut archive = tokio_tar::Archive::new(reader);

            let mut entries = archive.entries()?;
            while let Some(entry) = entries.next().await {
                let mut entry = entry?;
                let path = entry.path()?;

                if path.is_absolute() {
                    continue;
                }

                if server
                    .filesystem
                    .is_ignored(
                        &path,
                        entry.header().entry_type() == tokio_tar::EntryType::Directory,
                    )
                    .await
                {
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

                        if let Some(parent) = path.parent() {
                            server.filesystem.create_dir_all(parent).await?;
                        }

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
                let reader = MultiReader::new(file)?;
                let mut archive = zip::ZipArchive::new(reader)?;
                let entry_index = Arc::new(AtomicUsize::new(0));

                for i in 0..archive.len() {
                    let entry = archive.by_index(i)?;

                    if entry.enclosed_name().is_none() {
                        continue;
                    }

                    total.fetch_add(entry.size(), Ordering::SeqCst);
                }

                let pool = rayon::ThreadPoolBuilder::new()
                    .num_threads(server.config.system.backups.wings.restore_threads)
                    .build()?;

                let error = Arc::new(RwLock::new(None));

                pool.in_place_scope(|scope| {
                    let error_clone = Arc::clone(&error);

                    scope.spawn_broadcast(move |_, _| {
                        let mut archive = archive.clone();
                        let runtime = runtime.clone();
                        let progress = Arc::clone(&progress);
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

                                let entry = archive.by_index(i)?;
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
                                    let mut reader = CountingReader::new_with_bytes_read(
                                        entry,
                                        Arc::clone(&progress),
                                    );

                                    std::io::copy(&mut reader, &mut writer)?;
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
) -> Result<ApiResponse, anyhow::Error> {
    let (file_format, file_name) = get_first_file_name(server, uuid).await?;
    let file = tokio::fs::File::open(&file_name).await?;

    let mut headers = HeaderMap::with_capacity(3);
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

    Ok(ApiResponse::new(Body::from_stream(
        tokio_util::io::ReaderStream::with_capacity(file, crate::BUFFER_SIZE),
    ))
    .with_headers(headers))
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
