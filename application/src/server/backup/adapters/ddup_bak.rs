use crate::{
    io::{
        compression::{CompressionLevel, writer::CompressionWriter},
        counting_reader::CountingReader,
        fixed_reader::FixedReader,
    },
    models::DirectoryEntry,
    remote::backups::RawServerBackup,
    response::ApiResponse,
    server::{
        backup::{
            Backup, BackupBrowseExt, BackupCleanExt, BackupCreateExt, BackupExt, BackupFindExt,
            BrowseBackup,
        },
        filesystem::archive::StreamableArchiveFormat,
    },
};
use axum::{body::Body, http::HeaderMap};
use cap_std::fs::Permissions;
use chrono::{Datelike, Timelike};
use ddup_bak::archive::entries::Entry;
use ignore::{WalkBuilder, overrides::OverrideBuilder};
use sha1::Digest;
use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::RwLock,
};

static REPOSITORY: RwLock<Option<Arc<ddup_bak::repository::Repository>>> = RwLock::const_new(None);

pub async fn get_repository(
    config: &crate::config::Config,
) -> Arc<ddup_bak::repository::Repository> {
    if let Some(repository) = REPOSITORY.read().await.as_ref() {
        return Arc::clone(repository);
    }

    let path = PathBuf::from(&config.system.backup_directory);
    if tokio::fs::metadata(path.join(".ddup-bak")).await.is_ok() {
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

pub struct DdupBakBackup {
    uuid: uuid::Uuid,
    archive: Arc<ddup_bak::archive::Archive>,
}

impl DdupBakBackup {
    fn tar_recursive_convert_entries(
        entry: &Entry,
        repository: &ddup_bak::repository::Repository,
        archive: &mut tar::Builder<impl Write + 'static>,
        parent_path: &Path,
    ) -> Result<(), anyhow::Error> {
        let mut entry_header = tar::Header::new_gnu();
        entry_header.set_uid(entry.owner().0 as u64);
        entry_header.set_gid(entry.owner().1 as u64);
        entry_header.set_mode(entry.mode().bits());

        entry_header.set_mtime(
            entry
                .mtime()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_secs(),
        );

        let path = parent_path.join(entry.name());

        match entry {
            Entry::Directory(dir) => {
                entry_header.set_entry_type(tar::EntryType::Directory);

                archive.append_data(&mut entry_header, &path, std::io::empty())?;

                for entry in dir.entries.iter() {
                    Self::tar_recursive_convert_entries(entry, repository, archive, &path)?;
                }
            }
            Entry::File(file) => {
                entry_header.set_entry_type(tar::EntryType::Regular);
                entry_header.set_size(file.size_real);

                let reader = FixedReader::new_with_fixed_bytes(
                    Box::new(repository.entry_reader(Entry::File(file.clone()))?),
                    file.size_real as usize,
                );

                archive.append_data(&mut entry_header, &path, reader)?;
            }
            Entry::Symlink(link) => {
                entry_header.set_entry_type(tar::EntryType::Symlink);

                archive.append_link(&mut entry_header, &path, &link.target)?;
            }
        }

        Ok(())
    }

    fn zip_recursive_convert_entries(
        entry: &Entry,
        repository: &ddup_bak::repository::Repository,
        zip: &mut zip::ZipWriter<
            zip::write::StreamWriter<tokio_util::io::SyncIoBridge<tokio::io::DuplexStream>>,
        >,
        compression_level: CompressionLevel,
        parent_path: &Path,
    ) -> Result<(), anyhow::Error> {
        let size = match entry {
            Entry::File(file) => file.size,
            _ => 0,
        };

        let mut options: zip::write::FileOptions<'_, ()> = zip::write::FileOptions::default()
            .compression_level(Some(compression_level.to_deflate_level() as i64))
            .unix_permissions(entry.mode().bits())
            .large_file(size >= u32::MAX as u64);
        {
            let mtime: chrono::DateTime<chrono::Utc> = chrono::DateTime::from(entry.mtime());

            options = options.last_modified_time(zip::DateTime::from_date_and_time(
                mtime.year() as u16,
                mtime.month() as u8,
                mtime.day() as u8,
                mtime.hour() as u8,
                mtime.minute() as u8,
                mtime.second() as u8,
            )?);
        }

        let path = parent_path.join(entry.name());

        match entry {
            Entry::Directory(dir) => {
                zip.add_directory(path.to_string_lossy(), options)?;

                for entry in dir.entries.iter() {
                    Self::zip_recursive_convert_entries(
                        entry,
                        repository,
                        zip,
                        compression_level,
                        &path,
                    )?;
                }
            }
            Entry::File(file) => {
                let mut reader = FixedReader::new_with_fixed_bytes(
                    Box::new(repository.entry_reader(Entry::File(file.clone()))?),
                    file.size_real as usize,
                );

                zip.start_file(path.to_string_lossy(), options)?;
                crate::io::copy(&mut reader, zip)?;
            }
            Entry::Symlink(link) => {
                zip.add_symlink(&link.name, &link.target, options)?;
            }
        }

        Ok(())
    }
}

#[async_trait::async_trait]
impl BackupFindExt for DdupBakBackup {
    async fn exists(
        config: &Arc<crate::config::Config>,
        uuid: uuid::Uuid,
    ) -> Result<bool, anyhow::Error> {
        let repository = get_repository(config).await;
        let path = repository.archive_path(&uuid.to_string());

        Ok(tokio::fs::metadata(&path).await.is_ok())
    }

    async fn find(
        config: &Arc<crate::config::Config>,
        uuid: uuid::Uuid,
    ) -> Result<Option<Backup>, anyhow::Error> {
        let repository = get_repository(config).await;

        if let Ok(archive) =
            tokio::task::spawn_blocking(move || repository.get_archive(&uuid.to_string())).await?
        {
            Ok(Some(Backup::DdupBak(DdupBakBackup {
                uuid,
                archive: Arc::new(archive),
            })))
        } else {
            Ok(None)
        }
    }
}

#[async_trait::async_trait]
impl BackupCreateExt for DdupBakBackup {
    async fn create(
        server: &crate::server::Server,
        uuid: uuid::Uuid,
        progress: Arc<AtomicU64>,
        total: Arc<AtomicU64>,
        ignore: ignore::gitignore::Gitignore,
        ignore_raw: String,
    ) -> Result<RawServerBackup, anyhow::Error> {
        let repository = get_repository(&server.app_state.config).await;
        let path = repository.archive_path(&uuid.to_string());

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
                while let Some(Ok((_, path))) = walker.next_entry().await {
                    let metadata = match server.filesystem.async_symlink_metadata(&path).await {
                        Ok(metadata) => metadata,
                        Err(_) => continue,
                    };

                    total.fetch_add(metadata.len(), Ordering::Relaxed);
                }

                Ok::<(), anyhow::Error>(())
            }
        };

        let server = server.clone();
        let archive_task =
            tokio::task::spawn_blocking(move || -> Result<(u64, u64), anyhow::Error> {
                let mut override_builder = OverrideBuilder::new(&server.filesystem.base_path);

                for line in ignore_raw.lines() {
                    if let Some(line) = line.trim().strip_prefix('!') {
                        override_builder.add(line).ok();
                    } else {
                        override_builder.add(&format!("!{}", line.trim())).ok();
                    }
                }

                let archive = repository.create_archive(
                    &uuid.to_string(),
                    Some(
                        WalkBuilder::new(&server.filesystem.base_path)
                            .overrides(override_builder.build()?)
                            .ignore(false)
                            .git_ignore(false)
                            .follow_links(false)
                            .git_global(false)
                            .hidden(false)
                            .build(),
                    ),
                    Some(&server.filesystem.base_path),
                    None,
                    Some({
                        let compression_format = server
                            .app_state
                            .config
                            .system
                            .backups
                            .ddup_bak
                            .compression_format;

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
                    server
                        .app_state
                        .config
                        .system
                        .backups
                        .ddup_bak
                        .create_threads,
                )?;

                repository.save()?;

                let mut total_size = 0;
                let mut total_files = 0;

                fn recursive_size(total_size: &mut u64, total_files: &mut u64, entry: Entry) {
                    match entry {
                        Entry::File(file) => {
                            *total_size += file.size_real;
                            *total_files += 1;
                        }
                        Entry::Directory(directory) => {
                            directory
                                .entries
                                .into_iter()
                                .for_each(|e| recursive_size(total_size, total_files, e));
                        }
                        Entry::Symlink(_) => {
                            *total_files += 1;
                        }
                    }
                }

                for entry in archive.entries.into_iter() {
                    recursive_size(&mut total_size, &mut total_files, entry.clone());
                }

                Ok((total_size, total_files))
            });

        let (total_size, total_files) = match tokio::join!(total_task, archive_task) {
            (Ok(()), Ok(Ok(size))) => size,
            (Err(err), _) => return Err(err),
            (_, Err(err)) => return Err(err.into()),
            (_, Ok(Err(err))) => return Err(err),
        };

        let mut sha1 = sha1::Sha1::new();
        let mut file = tokio::fs::File::open(path).await?;

        let mut buffer = vec![0; crate::BUFFER_SIZE];
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
            size: total_size,
            files: total_files,
            successful: true,
            parts: vec![],
        })
    }
}

#[async_trait::async_trait]
impl BackupExt for DdupBakBackup {
    #[inline]
    fn uuid(&self) -> uuid::Uuid {
        self.uuid
    }

    async fn download(
        &self,
        config: &Arc<crate::config::Config>,
        archive_format: StreamableArchiveFormat,
    ) -> Result<ApiResponse, anyhow::Error> {
        let repository = get_repository(config).await;

        let archive = self.archive.clone();
        let compression_level = config.system.backups.compression_level;
        let (reader, writer) = tokio::io::duplex(crate::BUFFER_SIZE);

        match archive_format {
            StreamableArchiveFormat::Zip => {
                tokio::task::spawn_blocking(move || -> Result<(), anyhow::Error> {
                    let writer = tokio_util::io::SyncIoBridge::new(writer);
                    let mut zip = zip::ZipWriter::new_stream(writer);

                    for entry in archive.entries.iter() {
                        DdupBakBackup::zip_recursive_convert_entries(
                            entry,
                            &repository,
                            &mut zip,
                            compression_level,
                            Path::new(""),
                        )?;
                    }

                    zip.finish()?;

                    Ok(())
                });
            }
            _ => {
                let writer = CompressionWriter::new(
                    tokio_util::io::SyncIoBridge::new(writer),
                    archive_format.compression_format(),
                    compression_level,
                    config.api.file_compression_threads,
                );

                tokio::task::spawn_blocking(move || -> Result<(), anyhow::Error> {
                    let mut tar = tar::Builder::new(writer);
                    tar.mode(tar::HeaderMode::Complete);

                    for entry in archive.entries.iter() {
                        DdupBakBackup::tar_recursive_convert_entries(
                            entry,
                            &repository,
                            &mut tar,
                            Path::new(""),
                        )?;
                    }

                    tar.finish()?;

                    Ok(())
                });
            }
        }

        let mut headers = HeaderMap::with_capacity(2);
        headers.insert(
            "Content-Disposition",
            format!(
                "attachment; filename={}.{}",
                self.uuid,
                archive_format.extension()
            )
            .parse()?,
        );
        headers.insert("Content-Type", archive_format.mime_type().parse()?);

        Ok(ApiResponse::new(Body::from_stream(
            tokio_util::io::ReaderStream::with_capacity(reader, crate::BUFFER_SIZE),
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
        let repository = get_repository(&server.app_state.config).await;

        let archive = self.archive.clone();

        let server = server.clone();
        tokio::task::spawn_blocking(move || -> Result<(), anyhow::Error> {
            let runtime = tokio::runtime::Handle::current();

            fn recursive_size(entry: &Entry) -> u64 {
                match entry {
                    Entry::File(file) => file.size_real,
                    Entry::Directory(directory) => {
                        directory.entries.iter().map(recursive_size).sum()
                    }
                    Entry::Symlink(_) => 0,
                }
            }

            total.store(
                archive.entries().iter().map(recursive_size).sum(),
                Ordering::SeqCst,
            );

            fn recursive_restore(
                runtime: &tokio::runtime::Handle,
                repository: &Arc<ddup_bak::repository::Repository>,
                entry: &Entry,
                path: &Path,
                server: &crate::server::Server,
                progress: &Arc<AtomicU64>,
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
                        runtime.block_on(
                            server.log_daemon(format!("(restoring): {}", path.display())),
                        );

                        if let Some(parent) = path.parent() {
                            server.filesystem.create_dir_all(parent)?;
                        }

                        let mut writer = crate::server::filesystem::writer::FileSystemWriter::new(
                            server.clone(),
                            &path,
                            Some(Permissions::from_std(file.mode.into())),
                            Some(cap_std::time::SystemTime::from_std(file.mtime)),
                        )?;
                        let reader = repository.entry_reader(Entry::File(file.clone()))?;
                        let mut reader =
                            CountingReader::new_with_bytes_read(reader, Arc::clone(progress));

                        crate::io::copy(&mut reader, &mut writer)?;
                        writer.flush()?;
                    }
                    Entry::Directory(directory) => {
                        server.filesystem.create_dir_all(&path)?;
                        server.filesystem.set_permissions(
                            &path,
                            cap_std::fs::Permissions::from_std(directory.mode.into()),
                        )?;

                        for entry in &directory.entries {
                            recursive_restore(runtime, repository, entry, &path, server, progress)?;
                        }

                        server.filesystem.set_times(&path, directory.mtime, None)?;
                    }
                    Entry::Symlink(symlink) => {
                        if let Err(err) = server.filesystem.symlink(&symlink.target, &path) {
                            tracing::debug!(path = %path.display(), "failed to create symlink from backup: {:#?}", err);
                        } else {
                            server.filesystem.set_symlink_permissions(
                                &path,
                                cap_std::fs::Permissions::from_std(symlink.mode.into()),
                            )?;
                            server.filesystem.set_times(&path, symlink.mtime, None)?;
                        }
                    }
                }

                Ok(())
            }

            for entry in archive.entries() {
                recursive_restore(
                    &runtime,
                    &repository,
                    entry,
                    Path::new("."),
                    &server,
                    &progress,
                )?;
            }

            Ok(())
        })
        .await??;

        Ok(())
    }

    async fn delete(&self, config: &Arc<crate::config::Config>) -> Result<(), anyhow::Error> {
        let repository = get_repository(config).await;

        let uuid = self.uuid;
        tokio::task::spawn_blocking(move || -> Result<(), anyhow::Error> {
            repository.delete_archive(&uuid.to_string(), None)?;
            repository.save()?;

            Ok(())
        })
        .await??;

        Ok(())
    }

    async fn browse(&self, server: &crate::server::Server) -> Result<BrowseBackup, anyhow::Error> {
        Ok(BrowseBackup::DdupBak(BrowseDdupBakBackup {
            server: server.clone(),
            archive: self.archive.clone(),
        }))
    }
}

#[async_trait::async_trait]
impl BackupCleanExt for DdupBakBackup {
    async fn clean(server: &crate::server::Server, uuid: uuid::Uuid) -> Result<(), anyhow::Error> {
        let repository = get_repository(&server.app_state.config).await;

        tokio::task::spawn_blocking(move || -> Result<(), anyhow::Error> {
            repository.delete_archive(&uuid.to_string(), None)?;
            repository.save()?;

            Ok(())
        })
        .await??;

        Ok(())
    }
}

pub struct BrowseDdupBakBackup {
    server: crate::server::Server,
    archive: Arc<ddup_bak::archive::Archive>,
}

impl BrowseDdupBakBackup {
    fn ddup_bak_entry_to_directory_entry(
        path: &Path,
        entry: &ddup_bak::archive::entries::Entry,
    ) -> DirectoryEntry {
        let size = match entry {
            ddup_bak::archive::entries::Entry::File(file) => file.size_real,
            ddup_bak::archive::entries::Entry::Directory(dir) => {
                fn recursive_size(entry: &ddup_bak::archive::entries::Entry) -> u64 {
                    match entry {
                        ddup_bak::archive::entries::Entry::File(file) => file.size_real,
                        ddup_bak::archive::entries::Entry::Directory(dir) => {
                            dir.entries.iter().map(recursive_size).sum()
                        }
                        ddup_bak::archive::entries::Entry::Symlink(link) => {
                            link.target.len() as u64
                        }
                    }
                }

                dir.entries.iter().map(recursive_size).sum()
            }
            ddup_bak::archive::entries::Entry::Symlink(link) => link.target.len() as u64,
        };

        let mime = if entry.is_directory() {
            "inode/directory"
        } else if entry.is_symlink() {
            "inode/symlink"
        } else {
            new_mime_guess::from_path(entry.name())
                .iter_raw()
                .next()
                .unwrap_or("application/octet-stream")
        };

        let mut mode_str = String::new();
        let mode = entry.mode().bits();

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
            created: chrono::DateTime::from_timestamp(0, 0).unwrap_or_default(),
            modified: chrono::DateTime::from_timestamp(
                entry
                    .mtime()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs() as i64,
                0,
            )
            .unwrap_or_default(),
            mode: mode_str,
            mode_bits: format!("{:o}", entry.mode().bits() & 0o777),
            size,
            directory: entry.is_directory(),
            file: entry.is_file(),
            symlink: entry.is_symlink(),
            mime,
        }
    }
}

#[async_trait::async_trait]
impl BackupBrowseExt for BrowseDdupBakBackup {
    async fn read_dir(
        &self,
        path: PathBuf,
        per_page: Option<usize>,
        page: usize,
        is_ignored: impl Fn(PathBuf, bool) -> bool + Send + Sync + 'static,
    ) -> Result<(usize, Vec<crate::models::DirectoryEntry>), anyhow::Error> {
        let archive = self.archive.clone();

        let entries = tokio::task::spawn_blocking(
            move || -> Result<(usize, Vec<DirectoryEntry>), anyhow::Error> {
                let entry = match archive.find_archive_entry(&path) {
                    Some(entry) => entry,
                    None => {
                        let mut directory_entries = Vec::new();
                        directory_entries.reserve_exact(
                            archive
                                .entries()
                                .iter()
                                .filter(|e| e.is_directory())
                                .count(),
                        );
                        let mut other_entries = Vec::new();
                        other_entries.reserve_exact(
                            archive
                                .entries()
                                .iter()
                                .filter(|e| !e.is_directory())
                                .count(),
                        );

                        for entry in archive.entries() {
                            if is_ignored(PathBuf::from(entry.name()), entry.is_directory()) {
                                continue;
                            }

                            if entry.is_directory() {
                                directory_entries.push(entry);
                            } else {
                                other_entries.push(entry);
                            }
                        }

                        directory_entries.sort_unstable_by(|a, b| a.name().cmp(b.name()));
                        other_entries.sort_unstable_by(|a, b| a.name().cmp(b.name()));

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
                                let path = path.join(entry.name());

                                entries.push(Self::ddup_bak_entry_to_directory_entry(&path, entry));
                            }
                        } else {
                            for entry in directory_entries
                                .into_iter()
                                .chain(other_entries.into_iter())
                            {
                                let path = path.join(entry.name());

                                entries.push(Self::ddup_bak_entry_to_directory_entry(&path, entry));
                            }
                        }

                        return Ok((total_entries, entries));
                    }
                };

                match entry {
                    ddup_bak::archive::entries::Entry::Directory(dir) => {
                        let mut directory_entries = Vec::new();
                        directory_entries
                            .reserve_exact(dir.entries.iter().filter(|e| e.is_directory()).count());
                        let mut other_entries = Vec::new();
                        other_entries.reserve_exact(
                            dir.entries.iter().filter(|e| !e.is_directory()).count(),
                        );

                        for entry in &dir.entries {
                            if is_ignored(PathBuf::from(entry.name()), entry.is_directory()) {
                                continue;
                            }

                            if entry.is_directory() {
                                directory_entries.push(entry);
                            } else {
                                other_entries.push(entry);
                            }
                        }

                        directory_entries.sort_unstable_by(|a, b| a.name().cmp(b.name()));
                        other_entries.sort_unstable_by(|a, b| a.name().cmp(b.name()));

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
                                let path = path.join(&dir.name).join(entry.name());

                                entries.push(Self::ddup_bak_entry_to_directory_entry(&path, entry));
                            }
                        } else {
                            for entry in directory_entries
                                .into_iter()
                                .chain(other_entries.into_iter())
                            {
                                let path = path.join(&dir.name).join(entry.name());

                                entries.push(Self::ddup_bak_entry_to_directory_entry(&path, entry));
                            }
                        }

                        Ok((total_entries, entries))
                    }
                    _ => Err(anyhow::anyhow!("Expected a directory entry")),
                }
            },
        )
        .await??;

        Ok(entries)
    }

    async fn read_file(
        &self,
        path: PathBuf,
    ) -> Result<(u64, Box<dyn tokio::io::AsyncRead + Unpin + Send>), anyhow::Error> {
        let archive = self.archive.clone();

        let entry = match archive.find_archive_entry(&path) {
            Some(entry) => entry,
            None => {
                return Err(anyhow::anyhow!(std::io::Error::from(
                    rustix::io::Errno::NOENT
                )));
            }
        };

        let size = match entry {
            ddup_bak::archive::entries::Entry::File(file) => file.size_real,
            _ => {
                return Err(anyhow::anyhow!(std::io::Error::from(
                    rustix::io::Errno::NOENT
                )));
            }
        };

        let repository = get_repository(&self.server.app_state.config).await;
        let mut entry_reader = repository.entry_reader(entry.clone())?;
        let (reader, mut writer) = tokio::io::duplex(crate::BUFFER_SIZE);

        tokio::task::spawn_blocking(move || {
            let runtime = tokio::runtime::Handle::current();

            let mut buffer = vec![0; crate::BUFFER_SIZE];
            loop {
                match entry_reader.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(n) => {
                        if runtime.block_on(writer.write_all(&buffer[..n])).is_err() {
                            break;
                        }
                    }
                    Err(err) => {
                        tracing::error!("error reading from ddup_bak entry: {:#?}", err);
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
        let archive = self.archive.clone();

        match archive.find_archive_entry(&path) {
            Some(ddup_bak::archive::entries::Entry::Directory(_)) | None => {}
            Some(_) => {
                return Err(anyhow::anyhow!(std::io::Error::from(
                    rustix::io::Errno::NOENT
                )));
            }
        };

        let repository = get_repository(&self.server.app_state.config).await;
        let compression_level = self
            .server
            .app_state
            .config
            .system
            .backups
            .compression_level;
        let (reader, writer) = tokio::io::duplex(crate::BUFFER_SIZE);

        match archive_format {
            StreamableArchiveFormat::Zip => {
                tokio::task::spawn_blocking(move || -> Result<(), anyhow::Error> {
                    let writer = tokio_util::io::SyncIoBridge::new(writer);
                    let mut zip = zip::ZipWriter::new_stream(writer);

                    match archive.find_archive_entry(&path) {
                        Some(entry) => {
                            let entry = match entry {
                                ddup_bak::archive::entries::Entry::Directory(entry) => entry,
                                _ => {
                                    return Err(anyhow::anyhow!(std::io::Error::from(
                                        rustix::io::Errno::NOENT
                                    )));
                                }
                            };

                            for entry in entry.entries.iter() {
                                DdupBakBackup::zip_recursive_convert_entries(
                                    entry,
                                    &repository,
                                    &mut zip,
                                    compression_level,
                                    Path::new(""),
                                )?;
                            }

                            zip.finish()?;
                        }
                        None => {
                            if path.components().count() == 0 {
                                for entry in archive.entries() {
                                    DdupBakBackup::zip_recursive_convert_entries(
                                        entry,
                                        &repository,
                                        &mut zip,
                                        compression_level,
                                        Path::new(""),
                                    )?;
                                }

                                zip.finish()?;
                            }
                        }
                    };

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

                    match archive.find_archive_entry(&path) {
                        Some(entry) => {
                            let entry = match entry {
                                ddup_bak::archive::entries::Entry::Directory(entry) => entry,
                                _ => {
                                    return Err(anyhow::anyhow!(std::io::Error::from(
                                        rustix::io::Errno::NOENT
                                    )));
                                }
                            };

                            for entry in entry.entries.iter() {
                                DdupBakBackup::tar_recursive_convert_entries(
                                    entry,
                                    &repository,
                                    &mut tar,
                                    Path::new(""),
                                )?;
                            }

                            tar.finish()?;
                        }
                        None => {
                            if path.components().count() == 0 {
                                for entry in archive.entries() {
                                    DdupBakBackup::tar_recursive_convert_entries(
                                        entry,
                                        &repository,
                                        &mut tar,
                                        Path::new(""),
                                    )?;
                                }

                                tar.finish()?;
                            }
                        }
                    };

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
        let archive = self.archive.clone();

        match archive.find_archive_entry(&path) {
            Some(ddup_bak::archive::entries::Entry::Directory(_)) | None => {}
            Some(_) => {
                return Err(anyhow::anyhow!(std::io::Error::from(
                    rustix::io::Errno::NOENT
                )));
            }
        };

        let repository = get_repository(&self.server.app_state.config).await;
        let compression_level = self
            .server
            .app_state
            .config
            .system
            .backups
            .compression_level;
        let (reader, writer) = tokio::io::duplex(crate::BUFFER_SIZE);

        match archive_format {
            StreamableArchiveFormat::Zip => {
                tokio::task::spawn_blocking(move || -> Result<(), anyhow::Error> {
                    let writer = tokio_util::io::SyncIoBridge::new(writer);
                    let mut zip = zip::ZipWriter::new_stream(writer);

                    for file in file_paths {
                        let entry = match archive.find_archive_entry(&path.join(&file)) {
                            Some(entry) => entry,
                            None => continue,
                        };

                        DdupBakBackup::zip_recursive_convert_entries(
                            entry,
                            &repository,
                            &mut zip,
                            compression_level,
                            Path::new(""),
                        )?;
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

                    for file in file_paths {
                        let entry = match archive.find_archive_entry(&path.join(&file)) {
                            Some(entry) => entry,
                            None => continue,
                        };

                        DdupBakBackup::tar_recursive_convert_entries(
                            entry,
                            &repository,
                            &mut tar,
                            Path::new(""),
                        )?;
                    }

                    Ok(())
                });
            }
        }

        Ok(reader)
    }
}
