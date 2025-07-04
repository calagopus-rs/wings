use crate::remote::backups::RawServerBackup;
use axum::{
    body::Body,
    http::{HeaderMap, StatusCode},
};
use ddup_bak::archive::entries::Entry;
use ignore::WalkBuilder;
use sha1::Digest;
use std::{
    io::Write,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};
use tokio::{io::AsyncReadExt, sync::RwLock};

static REPOSITORY: RwLock<Option<Arc<ddup_bak::repository::Repository>>> = RwLock::const_new(None);

pub async fn get_repository(
    server: &crate::server::Server,
) -> Arc<ddup_bak::repository::Repository> {
    if let Some(repository) = REPOSITORY.read().await.as_ref() {
        return Arc::clone(repository);
    }

    let path = PathBuf::from(&server.config.system.backup_directory);
    if path.join(".ddup-bak").exists() {
        let repository = Arc::new(
            tokio::task::spawn_blocking(move || {
                ddup_bak::repository::Repository::open(&path, None, None).unwrap()
            })
            .await
            .unwrap(),
        );
        *REPOSITORY.write().await = Some(Arc::clone(&repository));

        repository
    } else {
        let repository = Arc::new(
            tokio::task::spawn_blocking(move || {
                ddup_bak::repository::Repository::new(&path, 1024 * 1024, 0, None)
            })
            .await
            .unwrap(),
        );
        repository.save().unwrap();
        *REPOSITORY.write().await = Some(Arc::clone(&repository));

        repository
    }
}

pub async fn create_backup(
    server: crate::server::Server,
    uuid: uuid::Uuid,
    progress: Arc<AtomicU64>,
    overrides: ignore::overrides::Override,
) -> Result<RawServerBackup, anyhow::Error> {
    let repository = get_repository(&server).await;
    let path = repository.archive_path(&uuid.to_string());

    let size = tokio::task::spawn_blocking(move || -> Result<u64, anyhow::Error> {
        let archive = repository.create_archive(
            &uuid.to_string(),
            Some(
                WalkBuilder::new(&server.filesystem.base_path)
                    .overrides(overrides)
                    .add_custom_ignore_filename(".pteroignore")
                    .follow_links(false)
                    .git_global(false)
                    .hidden(false)
                    .build(),
            ),
            Some(&server.filesystem.base_path),
            None,
            Some({
                let compression_format = server.config.system.backups.ddup_bak.compression_format;

                Arc::new(move |_, metadata| {
                    progress.fetch_add(metadata.len(), Ordering::SeqCst);

                    match compression_format {
                        crate::config::SystemBackupsDdupBakCompressionFormat::None => {
                            ddup_bak::archive::CompressionFormat::None
                        }
                        crate::config::SystemBackupsDdupBakCompressionFormat::Deflate => {
                            ddup_bak::archive::CompressionFormat::Deflate
                        }
                        crate::config::SystemBackupsDdupBakCompressionFormat::Gzip => {
                            ddup_bak::archive::CompressionFormat::Gzip
                        }
                        crate::config::SystemBackupsDdupBakCompressionFormat::Brotli => {
                            ddup_bak::archive::CompressionFormat::Brotli
                        }
                    }
                })
            }),
            server.config.system.backups.ddup_bak.create_threads,
        )?;

        repository.save()?;

        fn recursive_size(entry: Entry) -> u64 {
            match entry {
                Entry::File(file) => file.size_real,
                Entry::Directory(directory) => {
                    directory.entries.into_iter().map(recursive_size).sum()
                }
                Entry::Symlink(_) => 0,
            }
        }

        Ok(archive.into_entries().into_iter().map(recursive_size).sum())
    })
    .await??;

    let mut sha1 = sha1::Sha1::new();
    let mut file = tokio::fs::File::open(path).await?;

    let mut buffer = [0; 8192];
    loop {
        let bytes_read = file.read(&mut buffer).await?;
        if bytes_read == 0 {
            break;
        }

        sha1.update(&buffer[..bytes_read]);
    }

    Ok(RawServerBackup {
        checksum: format!("{}-{:x}", file.metadata().await?.len(), sha1.finalize()),
        checksum_type: "ddup-sha1".to_string(),
        size,
        successful: true,
        parts: vec![],
    })
}

pub async fn restore_backup(
    server: crate::server::Server,
    uuid: uuid::Uuid,
) -> Result<(), anyhow::Error> {
    let repository = get_repository(&server).await;

    let runtime = tokio::runtime::Handle::current();
    tokio::task::spawn_blocking(move || -> Result<(), anyhow::Error> {
        let archive = repository.get_archive(&uuid.to_string())?;
        let filesystem = server.filesystem.sync_base_dir()?;

        fn recursive_restore(
            runtime: &tokio::runtime::Handle,
            repository: &Arc<ddup_bak::repository::Repository>,
            filesystem: &Arc<cap_std::fs::Dir>,
            entry: Entry,
            path: &Path,
            server: &crate::server::Server,
        ) -> Result<(), anyhow::Error> {
            let path = path.join(entry.name());

            if server
                .filesystem
                .is_ignored_sync(&path, entry.is_directory())
            {
                return Ok(());
            }

            match entry {
                Entry::File(file) => {
                    runtime.block_on(server.log_daemon(format!("(restoring): {}", path.display())));

                    if let Some(parent) = path.parent() {
                        if !parent.exists() {
                            std::fs::create_dir_all(parent).unwrap();
                        }
                    }

                    let mut writer = crate::server::filesystem::writer::FileSystemWriter::new(
                        server.clone(),
                        path,
                        Some(file.mode.into()),
                        Some(file.mtime),
                    )?;

                    repository.read_entry_content(Entry::File(file), &mut writer)?;
                    writer.flush()?;
                }
                Entry::Directory(directory) => {
                    filesystem.create_dir_all(&path)?;
                    filesystem.set_permissions(
                        &path,
                        cap_std::fs::Permissions::from_std(directory.mode.into()),
                    )?;

                    for entry in directory.entries {
                        recursive_restore(runtime, repository, filesystem, entry, &path, server)?;
                    }
                }
                Entry::Symlink(symlink) => {
                    filesystem
                        .symlink(&symlink.target, &path)
                        .unwrap_or_else(|err| {
                            tracing::debug!("failed to create symlink from backup: {:#?}", err);
                        });
                }
            }

            Ok(())
        }

        for entry in archive.into_entries() {
            recursive_restore(
                &runtime,
                &repository,
                &filesystem,
                entry,
                Path::new("."),
                &server,
            )?;
        }

        Ok(())
    })
    .await??;

    Ok(())
}

pub async fn download_backup(
    server: &crate::server::Server,
    uuid: uuid::Uuid,
) -> Result<(StatusCode, HeaderMap, Body), anyhow::Error> {
    let repository = get_repository(server).await;
    let archive = repository.get_archive(&uuid.to_string())?;

    let (writer, reader) = tokio::io::duplex(65536);

    tokio::task::spawn_blocking(move || {
        let writer = tokio_util::io::SyncIoBridge::new(writer);
        let writer = flate2::write::GzEncoder::new(writer, flate2::Compression::default());

        let mut tar = tar::Builder::new(writer);
        tar.mode(tar::HeaderMode::Complete);

        let exit_early = &mut false;
        for entry in archive.entries() {
            if *exit_early {
                break;
            }

            tar_recursive_convert_entries(entry, exit_early, &repository, &mut tar, "");
        }

        if !*exit_early {
            tar.finish().unwrap();
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
        Body::from_stream(tokio_util::io::ReaderStream::new(
            tokio::io::BufReader::new(reader),
        )),
    ))
}

pub fn tar_recursive_convert_entries(
    entry: &Entry,
    exit_early: &mut bool,
    repository: &ddup_bak::repository::Repository,
    archive: &mut tar::Builder<
        flate2::write::GzEncoder<tokio_util::io::SyncIoBridge<tokio::io::DuplexStream>>,
    >,
    parent_path: &str,
) {
    if *exit_early {
        return;
    }

    match entry {
        Entry::Directory(entries) => {
            let path = if parent_path.is_empty() {
                entries.name.clone()
            } else {
                format!("{}/{}", parent_path, entries.name)
            };

            let mut entry_header = tar::Header::new_gnu();
            entry_header.set_uid(entries.owner.0 as u64);
            entry_header.set_gid(entries.owner.1 as u64);
            entry_header.set_mode(entries.mode.bits());

            entry_header.set_mtime(
                entries
                    .mtime
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_secs(),
            );
            entry_header.set_entry_type(tar::EntryType::Directory);

            let dir_path = if path.ends_with('/') {
                path.clone()
            } else {
                format!("{path}/")
            };

            if archive
                .append_data(&mut entry_header, &dir_path, std::io::empty())
                .is_err()
            {
                *exit_early = true;

                return;
            }

            for entry in entries.entries.iter() {
                tar_recursive_convert_entries(entry, exit_early, repository, archive, &path);
            }
        }
        Entry::File(file) => {
            let path = if parent_path.is_empty() {
                file.name.clone()
            } else {
                format!("{}/{}", parent_path, file.name)
            };

            let mut entry_header = tar::Header::new_gnu();
            entry_header.set_uid(file.owner.0 as u64);
            entry_header.set_gid(file.owner.1 as u64);
            entry_header.set_mode(file.mode.bits());

            entry_header.set_mtime(
                file.mtime
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_secs(),
            );
            entry_header.set_entry_type(tar::EntryType::Regular);
            entry_header.set_size(file.size_real);

            let size_real = file.size_real as usize;
            let reader = FixedReader::new(
                Box::new(repository.entry_reader(Entry::File(file.clone())).unwrap()),
                size_real,
            );

            if archive
                .append_data(&mut entry_header, &path, reader)
                .is_err()
            {
                *exit_early = true;
            }
        }
        Entry::Symlink(link) => {
            let path = if parent_path.is_empty() {
                link.name.clone()
            } else {
                format!("{}/{}", parent_path, link.name)
            };

            let mut entry_header = tar::Header::new_gnu();
            entry_header.set_uid(link.owner.0 as u64);
            entry_header.set_gid(link.owner.1 as u64);
            entry_header.set_mode(link.mode.bits());

            entry_header.set_mtime(
                link.mtime
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_secs(),
            );
            entry_header.set_entry_type(tar::EntryType::Symlink);

            if archive
                .append_link(&mut entry_header, &path, &link.target)
                .is_err()
            {
                *exit_early = true;
            }
        }
    }
}

pub async fn delete_backup(
    server: &crate::server::Server,
    uuid: uuid::Uuid,
) -> Result<(), anyhow::Error> {
    let repository = get_repository(server).await;

    tokio::task::spawn_blocking(move || -> Result<(), anyhow::Error> {
        repository.delete_archive(&uuid.to_string(), None)?;
        repository.save()?;

        Ok(())
    })
    .await??;

    Ok(())
}

pub async fn list_backups(
    server: &crate::server::Server,
) -> Result<Vec<uuid::Uuid>, anyhow::Error> {
    let repository = get_repository(server).await;
    let mut backups = Vec::new();

    for archive in tokio::task::spawn_blocking(move || repository.list_archives()).await?? {
        if let Ok(uuid) = uuid::Uuid::parse_str(&archive) {
            backups.push(uuid);
        }
    }

    Ok(backups)
}

pub struct FixedReader {
    inner: Box<dyn std::io::Read>,
    size: usize,
    bytes_read: usize,
}

impl FixedReader {
    pub fn new(inner: Box<dyn std::io::Read>, size: usize) -> Self {
        FixedReader {
            inner,
            size,
            bytes_read: 0,
        }
    }
}

impl std::io::Read for FixedReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.bytes_read >= self.size {
            return Ok(0);
        }

        let remaining = self.size - self.bytes_read;
        let to_read = std::cmp::min(buf.len(), remaining);
        let bytes = self.inner.read(&mut buf[..to_read])?;

        if bytes == 0 && remaining > 0 {
            let zeros_to_write = std::cmp::min(buf.len(), remaining);
            for byte in buf.iter_mut().take(zeros_to_write) {
                *byte = 0;
            }

            self.bytes_read += zeros_to_write;
            return Ok(zeros_to_write);
        }

        self.bytes_read += bytes;

        Ok(bytes)
    }
}
