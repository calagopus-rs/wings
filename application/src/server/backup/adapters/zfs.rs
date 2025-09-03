use crate::{
    io::counting_reader::AsyncCountingReader,
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
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};
use tokio::{io::AsyncWriteExt, process::Command};

pub struct ZfsBackup {
    server_uuid: uuid::Uuid,
    uuid: uuid::Uuid,
}

impl ZfsBackup {
    #[inline]
    fn get_backup_path(config: &crate::config::Config, uuid: uuid::Uuid) -> PathBuf {
        Path::new(&config.system.backup_directory)
            .join("zfs")
            .join(uuid.to_string())
    }

    #[inline]
    fn get_snapshot_name(uuid: uuid::Uuid) -> String {
        format!("backup-{uuid}")
    }

    #[inline]
    pub fn get_snapshot_path(
        config: &crate::config::Config,
        server_uuid: uuid::Uuid,
        uuid: uuid::Uuid,
    ) -> PathBuf {
        Path::new(&config.system.data_directory)
            .join(server_uuid.to_string())
            .join(".zfs")
            .join("snapshot")
            .join(Self::get_snapshot_name(uuid))
    }

    #[inline]
    pub fn get_dataset_path(config: &crate::config::Config, uuid: uuid::Uuid) -> PathBuf {
        Self::get_backup_path(config, uuid).join("dataset")
    }

    #[inline]
    pub fn get_ignore_path(config: &crate::config::Config, uuid: uuid::Uuid) -> PathBuf {
        Self::get_backup_path(config, uuid).join("ignored")
    }

    pub async fn get_ignore(
        config: &crate::config::Config,
        uuid: uuid::Uuid,
    ) -> Result<ignore::gitignore::Gitignore, anyhow::Error> {
        let ignored_path = Self::get_ignore_path(config, uuid);
        let mut ignore_builder = ignore::gitignore::GitignoreBuilder::new("");

        if let Ok(ignore_content) = tokio::fs::read_to_string(&ignored_path).await {
            for line in ignore_content.lines() {
                ignore_builder.add_line(None, line).ok();
            }
        }

        Ok(ignore_builder.build()?)
    }
}

#[async_trait::async_trait]
impl BackupFindExt for ZfsBackup {
    async fn exists(
        config: &Arc<crate::config::Config>,
        uuid: uuid::Uuid,
    ) -> Result<bool, anyhow::Error> {
        let path = Self::get_backup_path(config, uuid);
        Ok(tokio::fs::metadata(&path).await.is_ok())
    }

    async fn find(
        config: &Arc<crate::config::Config>,
        uuid: uuid::Uuid,
    ) -> Result<Option<Backup>, anyhow::Error> {
        if Self::exists(config, uuid).await? {
            let dataset_path = Self::get_dataset_path(config, uuid);
            let dataset = tokio::fs::read_to_string(&dataset_path).await?;
            let server_uuid = dataset
                .split_once("server-")
                .map(|(_, uuid)| uuid::Uuid::parse_str(uuid))
                .ok_or_else(|| anyhow::anyhow!("failed to parse dataset name: {}", dataset))??;

            Ok(Some(Backup::Zfs(Self { server_uuid, uuid })))
        } else {
            Ok(None)
        }
    }
}

#[async_trait::async_trait]
impl BackupCreateExt for ZfsBackup {
    async fn create(
        server: &crate::server::Server,
        uuid: uuid::Uuid,
        _progress: Arc<AtomicU64>,
        _total: Arc<AtomicU64>,
        ignore: ignore::gitignore::Gitignore,
        ignore_raw: String,
    ) -> Result<RawServerBackup, anyhow::Error> {
        let backup_path = Self::get_backup_path(&server.app_state.config, uuid);
        let snapshot_name = Self::get_snapshot_name(uuid);
        let ignored_path = Self::get_ignore_path(&server.app_state.config, uuid);

        tokio::fs::create_dir_all(Self::get_backup_path(&server.app_state.config, uuid)).await?;

        let total_task = {
            let server = server.clone();
            let ignore = ignore.clone();

            async move {
                let ignored = [ignore];

                let mut walker = server
                    .filesystem
                    .async_walk_dir(&PathBuf::from(""))
                    .await?
                    .with_ignored(&ignored);
                let mut total_size = 0;
                let mut total_files = 0;
                while let Some(Ok((_, path))) = walker.next_entry().await {
                    let metadata = match server.filesystem.async_symlink_metadata(&path).await {
                        Ok(metadata) => metadata,
                        Err(_) => continue,
                    };

                    total_size += metadata.len();
                    if !metadata.is_dir() {
                        total_files += 1;
                    }
                }

                Ok::<_, anyhow::Error>((total_size, total_files))
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

        let ((total_size, total_files), dataset_name) = tokio::try_join!(total_task, dataset_task)?;

        Ok(RawServerBackup {
            checksum: dataset_name,
            checksum_type: "zfs-subvolume".to_string(),
            size: total_size,
            files: total_files,
            successful: true,
            parts: vec![],
        })
    }
}

#[async_trait::async_trait]
impl BackupExt for ZfsBackup {
    #[inline]
    fn uuid(&self) -> uuid::Uuid {
        self.uuid
    }

    async fn download(
        &self,
        config: &Arc<crate::config::Config>,
        archive_format: StreamableArchiveFormat,
    ) -> Result<ApiResponse, anyhow::Error> {
        let snapshot_path = Self::get_snapshot_path(config, self.server_uuid, self.uuid);

        if tokio::fs::metadata(&snapshot_path).await.is_err() {
            return Err(anyhow::anyhow!(
                "zfs backup snapshot does not exist: {}",
                snapshot_path.display()
            ));
        }

        let filesystem = crate::server::filesystem::cap::CapFilesystem::new(snapshot_path).await?;
        let names = filesystem.async_read_dir_all(Path::new("")).await?;
        let ignore = Self::get_ignore(config, self.uuid).await?;

        let (reader, writer) = tokio::io::duplex(crate::BUFFER_SIZE);

        tokio::spawn({
            let config = Arc::clone(config);

            async move {
                let writer = tokio_util::io::SyncIoBridge::new(writer);

                match archive_format {
                    StreamableArchiveFormat::Zip => {
                        if let Err(err) =
                            crate::server::filesystem::archive::create::create_zip_streaming(
                                filesystem,
                                writer,
                                Path::new(""),
                                names.into_iter().map(PathBuf::from).collect(),
                                None,
                                vec![ignore],
                                crate::server::filesystem::archive::create::CreateZipOptions {
                                    compression_level: config.system.backups.compression_level,
                                },
                            )
                            .await
                        {
                            tracing::error!(
                                "failed to create zip archive for btrfs backup: {}",
                                err
                            );
                        }
                    }
                    _ => {
                        if let Err(err) = crate::server::filesystem::archive::create::create_tar(
                            filesystem,
                            writer,
                            Path::new(""),
                            names.into_iter().map(PathBuf::from).collect(),
                            None,
                            vec![ignore],
                            crate::server::filesystem::archive::create::CreateTarOptions {
                                compression_type: archive_format.compression_format(),
                                compression_level: config.system.backups.compression_level,
                                threads: config.api.file_compression_threads,
                            },
                        )
                        .await
                        {
                            tracing::error!(
                                "failed to create tar archive for btrfs backup: {}",
                                err
                            );
                        }
                    }
                }
            }
        });

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
        let snapshot_path =
            Self::get_snapshot_path(&server.app_state.config, self.server_uuid, self.uuid);

        if tokio::fs::metadata(&snapshot_path).await.is_err() {
            return Err(anyhow::anyhow!(
                "zfs backup snapshot does not exist: {}",
                snapshot_path.display()
            ));
        }

        let filesystem = crate::server::filesystem::cap::CapFilesystem::new(snapshot_path).await?;
        let ignore = Self::get_ignore(&server.app_state.config, self.uuid).await?;

        let total_task = {
            let filesystem = filesystem.clone();
            let ignore = ignore.clone();

            async move {
                let ignored = [ignore];

                let mut walker = filesystem
                    .async_walk_dir(&PathBuf::from(""))
                    .await?
                    .with_ignored(&ignored);
                while let Some(Ok((_, path))) = walker.next_entry().await {
                    let metadata = match filesystem.async_symlink_metadata(&path).await {
                        Ok(metadata) => metadata,
                        Err(_) => continue,
                    };

                    total.fetch_add(metadata.len(), Ordering::Relaxed);
                }

                Ok::<(), anyhow::Error>(())
            }
        };

        let server = server.clone();
        let restore_task = async move {
            let ignored = [ignore];

            filesystem
                .async_walk_dir(Path::new(""))
                .await?
                .with_ignored(&ignored)
                .run_multithreaded(
                    server.app_state.config.system.backups.btrfs.restore_threads,
                    Arc::new({
                        let server = server.clone();
                        let filesystem = filesystem.clone();
                        let progress = Arc::clone(&progress);

                        move |_, path: PathBuf| {
                            let server = server.clone();
                            let filesystem = filesystem.clone();
                            let progress = Arc::clone(&progress);

                            async move {
                                let metadata =
                                    match filesystem.async_symlink_metadata(&path).await {
                                        Ok(metadata) => metadata,
                                        Err(_) => return Ok(()),
                                    };

                                if metadata.is_file() {
                                    server
                                        .log_daemon(format!("(restoring): {}", path.display()))
                                        .await;

                                    if let Some(parent) = path.parent() {
                                        server.filesystem.async_create_dir_all(parent).await?;
                                    }

                                    let file = filesystem.async_open(&path).await?;
                                    let mut writer =
                                        crate::server::filesystem::writer::AsyncFileSystemWriter::new(
                                            server.clone(),
                                            &path,
                                            Some(metadata.permissions()),
                                            metadata.modified().ok(),
                                        )
                                        .await?;
                                    let mut reader = AsyncCountingReader::new_with_bytes_read(
                                        file,
                                        Arc::clone(&progress),
                                    );

                                    tokio::io::copy(&mut reader, &mut writer).await?;
                                    writer.flush().await?;
                                } else if metadata.is_dir() {
                                    server.filesystem.async_create_dir_all(&path).await?;
                                    server
                                        .filesystem
                                        .async_set_permissions(&path, metadata.permissions())
                                        .await?;
                                    if let Ok(modified_time) = metadata.modified() {
                                        server.filesystem.async_set_times(
                                            &path,
                                            modified_time.into_std(),
                                            None,
                                        ).await?;
                                    }
                                } else if metadata.is_symlink() && let Ok(target) = filesystem.async_read_link(&path).await {
                                    if let Err(err) = server.filesystem.async_symlink(&target, &path).await {
                                        tracing::debug!(path = %path.display(), "failed to create symlink from backup: {:#?}", err);
                                    } else if let Ok(modified_time) = metadata.modified() {
                                        server.filesystem.async_set_times(
                                            &path,
                                            modified_time.into_std(),
                                            None,
                                        ).await?;
                                    }
                                }

                                Ok(())
                            }
                        }
                    }),
                ).await?;

            Ok::<(), anyhow::Error>(())
        };

        let (_, _) = tokio::try_join!(total_task, restore_task)?;

        Ok(())
    }

    async fn delete(&self, config: &Arc<crate::config::Config>) -> Result<(), anyhow::Error> {
        let backup_path = Self::get_backup_path(config, self.uuid);
        let dataset_path = Self::get_dataset_path(config, self.uuid);
        let snapshot_name = Self::get_snapshot_name(self.uuid);

        if tokio::fs::metadata(&backup_path).await.is_err() {
            return Ok(());
        }

        if let Ok(dataset_name) = tokio::fs::read_to_string(dataset_path).await {
            let output = Command::new("zfs")
                .arg("destroy")
                .arg(format!("{}@{}", dataset_name.trim(), snapshot_name))
                .output()
                .await?;

            if !output.status.success() {
                return Err(anyhow::anyhow!(
                    "failed to delete zfs snapshot: {}",
                    String::from_utf8_lossy(&output.stderr)
                ));
            }
        }

        tokio::fs::remove_dir_all(backup_path).await?;

        Ok(())
    }

    async fn browse(&self, server: &crate::server::Server) -> Result<BrowseBackup, anyhow::Error> {
        let snapshot_path =
            Self::get_snapshot_path(&server.app_state.config, self.server_uuid, self.uuid);

        if tokio::fs::metadata(&snapshot_path).await.is_err() {
            return Err(anyhow::anyhow!(
                "zfs backup subvolume does not exist: {}",
                snapshot_path.display()
            ));
        }

        let filesystem = crate::server::filesystem::cap::CapFilesystem::new(snapshot_path).await?;
        let ignore = Self::get_ignore(&server.app_state.config, self.uuid).await?;

        Ok(BrowseBackup::Zfs(BrowseZfsBackup {
            server: server.clone(),
            filesystem,
            ignore,
        }))
    }
}

#[async_trait::async_trait]
impl BackupCleanExt for ZfsBackup {
    async fn clean(server: &crate::server::Server, uuid: uuid::Uuid) -> Result<(), anyhow::Error> {
        let backup_path = Self::get_backup_path(&server.app_state.config, uuid);
        let dataset_path = Self::get_dataset_path(&server.app_state.config, uuid);
        let snapshot_name = Self::get_snapshot_name(uuid);

        if tokio::fs::metadata(&backup_path).await.is_err() {
            return Ok(());
        }

        if let Ok(dataset_name) = tokio::fs::read_to_string(dataset_path).await {
            let output = Command::new("zfs")
                .arg("destroy")
                .arg(format!("{}@{}", dataset_name.trim(), snapshot_name))
                .output()
                .await?;

            if !output.status.success() {
                return Err(anyhow::anyhow!(
                    "failed to delete zfs snapshot: {}",
                    String::from_utf8_lossy(&output.stderr)
                ));
            }
        }

        tokio::fs::remove_dir_all(backup_path).await?;

        Ok(())
    }
}

pub struct BrowseZfsBackup {
    pub server: crate::server::Server,
    pub filesystem: crate::server::filesystem::cap::CapFilesystem,
    pub ignore: ignore::gitignore::Gitignore,
}

#[async_trait::async_trait]
impl BackupBrowseExt for BrowseZfsBackup {
    async fn read_dir(
        &self,
        path: PathBuf,
        per_page: Option<usize>,
        page: usize,
        is_ignored: impl Fn(PathBuf, bool) -> bool + Send + Sync + 'static,
    ) -> Result<(usize, Vec<crate::models::DirectoryEntry>), anyhow::Error> {
        let mut directory_reader = self.filesystem.async_read_dir(&path).await?;
        let mut directory_entries = Vec::new();
        let mut other_entries = Vec::new();

        while let Some(Ok((is_dir, entry))) = directory_reader.next_entry().await {
            let path = path.join(&entry);

            if self.ignore.matched(&path, is_dir).is_ignore() || is_ignored(path, is_dir) {
                continue;
            }

            if is_dir {
                directory_entries.push(entry);
            } else {
                other_entries.push(entry);
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
                let path = path.join(&entry);
                let metadata = match self.filesystem.async_symlink_metadata(&path).await {
                    Ok(metadata) => metadata,
                    Err(_) => continue,
                };

                entries.push(
                    self.server
                        .filesystem
                        .to_api_entry_cap(&self.filesystem, path, metadata)
                        .await,
                );
            }
        } else {
            for entry in directory_entries
                .into_iter()
                .chain(other_entries.into_iter())
            {
                let path = path.join(&entry);
                let metadata = match self.filesystem.async_symlink_metadata(&path).await {
                    Ok(metadata) => metadata,
                    Err(_) => continue,
                };

                entries.push(
                    self.server
                        .filesystem
                        .to_api_entry_cap(&self.filesystem, path, metadata)
                        .await,
                );
            }
        }

        Ok((total_entries, entries))
    }

    async fn read_file(
        &self,
        path: PathBuf,
    ) -> Result<(u64, Box<dyn tokio::io::AsyncRead + Unpin + Send>), anyhow::Error> {
        if self.ignore.matched(&path, false).is_ignore() {
            return Err(anyhow::anyhow!(std::io::Error::from(
                rustix::io::Errno::NOENT
            )));
        }

        let metadata = self.filesystem.async_symlink_metadata(&path).await?;
        let file = self.filesystem.async_open(path).await?;

        Ok((metadata.len(), Box::new(file)))
    }

    async fn read_directory_archive(
        &self,
        path: PathBuf,
        archive_format: StreamableArchiveFormat,
    ) -> Result<tokio::io::DuplexStream, anyhow::Error> {
        if self.ignore.matched(&path, true).is_ignore() {
            return Err(anyhow::anyhow!(std::io::Error::from(
                rustix::io::Errno::NOENT
            )));
        }

        let names = self.filesystem.async_read_dir_all(&path).await?;
        let compression_level = self
            .server
            .app_state
            .config
            .system
            .backups
            .compression_level;
        let file_compression_threads = self.server.app_state.config.api.file_compression_threads;
        let (reader, writer) = tokio::io::duplex(crate::BUFFER_SIZE);

        tokio::spawn({
            let filesystem = self.filesystem.clone();
            let ignore = self.ignore.clone();

            async move {
                let writer = tokio_util::io::SyncIoBridge::new(writer);

                match archive_format {
                    StreamableArchiveFormat::Zip => {
                        if let Err(err) =
                            crate::server::filesystem::archive::create::create_zip_streaming(
                                filesystem,
                                writer,
                                &path,
                                names.into_iter().map(PathBuf::from).collect(),
                                None,
                                vec![ignore],
                                crate::server::filesystem::archive::create::CreateZipOptions {
                                    compression_level,
                                },
                            )
                            .await
                        {
                            tracing::error!(
                                "failed to create zip archive for btrfs backup: {}",
                                err
                            );
                        }
                    }
                    _ => {
                        if let Err(err) = crate::server::filesystem::archive::create::create_tar(
                            filesystem,
                            writer,
                            &path,
                            names.into_iter().map(PathBuf::from).collect(),
                            None,
                            vec![ignore],
                            crate::server::filesystem::archive::create::CreateTarOptions {
                                compression_type: archive_format.compression_format(),
                                compression_level,
                                threads: file_compression_threads,
                            },
                        )
                        .await
                        {
                            tracing::error!(
                                "failed to create tar archive for btrfs backup: {}",
                                err
                            );
                        }
                    }
                }
            }
        });

        Ok(reader)
    }

    async fn read_files_archive(
        &self,
        path: PathBuf,
        file_paths: Vec<PathBuf>,
        archive_format: StreamableArchiveFormat,
    ) -> Result<tokio::io::DuplexStream, anyhow::Error> {
        if self.ignore.matched(&path, true).is_ignore() {
            return Err(anyhow::anyhow!(std::io::Error::from(
                rustix::io::Errno::NOENT
            )));
        }

        let compression_level = self
            .server
            .app_state
            .config
            .system
            .backups
            .compression_level;
        let file_compression_threads = self.server.app_state.config.api.file_compression_threads;
        let (reader, writer) = tokio::io::duplex(crate::BUFFER_SIZE);

        tokio::spawn({
            let filesystem = self.filesystem.clone();
            let ignore = self.ignore.clone();

            async move {
                let writer = tokio_util::io::SyncIoBridge::new(writer);

                match archive_format {
                    StreamableArchiveFormat::Zip => {
                        if let Err(err) =
                            crate::server::filesystem::archive::create::create_zip_streaming(
                                filesystem,
                                writer,
                                &path,
                                file_paths,
                                None,
                                vec![ignore],
                                crate::server::filesystem::archive::create::CreateZipOptions {
                                    compression_level,
                                },
                            )
                            .await
                        {
                            tracing::error!(
                                "failed to create zip archive for btrfs backup: {}",
                                err
                            );
                        }
                    }
                    _ => {
                        if let Err(err) = crate::server::filesystem::archive::create::create_tar(
                            filesystem,
                            writer,
                            &path,
                            file_paths,
                            None,
                            vec![ignore],
                            crate::server::filesystem::archive::create::CreateTarOptions {
                                compression_type: archive_format.compression_format(),
                                compression_level,
                                threads: file_compression_threads,
                            },
                        )
                        .await
                        {
                            tracing::error!(
                                "failed to create tar archive for btrfs backup: {}",
                                err
                            );
                        }
                    }
                }
            }
        });

        Ok(reader)
    }
}
