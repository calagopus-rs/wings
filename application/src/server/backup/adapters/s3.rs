use crate::{
    io::{
        compression::{CompressionType, reader::CompressionReader},
        counting_reader::CountingReader,
        limited_reader::{AsyncLimitedReader, LimitedReader},
        limited_writer::LimitedWriter,
    },
    remote::backups::RawServerBackup,
    server::{
        backup::{Backup, BackupCleanExt, BackupCreateExt, BackupExt, BackupFindExt, BrowseBackup},
        filesystem::archive::StreamableArchiveFormat,
    },
};
use cap_std::fs::{Permissions, PermissionsExt};
use futures::TryStreamExt;
use sha1::Digest;
use std::{
    io::Write,
    path::{Path, PathBuf},
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    task::{Context, Poll},
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncSeekExt, AsyncWriteExt, ReadBuf},
    sync::{Mutex, OwnedMutexGuard, RwLock},
};

static CLIENT: RwLock<Option<Arc<reqwest::Client>>> = RwLock::const_new(None);

#[inline]
async fn get_client(server: &crate::server::Server) -> Arc<reqwest::Client> {
    if let Some(client) = CLIENT.read().await.as_ref() {
        return Arc::clone(client);
    }

    let client = Arc::new(
        reqwest::ClientBuilder::new()
            .timeout(std::time::Duration::from_secs(
                server
                    .app_state
                    .config
                    .system
                    .backups
                    .s3
                    .part_upload_timeout,
            ))
            .danger_accept_invalid_certs(server.app_state.config.ignore_certificate_errors)
            .build()
            .unwrap(),
    );

    *CLIENT.write().await = Some(Arc::clone(&client));
    client
}

struct BoundedReader {
    file: OwnedMutexGuard<tokio::fs::File>,
    size: u64,
    position: u64,

    bytes_written: Arc<AtomicU64>,
}

impl BoundedReader {
    async fn new_with_bytes_written(
        file: Arc<Mutex<tokio::fs::File>>,
        offset: u64,
        size: u64,
        bytes_written: Arc<AtomicU64>,
    ) -> std::io::Result<Self> {
        let mut guard = file.lock_owned().await;

        guard.seek(std::io::SeekFrom::Start(offset)).await?;

        Ok(Self {
            file: guard,
            size,
            position: 0,
            bytes_written,
        })
    }
}

impl AsyncRead for BoundedReader {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let this = self.get_mut();

        if this.position >= this.size {
            return Poll::Ready(Ok(()));
        }

        let remaining = this.size - this.position;
        let buffer_space = buf.remaining();
        let to_read = std::cmp::min(buffer_space, remaining as usize);

        let mut temp_buf = vec![0u8; to_read];

        let read_future = this.file.read(&mut temp_buf);

        match Pin::new(&mut Box::pin(read_future)).poll(cx) {
            Poll::Ready(Ok(bytes_read)) => {
                this.position += bytes_read as u64;
                this.bytes_written
                    .fetch_add(bytes_read as u64, Ordering::Relaxed);
                buf.put_slice(&temp_buf[..bytes_read]);

                Poll::Ready(Ok(()))
            }
            Poll::Ready(Err(e)) => Poll::Ready(Err(e)),
            Poll::Pending => Poll::Pending,
        }
    }
}

pub struct S3Backup {
    uuid: uuid::Uuid,
}

impl S3Backup {
    #[inline]
    fn get_file_name(config: &crate::config::Config, uuid: uuid::Uuid) -> PathBuf {
        Path::new(&config.system.backup_directory).join(format!("{uuid}.s3.tar.gz"))
    }
}

#[async_trait::async_trait]
impl BackupFindExt for S3Backup {
    async fn exists(
        config: &Arc<crate::config::Config>,
        uuid: uuid::Uuid,
    ) -> Result<bool, anyhow::Error> {
        let path = Self::get_file_name(config, uuid);
        Ok(tokio::fs::metadata(&path).await.is_ok())
    }

    async fn find(
        _config: &Arc<crate::config::Config>,
        uuid: uuid::Uuid,
    ) -> Result<Option<Backup>, anyhow::Error> {
        Ok(Some(Backup::S3(S3Backup { uuid })))
    }
}

#[async_trait::async_trait]
impl BackupCreateExt for S3Backup {
    async fn create(
        server: &crate::server::Server,
        uuid: uuid::Uuid,
        progress: Arc<AtomicU64>,
        total: Arc<AtomicU64>,
        ignore: ignore::gitignore::Gitignore,
        _ignore_raw: String,
    ) -> Result<RawServerBackup, anyhow::Error> {
        let file_name = Self::get_file_name(&server.app_state.config, uuid);
        let mut file = tokio::fs::OpenOptions::new()
            .read(true)
            .create(true)
            .write(true)
            .truncate(true)
            .open(&file_name)
            .await?;

        let (mut checksum_reader, checksum_writer) = tokio::io::duplex(crate::BUFFER_SIZE);

        let checksum_task = async {
            let mut sha1 = sha1::Sha1::new();

            let mut buffer = vec![0; crate::BUFFER_SIZE];
            loop {
                let bytes_read = checksum_reader.read(&mut buffer).await?;
                if bytes_read == 0 {
                    break;
                }

                sha1.update(&buffer[..bytes_read]);
                file.write_all(&buffer[..bytes_read]).await?;
            }

            Ok::<_, anyhow::Error>(format!("{:x}", sha1.finalize()))
        };

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

        let archive_task = async {
            let sources = server.filesystem.async_read_dir_all(Path::new("")).await?;
            let writer = tokio_util::io::SyncIoBridge::new(checksum_writer);
            let writer = LimitedWriter::new_with_bytes_per_second(
                writer,
                server.app_state.config.system.backups.write_limit * 1024 * 1024,
            );

            crate::server::filesystem::archive::create::create_tar(
                server.filesystem.clone(),
                writer,
                Path::new(""),
                sources.into_iter().map(PathBuf::from).collect(),
                Some(Arc::clone(&progress)),
                vec![ignore],
                crate::server::filesystem::archive::create::CreateTarOptions {
                    compression_type: CompressionType::Gz,
                    compression_level: server.app_state.config.system.backups.compression_level,
                    threads: server.app_state.config.system.backups.s3.create_threads,
                },
            )
            .await
        };

        let (checksum, total_files, _) = tokio::try_join!(checksum_task, total_task, archive_task)?;

        let size = file.metadata().await?.len();
        let (part_size, part_urls) = server
            .app_state
            .config
            .client
            .backup_upload_urls(uuid, size)
            .await?;

        let file = Arc::new(Mutex::new(file));

        let mut remaining_size = size;
        let mut parts = Vec::with_capacity(part_urls.len());
        for (i, url) in part_urls.into_iter().enumerate() {
            let offset = size - remaining_size;
            let part_size = std::cmp::min(remaining_size, part_size);

            let etag;
            let mut attempts = 0;
            loop {
                attempts += 1;
                if attempts > server.app_state.config.system.backups.s3.retry_limit {
                    return Err(anyhow::anyhow!(
                        "failed to upload s3 part after {} attempts",
                        server.app_state.config.system.backups.s3.retry_limit
                    ));
                }

                tracing::debug!(
                    "uploading s3 backup part {} of size {} for backup {} for {}",
                    i + 1,
                    part_size,
                    uuid,
                    server.uuid
                );

                match get_client(server)
                    .await
                    .put(&url)
                    .header("Content-Length", part_size)
                    .header("Content-Type", "application/gzip")
                    .body(reqwest::Body::wrap_stream(
                        tokio_util::io::ReaderStream::with_capacity(
                            AsyncLimitedReader::new_with_bytes_per_second(
                                BoundedReader::new_with_bytes_written(
                                    Arc::clone(&file),
                                    offset,
                                    part_size,
                                    Arc::clone(&progress),
                                )
                                .await?,
                                server.app_state.config.system.backups.write_limit * 1024 * 1024,
                            ),
                            crate::BUFFER_SIZE,
                        ),
                    ))
                    .send()
                    .await
                {
                    Ok(response) => {
                        if response.status().is_success() {
                            etag = response
                                .headers()
                                .get("ETag")
                                .and_then(|v| v.to_str().ok())
                                .unwrap_or_default()
                                .to_string();

                            break;
                        } else {
                            tracing::error!(
                                backup = %uuid,
                                server = %server.uuid,
                                "failed to upload s3 backup part {}: status code {}",
                                i + 1,
                                response.status()
                            );
                        }
                    }
                    Err(err) => {
                        tracing::error!(
                            backup = %uuid,
                            server = %server.uuid,
                            "failed to upload s3 backup part {}: {:#?}",
                            i + 1,
                            err
                        );

                        tokio::time::sleep(std::time::Duration::from_secs(attempts.pow(2))).await;
                    }
                }
            }

            parts.push(crate::remote::backups::RawServerBackupPart {
                etag,
                part_number: i + 1,
            });
            remaining_size -= part_size;
        }

        if remaining_size > 0 {
            return Err(anyhow::anyhow!("failed to upload all parts"));
        }

        tokio::fs::remove_file(&file_name).await?;

        Ok(RawServerBackup {
            checksum,
            checksum_type: "sha1".to_string(),
            size,
            files: total_files,
            successful: true,
            parts,
        })
    }
}

#[async_trait::async_trait]
impl BackupExt for S3Backup {
    #[inline]
    fn uuid(&self) -> uuid::Uuid {
        self.uuid
    }

    async fn download(
        &self,
        _config: &Arc<crate::config::Config>,
        _archive_format: StreamableArchiveFormat,
    ) -> Result<crate::response::ApiResponse, anyhow::Error> {
        Err(anyhow::anyhow!(
            "this backup adapter does not support downloads"
        ))
    }

    async fn restore(
        &self,
        server: &crate::server::Server,
        progress: Arc<AtomicU64>,
        total: Arc<AtomicU64>,
        download_url: Option<String>,
    ) -> Result<(), anyhow::Error> {
        let response = get_client(server)
            .await
            .get(download_url.unwrap())
            .send()
            .await?;
        if let Some(content_length) = response.content_length() {
            total.store(content_length, Ordering::SeqCst);
        }

        let reader = tokio_util::io::StreamReader::new(Box::pin(
            response.bytes_stream().map_err(std::io::Error::other),
        ));

        let runtime = tokio::runtime::Handle::current();
        let server = server.clone();

        tokio::task::spawn_blocking(move || -> Result<(), anyhow::Error> {
            let reader = tokio_util::io::SyncIoBridge::new(reader);
            let reader = LimitedReader::new_with_bytes_per_second(
                reader,
                server.app_state.config.system.backups.read_limit * 1024 * 1024,
            );
            let reader = CountingReader::new_with_bytes_read(reader, progress);
            let reader = CompressionReader::new(reader, CompressionType::Gz);

            let mut archive = tar::Archive::new(reader);
            let mut directory_entries = Vec::new();
            let entries = archive.entries()?;

            let mut read_buffer = vec![0; crate::BUFFER_SIZE];
            for entry in entries {
                let mut entry = entry?;
                let path = entry.path()?;

                if path.is_absolute() {
                    continue;
                }

                let header = entry.header();
                match header.entry_type() {
                    tar::EntryType::Directory => {
                        server.filesystem.create_dir_all(path.as_ref())?;
                        server
                            .filesystem
                            .set_permissions(
                                path.as_ref(),
                                Permissions::from_mode(header.mode().unwrap_or(0o755)),
                            )
                            ?;

                        if let Ok(modified_time) = header.mtime() {
                            directory_entries.push((path.to_path_buf(), modified_time));
                        }
                    }
                    tar::EntryType::Regular => {
                        runtime.block_on(
                            server
                                .log_daemon(format!("(restoring): {}", path.display()))
                        );

                        if let Some(parent) = path.parent() {
                            server.filesystem.create_dir_all(parent)?;
                        }

                        let mut writer = crate::server::filesystem::writer::FileSystemWriter::new(
                            server.clone(),
                            &path,
                            Some(Permissions::from_mode(header.mode().unwrap_or(0o644))),
                            header
                                .mtime()
                                .map(|t| {
                                    cap_std::time::SystemTime::from_std(
                                        std::time::UNIX_EPOCH + std::time::Duration::from_secs(t),
                                    )
                                })
                                .ok(),
                        )?;

                        crate::io::copy_shared(&mut read_buffer, &mut entry, &mut writer)?;
                        writer.flush()?;
                    }
                    tar::EntryType::Symlink => {
                        let link = entry.link_name().unwrap_or_default().unwrap_or_default();

                        if let Err(err) = server.filesystem.symlink(link, path.as_ref()) {
                            tracing::debug!(path = %path.display(), "failed to create symlink from backup: {:#?}", err);
                        } else if let Ok(modified_time) = header.mtime() {
                            server
                                .filesystem
                                .set_times(
                                    path.as_ref(),
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

        Ok(())
    }

    async fn delete(&self, config: &Arc<crate::config::Config>) -> Result<(), anyhow::Error> {
        let file_name = Self::get_file_name(config, self.uuid);
        if tokio::fs::metadata(&file_name).await.is_ok() {
            tokio::fs::remove_file(&file_name).await?;
        }

        Ok(())
    }

    async fn browse(&self, _server: &crate::server::Server) -> Result<BrowseBackup, anyhow::Error> {
        Err(anyhow::anyhow!(
            "this backup adapter does not support browsing files"
        ))
    }
}

#[async_trait::async_trait]
impl BackupCleanExt for S3Backup {
    async fn clean(server: &crate::server::Server, uuid: uuid::Uuid) -> Result<(), anyhow::Error> {
        let file_name = Self::get_file_name(&server.app_state.config, uuid);
        if tokio::fs::metadata(&file_name).await.is_ok() {
            tokio::fs::remove_file(&file_name).await?;
        }

        Ok(())
    }
}
