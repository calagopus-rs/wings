use crate::{
    io::{
        compression::{CompressionType, reader::CompressionReader, writer::CompressionWriter},
        counting_reader::CountingReader,
        limited_reader::LimitedReader,
        limited_writer::LimitedWriter,
    },
    models::DirectoryEntry,
    remote::backups::RawServerBackup,
    response::ApiResponse,
    server::{
        backup::{
            Backup, BackupBrowseExt, BackupCleanExt, BackupCreateExt, BackupExt, BackupFindExt,
            BrowseBackup,
        },
        filesystem::archive::{
            StreamableArchiveFormat, multi_reader::MultiReader, zip_entry_get_modified_time,
        },
    },
};
use axum::{body::Body, http::HeaderMap};
use cap_std::fs::{Permissions, PermissionsExt};
use sha1::Digest;
use std::{
    io::{Read, Seek, Write},
    path::{Path, PathBuf},
    sync::{
        Arc, RwLock,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub struct WingsBackup {
    uuid: uuid::Uuid,
    format: crate::config::SystemBackupsWingsArchiveFormat,

    path: PathBuf,
}

impl WingsBackup {
    #[inline]
    fn get_tar_file_name(config: &crate::config::Config, uuid: uuid::Uuid) -> PathBuf {
        Path::new(&config.system.backup_directory).join(format!("{uuid}.tar"))
    }

    #[inline]
    fn get_tar_gz_file_name(config: &crate::config::Config, uuid: uuid::Uuid) -> PathBuf {
        Path::new(&config.system.backup_directory).join(format!("{uuid}.tar.gz"))
    }

    #[inline]
    fn get_tar_zstd_file_name(config: &crate::config::Config, uuid: uuid::Uuid) -> PathBuf {
        Path::new(&config.system.backup_directory).join(format!("{uuid}.tar.zst"))
    }

    #[inline]
    fn get_zip_file_name(config: &crate::config::Config, uuid: uuid::Uuid) -> PathBuf {
        Path::new(&config.system.backup_directory).join(format!("{uuid}.zip"))
    }

    #[inline]
    fn get_file_name(config: &crate::config::Config, uuid: uuid::Uuid) -> PathBuf {
        match config.system.backups.wings.archive_format {
            crate::config::SystemBackupsWingsArchiveFormat::Tar => {
                Self::get_tar_file_name(config, uuid)
            }
            crate::config::SystemBackupsWingsArchiveFormat::TarGz => {
                Self::get_tar_gz_file_name(config, uuid)
            }
            crate::config::SystemBackupsWingsArchiveFormat::TarZstd => {
                Self::get_tar_zstd_file_name(config, uuid)
            }
            crate::config::SystemBackupsWingsArchiveFormat::Zip => {
                Self::get_zip_file_name(config, uuid)
            }
        }
    }

    #[inline]
    pub async fn get_first_file_name(
        config: &crate::config::Config,
        uuid: uuid::Uuid,
    ) -> Result<(crate::config::SystemBackupsWingsArchiveFormat, PathBuf), anyhow::Error> {
        let file_name = Self::get_tar_file_name(config, uuid);
        if tokio::fs::metadata(&file_name).await.is_ok() {
            return Ok((
                crate::config::SystemBackupsWingsArchiveFormat::Tar,
                file_name,
            ));
        }

        let file_name = Self::get_tar_gz_file_name(config, uuid);
        if tokio::fs::metadata(&file_name).await.is_ok() {
            return Ok((
                crate::config::SystemBackupsWingsArchiveFormat::TarGz,
                file_name,
            ));
        }

        let file_name = Self::get_tar_zstd_file_name(config, uuid);
        if tokio::fs::metadata(&file_name).await.is_ok() {
            return Ok((
                crate::config::SystemBackupsWingsArchiveFormat::TarZstd,
                file_name,
            ));
        }

        let file_name = Self::get_zip_file_name(config, uuid);
        if tokio::fs::metadata(&file_name).await.is_ok() {
            return Ok((
                crate::config::SystemBackupsWingsArchiveFormat::Zip,
                file_name,
            ));
        }

        Err(anyhow::anyhow!("no backup file found for backup {}", uuid))
    }
}

#[async_trait::async_trait]
impl BackupFindExt for WingsBackup {
    async fn exists(
        config: &Arc<crate::config::Config>,
        uuid: uuid::Uuid,
    ) -> Result<bool, anyhow::Error> {
        Ok(Self::get_first_file_name(config, uuid).await.is_ok())
    }

    async fn find(
        config: &Arc<crate::config::Config>,
        uuid: uuid::Uuid,
    ) -> Result<Option<Backup>, anyhow::Error> {
        if let Ok((format, path)) = Self::get_first_file_name(config, uuid).await {
            Ok(Some(Backup::Wings(Self { uuid, format, path })))
        } else {
            Ok(None)
        }
    }
}

#[async_trait::async_trait]
impl BackupCreateExt for WingsBackup {
    async fn create(
        server: &crate::server::Server,
        uuid: uuid::Uuid,
        progress: Arc<AtomicU64>,
        total: Arc<AtomicU64>,
        ignore: ignore::gitignore::Gitignore,
        _ignore_raw: String,
    ) -> Result<RawServerBackup, anyhow::Error> {
        let file_name = Self::get_file_name(&server.app_state.config, uuid);
        let file = tokio::fs::File::create(&file_name).await?.into_std().await;

        let total_task = {
            let server = server.clone();
            let ignore = ignore.clone();

            async move {
                let ignored = [ignore];

                let mut walker = server
                    .filesystem
                    .async_walk_dir(Path::new(""))
                    .await?
                    .with_ignored(&ignored);
                let mut total_files = 0;
                while let Some(Ok((_, path))) = walker.next_entry().await {
                    let metadata = match server.filesystem.async_symlink_metadata(&path).await {
                        Ok(metadata) => metadata,
                        Err(_) => continue,
                    };

                    total.fetch_add(metadata.len(), Ordering::Relaxed);
                    if !metadata.is_dir() {
                        total_files += 1;
                    }
                }

                Ok::<_, anyhow::Error>(total_files)
            }
        };

        let archive_task = async move {
            let sources = server.filesystem.async_read_dir_all(Path::new("")).await?;
            let writer = LimitedWriter::new_with_bytes_per_second(
                file,
                server.app_state.config.system.backups.write_limit * 1024 * 1024,
            );

            match server.app_state.config.system.backups.wings.archive_format {
                crate::config::SystemBackupsWingsArchiveFormat::Tar
                | crate::config::SystemBackupsWingsArchiveFormat::TarGz
                | crate::config::SystemBackupsWingsArchiveFormat::TarZstd => {
                    crate::server::filesystem::archive::create::create_tar(
                        server.filesystem.clone(),
                        writer,
                        Path::new(""),
                        sources.into_iter().map(PathBuf::from).collect(),
                        Some(progress),
                        vec![ignore],
                        crate::server::filesystem::archive::create::CreateTarOptions {
                            compression_type: match server
                                .app_state
                                .config
                                .system
                                .backups
                                .wings
                                .archive_format
                            {
                                crate::config::SystemBackupsWingsArchiveFormat::Tar => {
                                    CompressionType::None
                                }
                                crate::config::SystemBackupsWingsArchiveFormat::TarGz => {
                                    CompressionType::Gz
                                }
                                crate::config::SystemBackupsWingsArchiveFormat::TarZstd => {
                                    CompressionType::Zstd
                                }
                                _ => unreachable!(),
                            },
                            compression_level: server
                                .app_state
                                .config
                                .system
                                .backups
                                .compression_level,
                            threads: server.app_state.config.system.backups.wings.create_threads,
                        },
                    )
                    .await
                }
                crate::config::SystemBackupsWingsArchiveFormat::Zip => {
                    crate::server::filesystem::archive::create::create_zip(
                        server.filesystem.clone(),
                        writer,
                        Path::new(""),
                        sources.into_iter().map(PathBuf::from).collect(),
                        Some(progress),
                        vec![ignore],
                        crate::server::filesystem::archive::create::CreateZipOptions {
                            compression_level: server
                                .app_state
                                .config
                                .system
                                .backups
                                .compression_level,
                        },
                    )
                    .await
                }
            }
        };

        let (total_files, _) = tokio::try_join!(total_task, archive_task)?;

        let mut checksum_writer = sha1::Sha1::new();
        let mut file = tokio::fs::File::open(&file_name).await?;
        let mut buffer = vec![0; crate::BUFFER_SIZE];

        loop {
            match file.read(&mut buffer).await? {
                0 => break,
                bytes_read => checksum_writer.write_all(&buffer[..bytes_read])?,
            }
        }

        Ok(RawServerBackup {
            checksum: format!("{:x}", checksum_writer.finalize()),
            checksum_type: "sha1".to_string(),
            size: tokio::fs::metadata(file_name).await?.len(),
            files: total_files,
            successful: true,
            parts: vec![],
        })
    }
}

#[async_trait::async_trait]
impl BackupExt for WingsBackup {
    #[inline]
    fn uuid(&self) -> uuid::Uuid {
        self.uuid
    }

    async fn download(
        &self,
        _config: &Arc<crate::config::Config>,
        _archive_format: StreamableArchiveFormat,
    ) -> Result<ApiResponse, anyhow::Error> {
        let file = tokio::fs::File::open(&self.path).await?;

        let mut headers = HeaderMap::with_capacity(3);
        match self.format {
            crate::config::SystemBackupsWingsArchiveFormat::Tar => {
                headers.insert(
                    "Content-Disposition",
                    format!("attachment; filename={}.tar", self.uuid).parse()?,
                );
                headers.insert("Content-Type", "application/x-tar".parse()?);
            }
            crate::config::SystemBackupsWingsArchiveFormat::TarGz => {
                headers.insert(
                    "Content-Disposition",
                    format!("attachment; filename={}.tar.gz", self.uuid).parse()?,
                );
                headers.insert("Content-Type", "application/gzip".parse()?);
            }
            crate::config::SystemBackupsWingsArchiveFormat::TarZstd => {
                headers.insert(
                    "Content-Disposition",
                    format!("attachment; filename={}.tar.zst", self.uuid).parse()?,
                );
                headers.insert("Content-Type", "application/zstd".parse()?);
            }
            crate::config::SystemBackupsWingsArchiveFormat::Zip => {
                headers.insert(
                    "Content-Disposition",
                    format!("attachment; filename={}.zip", self.uuid).parse()?,
                );
                headers.insert("Content-Type", "application/zip".parse()?);
            }
        };

        headers.insert("Content-Length", file.metadata().await?.len().into());

        Ok(ApiResponse::new(Body::from_stream(
            tokio_util::io::ReaderStream::with_capacity(file, crate::BUFFER_SIZE),
        ))
        .with_headers(headers))
    }

    async fn restore(
        &self,
        server: &crate::server::Server,
        progress: Arc<AtomicU64>,
        total: Arc<AtomicU64>,
        _download_url: Option<String>,
    ) -> Result<(), anyhow::Error> {
        let file = tokio::fs::File::open(&self.path).await?.into_std().await;

        match self.format {
            crate::config::SystemBackupsWingsArchiveFormat::Tar
            | crate::config::SystemBackupsWingsArchiveFormat::TarGz
            | crate::config::SystemBackupsWingsArchiveFormat::TarZstd => {
                let compression_type = match self.format {
                    crate::config::SystemBackupsWingsArchiveFormat::Tar => CompressionType::None,
                    crate::config::SystemBackupsWingsArchiveFormat::TarGz => CompressionType::Gz,
                    crate::config::SystemBackupsWingsArchiveFormat::TarZstd => {
                        CompressionType::Zstd
                    }
                    _ => unreachable!(),
                };
                let runtime = tokio::runtime::Handle::current();
                let server = server.clone();

                tokio::task::spawn_blocking(move || -> Result<(), anyhow::Error> {
                    total.store(file.metadata()?.len(), Ordering::SeqCst);

                    let reader = LimitedReader::new_with_bytes_per_second(
                        file,
                        server.app_state.config.system.backups.read_limit * 1024 * 1024,
                    );
                    let reader = CountingReader::new_with_bytes_read(reader, progress);
                    let reader = CompressionReader::new(reader, compression_type);

                    let mut archive = tar::Archive::new(reader);
                    let mut directory_entries = Vec::new();
                    let mut entries = archive.entries()?;

                    let mut read_buffer = vec![0; crate::BUFFER_SIZE];
                    while let Some(Ok(mut entry)) = entries.next() {
                        let path = entry.path()?;

                        if path.is_absolute() {
                            continue;
                        }

                        let destination_path = path.as_ref();
                        let header = entry.header();

                        match header.entry_type() {
                            tar::EntryType::Directory => {
                                server.filesystem.create_dir_all(destination_path)?;
                                if let Ok(permissions) = header.mode().map(Permissions::from_mode) {
                                    server
                                        .filesystem
                                        .set_permissions(destination_path, permissions)?;
                                }

                                if let Ok(modified_time) = header.mtime() {
                                    directory_entries
                                        .push((destination_path.to_path_buf(), modified_time));
                                }
                            }
                            tar::EntryType::Regular => {
                                runtime.block_on(
                                    server.log_daemon(format!("(restoring): {}", path.display())),
                                );

                                if let Some(parent) = destination_path.parent() {
                                    server.filesystem.create_dir_all(parent)?;
                                }

                                let mut writer =
                                    crate::server::filesystem::writer::FileSystemWriter::new(
                                        server.clone(),
                                        destination_path,
                                        header.mode().map(Permissions::from_mode).ok(),
                                        header
                                            .mtime()
                                            .map(|t| {
                                                cap_std::time::SystemTime::from_std(
                                                    std::time::UNIX_EPOCH
                                                        + std::time::Duration::from_secs(t),
                                                )
                                            })
                                            .ok(),
                                    )?;

                                crate::io::copy_shared(&mut read_buffer, &mut entry, &mut writer)?;
                                writer.flush()?;
                            }
                            tar::EntryType::Symlink => {
                                let link =
                                    entry.link_name().unwrap_or_default().unwrap_or_default();

                                if let Err(err) = server.filesystem.symlink(link, destination_path)
                                {
                                    tracing::debug!(
                                        path = %destination_path.display(),
                                        "failed to create symlink from archive: {:#?}",
                                        err
                                    );
                                } else if let Ok(modified_time) = header.mtime() {
                                    server.filesystem.set_times(
                                        destination_path,
                                        std::time::UNIX_EPOCH
                                            + std::time::Duration::from_secs(modified_time),
                                        None,
                                    )?;
                                }
                            }
                            _ => {}
                        }
                    }

                    for (destination_path, modified_time) in directory_entries {
                        server.filesystem.set_times(
                            &destination_path,
                            std::time::UNIX_EPOCH + std::time::Duration::from_secs(modified_time),
                            None,
                        )?;
                    }

                    Ok(())
                })
                .await??;
            }
            crate::config::SystemBackupsWingsArchiveFormat::Zip => {
                let runtime = tokio::runtime::Handle::current();
                let server = server.clone();

                tokio::task::spawn_blocking(move || -> Result<(), anyhow::Error> {
                    let reader = MultiReader::new(Arc::new(file))?;
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
                        .num_threads(server.app_state.config.system.backups.wings.restore_threads)
                        .build()?;

                    let error = Arc::new(RwLock::new(None));

                    pool.in_place_scope(|scope| {
                        let archive = archive.clone();
                        let server = server.clone();
                        let error_clone = Arc::clone(&error);

                        scope.spawn_broadcast(move |_, _| {
                            let mut archive = archive.clone();
                            let runtime = runtime.clone();
                            let progress = Arc::clone(&progress);
                            let entry_index = Arc::clone(&entry_index);
                            let error_clone2 = Arc::clone(&error_clone);
                            let server = server.clone();

                            let mut run = move || -> Result<(), anyhow::Error> {
                                let mut read_buffer = vec![0; crate::BUFFER_SIZE];

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

                                    if entry.is_dir() {
                                        server.filesystem.create_dir_all(&path)?;
                                        server.filesystem.set_permissions(
                                            &path,
                                            Permissions::from_mode(
                                                entry.unix_mode().unwrap_or(0o755),
                                            ),
                                        )?;
                                    } else if entry.is_file() {
                                        runtime.block_on(
                                            server
                                                .log_daemon(format!("(restoring): {}", path.display())),
                                        );

                                        if let Some(parent) = path.parent() {
                                            server.filesystem.create_dir_all(parent)?;
                                        }

                                        let mut writer = crate::server::filesystem::writer::FileSystemWriter::new(
                                            server.clone(),
                                            &path,
                                            entry.unix_mode().map(Permissions::from_mode),
                                            crate::server::filesystem::archive::zip_entry_get_modified_time(&entry),
                                        )?;
                                        let mut reader = CountingReader::new_with_bytes_read(
                                            entry,
                                            Arc::clone(&progress),
                                        );

                                        if let Err(err) = crate::io::copy_shared(&mut read_buffer, &mut reader, &mut writer) {
                                            if err.kind() == std::io::ErrorKind::InvalidData {
                                                tracing::warn!(
                                                    path = %path.display(),
                                                    "corrupted backup file: {:#?}",
                                                    err
                                                );
                                            } else {
                                                Err(err)?;
                                            }
                                        }
                                        writer.flush()?;
                                    } else if entry.is_symlink() && (1..=2048).contains(&entry.size()) {
                                        let link = std::io::read_to_string(&mut entry).unwrap_or_default();

                                        if let Err(err) = server.filesystem.symlink(link, &path) {
                                            tracing::debug!(
                                                path = %path.display(),
                                                "failed to create symlink from backup: {:#?}",
                                                err
                                            );
                                        } else if let Some(modified_time) = zip_entry_get_modified_time(&entry) {
                                            server.filesystem.set_times(
                                                &path,
                                                modified_time.into_std(),
                                                None,
                                            )?;
                                        }
                                    }
                                }
                            };

                            if let Err(err) = run() {
                                error_clone.write().unwrap().replace(err);
                            }
                        });
                    });

                    for i in 0..archive.len() {
                        let entry = archive.by_index(i)?;

                        if entry.is_dir() {
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

                            if let Some(modified_time) = zip_entry_get_modified_time(&entry) {
                                server.filesystem.set_times(
                                    &path,
                                    modified_time.into_std(),
                                    None,
                                )?;
                            }
                        }
                    }

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

    async fn delete(&self, _config: &Arc<crate::config::Config>) -> Result<(), anyhow::Error> {
        tokio::fs::remove_file(&self.path).await?;

        Ok(())
    }

    async fn browse(&self, server: &crate::server::Server) -> Result<BrowseBackup, anyhow::Error> {
        match self.format {
            crate::config::SystemBackupsWingsArchiveFormat::Zip => {
                let archive = zip::ZipArchive::new(Arc::new(
                    tokio::fs::File::open(&self.path).await?.into_std().await,
                ))?;

                Ok(BrowseBackup::Wings(BrowseWingsBackup {
                    server: server.clone(),
                    archive,
                }))
            }
            _ => Err(anyhow::anyhow!(
                "this backup adapter does not support browsing files"
            )),
        }
    }
}

#[async_trait::async_trait]
impl BackupCleanExt for WingsBackup {
    async fn clean(server: &crate::server::Server, uuid: uuid::Uuid) -> Result<(), anyhow::Error> {
        let file_name = Self::get_file_name(&server.app_state.config, uuid);
        if tokio::fs::metadata(&file_name).await.is_err() {
            return Ok(());
        }

        tokio::fs::remove_file(&file_name).await?;

        Ok(())
    }
}

pub struct BrowseWingsBackup {
    server: crate::server::Server,
    archive: zip::ZipArchive<Arc<std::fs::File>>,
}

impl BrowseWingsBackup {
    fn zip_entry_to_directory_entry(
        path: &Path,
        sizes: &[(u64, PathBuf)],
        mut entry: zip::read::ZipFile<impl Read + Seek>,
    ) -> DirectoryEntry {
        let size = if entry.is_dir() {
            sizes
                .iter()
                .filter(|(_, name)| name.starts_with(path))
                .map(|(size, _)| *size)
                .sum()
        } else {
            entry.size()
        };

        let mut buffer = [0; 64];
        let buffer = if entry.read(&mut buffer).is_err() {
            None
        } else {
            Some(&buffer)
        };

        let mime = if entry.is_dir() {
            "inode/directory"
        } else if entry.is_symlink() {
            "inode/symlink"
        } else if let Some(buffer) = buffer {
            if let Some(mime) = infer::get(buffer) {
                mime.mime_type()
            } else if let Some(mime) = new_mime_guess::from_path(entry.name()).iter_raw().next() {
                mime
            } else if crate::is_valid_utf8_slice(buffer) || buffer.is_empty() {
                "text/plain"
            } else {
                "application/octet-stream"
            }
        } else {
            "application/octet-stream"
        };

        let mut mode_str = String::new();
        let mode = entry.unix_mode().unwrap_or(0o644);

        mode_str.reserve_exact(10);
        mode_str.push(match rustix::fs::FileType::from_raw_mode(mode) {
            rustix::fs::FileType::RegularFile => '-',
            rustix::fs::FileType::Directory => 'd',
            rustix::fs::FileType::Symlink => 'l',
            rustix::fs::FileType::BlockDevice => 'b',
            rustix::fs::FileType::CharacterDevice => 'c',
            rustix::fs::FileType::Socket => 's',
            rustix::fs::FileType::Fifo => 'p',
            rustix::fs::FileType::Unknown => '?',
        });

        const RWX: &str = "rwxrwxrwx";
        for i in 0..9 {
            if mode & (1 << (8 - i)) != 0 {
                mode_str.push(RWX.chars().nth(i).unwrap());
            } else {
                mode_str.push('-');
            }
        }

        DirectoryEntry {
            name: path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string(),
            created: chrono::DateTime::from_timestamp(0, 0).unwrap(),
            modified: crate::server::filesystem::archive::zip_entry_get_modified_time(&entry)
                .map(|dt| dt.into_std().into())
                .unwrap_or_default(),
            mode: mode_str,
            mode_bits: format!("{:o}", entry.unix_mode().unwrap_or(0x644) & 0o777),
            size,
            directory: entry.is_dir(),
            file: entry.is_file(),
            symlink: entry.is_symlink(),
            mime,
        }
    }
}

#[async_trait::async_trait]
impl BackupBrowseExt for BrowseWingsBackup {
    async fn read_dir(
        &self,
        path: PathBuf,
        per_page: Option<usize>,
        page: usize,
        is_ignored: impl Fn(PathBuf, bool) -> bool + Send + Sync + 'static,
    ) -> Result<(usize, Vec<crate::models::DirectoryEntry>), anyhow::Error> {
        let mut archive = self.archive.clone();

        let entries = tokio::task::spawn_blocking(
            move || -> Result<(usize, Vec<DirectoryEntry>), std::io::Error> {
                let names = archive
                    .file_names()
                    .map(|name| name.to_string())
                    .collect::<Vec<_>>();
                let sizes = names
                    .into_iter()
                    .map(|name| {
                        (
                            archive
                                .by_name(&name)
                                .map(|file| file.size())
                                .unwrap_or_default(),
                            PathBuf::from(name),
                        )
                    })
                    .collect::<Vec<_>>();

                let mut directory_entries = Vec::new();
                let mut other_entries = Vec::new();

                let path_len = path.components().count();
                for i in 0..archive.len() {
                    let entry = archive.by_index(i)?;
                    let name = match entry.enclosed_name() {
                        Some(name) => name,
                        None => continue,
                    };

                    let name_len = name.components().count();
                    if name_len < path_len
                        || !name.starts_with(&path)
                        || name == path
                        || name_len > path_len + 1
                    {
                        continue;
                    }

                    if is_ignored(name, entry.is_dir()) {
                        continue;
                    }

                    if entry.is_dir() {
                        directory_entries.push((i, entry.name().to_string()));
                    } else {
                        other_entries.push((i, entry.name().to_string()));
                    }
                }

                directory_entries.sort_unstable_by(|a, b| a.1.cmp(&b.1));
                other_entries.sort_unstable_by(|a, b| a.1.cmp(&b.1));

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
                        let entry = archive.by_index(entry.0)?;
                        let entry_path = match entry.enclosed_name() {
                            Some(name) => name,
                            None => continue,
                        };

                        entries.push(Self::zip_entry_to_directory_entry(
                            &entry_path,
                            &sizes,
                            entry,
                        ));
                    }
                } else {
                    for entry in directory_entries
                        .into_iter()
                        .chain(other_entries.into_iter())
                    {
                        let entry = archive.by_index(entry.0)?;
                        let entry_path = match entry.enclosed_name() {
                            Some(name) => name,
                            None => continue,
                        };

                        entries.push(Self::zip_entry_to_directory_entry(
                            &entry_path,
                            &sizes,
                            entry,
                        ));
                    }
                }

                Ok((total_entries, entries))
            },
        )
        .await??;

        Ok(entries)
    }

    async fn read_file(
        &self,
        path: PathBuf,
    ) -> Result<(u64, Box<dyn tokio::io::AsyncRead + Unpin + Send>), anyhow::Error> {
        let mut archive = self.archive.clone();

        let size = archive.by_name(&path.to_string_lossy())?.size();
        let (reader, mut writer) = tokio::io::duplex(crate::BUFFER_SIZE);

        tokio::task::spawn_blocking(move || {
            let runtime = tokio::runtime::Handle::current();
            let mut entry = archive.by_name(&path.to_string_lossy()).unwrap();

            let mut buffer = vec![0; crate::BUFFER_SIZE];
            loop {
                match entry.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(n) => {
                        if runtime.block_on(writer.write_all(&buffer[..n])).is_err() {
                            break;
                        }
                    }
                    Err(err) => {
                        tracing::error!("error reading from zip entry: {:#?}", err);
                        break;
                    }
                }
            }
        });

        Ok((size, Box::new(reader)))
    }

    async fn read_directory_archive(
        &self,
        path: PathBuf,
        archive_format: StreamableArchiveFormat,
    ) -> Result<tokio::io::DuplexStream, anyhow::Error> {
        let mut archive = self.archive.clone();

        let (reader, writer) = tokio::io::duplex(crate::BUFFER_SIZE);
        let compression_level = self
            .server
            .app_state
            .config
            .system
            .backups
            .compression_level;

        match archive_format {
            StreamableArchiveFormat::Zip => {
                tokio::task::spawn_blocking(move || -> Result<(), anyhow::Error> {
                    let writer = tokio_util::io::SyncIoBridge::new(writer);
                    let mut zip = zip::ZipWriter::new_stream(writer);

                    let mut read_buffer = vec![0; crate::BUFFER_SIZE];
                    for i in 0..archive.len() {
                        let mut entry = archive.by_index(i)?;
                        let name = match entry.enclosed_name() {
                            Some(name) => name,
                            None => continue,
                        };

                        let name = match name.strip_prefix(&path) {
                            Ok(name) => name,
                            Err(_) => continue,
                        };

                        if name.components().count() == 0 {
                            continue;
                        }

                        if entry.is_dir() {
                            zip.add_directory(name.to_string_lossy(), entry.options())?;
                        } else {
                            zip.start_file(name.to_string_lossy(), entry.options())?;

                            crate::io::copy_shared(&mut read_buffer, &mut entry, &mut zip)?;
                        }
                    }

                    Ok(())
                });
            }
            _ => {
                let writer = CompressionWriter::new(
                    tokio_util::io::SyncIoBridge::new(writer),
                    archive_format.compression_format(),
                    compression_level,
                    self.server.app_state.config.api.file_compression_threads,
                );

                tokio::task::spawn_blocking(move || -> Result<(), anyhow::Error> {
                    let mut tar = tar::Builder::new(writer);
                    tar.mode(tar::HeaderMode::Complete);

                    for i in 0..archive.len() {
                        let entry = archive.by_index(i)?;
                        let name = match entry.enclosed_name() {
                            Some(name) => name,
                            None => continue,
                        };

                        let name = match name.strip_prefix(&path) {
                            Ok(name) => name,
                            Err(_) => continue,
                        };

                        if name.components().count() == 0 {
                            continue;
                        }

                        let mut entry_header = tar::Header::new_gnu();
                        if let Some(mode) = entry.unix_mode() {
                            entry_header.set_mode(mode);
                        }
                        entry_header.set_mtime(
                            zip_entry_get_modified_time(&entry)
                                .map(|dt| dt.into_std().elapsed().unwrap_or_default().as_secs())
                                .unwrap_or_default(),
                        );

                        if entry.is_dir() {
                            entry_header.set_entry_type(tar::EntryType::Directory);

                            tar.append_data(&mut entry_header, name, std::io::empty())?;
                        } else if entry.is_file() {
                            entry_header.set_entry_type(tar::EntryType::Regular);
                            entry_header.set_size(entry.size());

                            tar.append_data(&mut entry_header, name, entry)?;
                        } else if entry.is_symlink() && (1..=2048).contains(&entry.size()) {
                            entry_header.set_entry_type(tar::EntryType::Symlink);

                            let link_name = std::io::read_to_string(entry)?;
                            tar.append_link(&mut entry_header, name, link_name)?;
                        }
                    }

                    Ok(())
                });
            }
        }

        Ok(reader)
    }

    async fn read_files_archive(
        &self,
        path: PathBuf,
        file_paths: Vec<PathBuf>,
        archive_format: StreamableArchiveFormat,
    ) -> Result<tokio::io::DuplexStream, anyhow::Error> {
        let mut archive = self.archive.clone();

        let (reader, writer) = tokio::io::duplex(crate::BUFFER_SIZE);
        let compression_level = self
            .server
            .app_state
            .config
            .system
            .backups
            .compression_level;

        match archive_format {
            StreamableArchiveFormat::Zip => {
                tokio::task::spawn_blocking(move || -> Result<(), anyhow::Error> {
                    let writer = tokio_util::io::SyncIoBridge::new(writer);
                    let mut zip = zip::ZipWriter::new_stream(writer);

                    let mut read_buffer = vec![0; crate::BUFFER_SIZE];
                    for i in 0..archive.len() {
                        let mut entry = archive.by_index(i)?;
                        let name = match entry.enclosed_name() {
                            Some(name) => name,
                            None => continue,
                        };

                        let name = match name.strip_prefix(&path) {
                            Ok(name) => name,
                            Err(_) => continue,
                        };

                        if !file_paths.iter().any(|p| name.starts_with(p)) {
                            continue;
                        }

                        if name.components().count() == 0 {
                            continue;
                        }

                        if entry.is_dir() {
                            zip.add_directory(name.to_string_lossy(), entry.options())?;
                        } else {
                            zip.start_file(name.to_string_lossy(), entry.options())?;

                            crate::io::copy_shared(&mut read_buffer, &mut entry, &mut zip)?;
                        }
                    }

                    Ok(())
                });
            }
            _ => {
                let writer = CompressionWriter::new(
                    tokio_util::io::SyncIoBridge::new(writer),
                    archive_format.compression_format(),
                    compression_level,
                    self.server.app_state.config.api.file_compression_threads,
                );

                tokio::task::spawn_blocking(move || -> Result<(), anyhow::Error> {
                    let mut tar = tar::Builder::new(writer);
                    tar.mode(tar::HeaderMode::Complete);

                    for i in 0..archive.len() {
                        let entry = archive.by_index(i)?;
                        let name = match entry.enclosed_name() {
                            Some(name) => name,
                            None => continue,
                        };

                        let name = match name.strip_prefix(&path) {
                            Ok(name) => name,
                            Err(_) => continue,
                        };

                        if !file_paths.iter().any(|p| name.starts_with(p)) {
                            continue;
                        }

                        if name.components().count() == 0 {
                            continue;
                        }

                        let mut entry_header = tar::Header::new_gnu();
                        if let Some(mode) = entry.unix_mode() {
                            entry_header.set_mode(mode);
                        }
                        entry_header.set_mtime(
                            crate::server::filesystem::archive::zip_entry_get_modified_time(&entry)
                                .map(|dt| dt.into_std().elapsed().unwrap_or_default().as_secs())
                                .unwrap_or_default(),
                        );

                        if entry.is_dir() {
                            entry_header.set_entry_type(tar::EntryType::Directory);

                            tar.append_data(&mut entry_header, name, std::io::empty())?;
                        } else if entry.is_file() {
                            entry_header.set_entry_type(tar::EntryType::Regular);
                            entry_header.set_size(entry.size());

                            tar.append_data(&mut entry_header, name, entry)?;
                        } else if entry.is_symlink() && (1..=2048).contains(&entry.size()) {
                            entry_header.set_entry_type(tar::EntryType::Symlink);

                            let link_name = std::io::read_to_string(entry)?;
                            tar.append_link(&mut entry_header, name, link_name)?;
                        }
                    }

                    Ok(())
                });
            }
        }

        Ok(reader)
    }
}
