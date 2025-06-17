use crate::remote::backups::RawServerBackup;
use futures::TryStreamExt;
use ignore::WalkBuilder;
use sha1::Digest;
use std::{
    fs::Permissions,
    io::Write,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncSeekExt, BufReader, ReadBuf},
    sync::RwLock,
};
use tokio_util::io::SyncIoBridge;

static CLIENT: RwLock<Option<Arc<reqwest::Client>>> = RwLock::const_new(None);

#[inline]
async fn get_client(server: &crate::server::Server) -> Arc<reqwest::Client> {
    if let Some(client) = CLIENT.read().await.as_ref() {
        return Arc::clone(client);
    }

    let client = Arc::new(
        reqwest::ClientBuilder::new()
            .timeout(std::time::Duration::from_secs(15))
            .danger_accept_invalid_certs(server.config.ignore_certificate_errors)
            .build()
            .unwrap(),
    );

    *CLIENT.write().await = Some(Arc::clone(&client));
    client
}

struct BoundedReader {
    file: tokio::fs::File,
    size: u64,
    position: u64,
}

impl BoundedReader {
    async fn new(file: &mut tokio::fs::File, offset: u64, size: u64) -> Self {
        file.seek(std::io::SeekFrom::Start(offset)).await.unwrap();

        Self {
            file: file.try_clone().await.unwrap(),
            size,
            position: 0,
        }
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
                buf.put_slice(&temp_buf[..bytes_read]);

                Poll::Ready(Ok(()))
            }
            Poll::Ready(Err(e)) => Poll::Ready(Err(e)),
            Poll::Pending => Poll::Pending,
        }
    }
}

#[inline]
fn get_file_name(server: &crate::server::Server, uuid: uuid::Uuid) -> PathBuf {
    Path::new(&server.config.system.backup_directory).join(format!("{}.s3.tar.gz", uuid))
}

pub async fn create_backup(
    server: crate::server::Server,
    uuid: uuid::Uuid,
    overrides: ignore::overrides::Override,
) -> Result<RawServerBackup, anyhow::Error> {
    let file_name = get_file_name(&server, uuid);
    let writer = std::io::BufWriter::new(std::fs::File::create(&file_name)?);

    let compression_level = server.config.system.backups.compression_level;
    tokio::task::spawn_blocking({
        let server = server.clone();

        move || -> Result<(), anyhow::Error> {
            let mut tar = tar::Builder::new(flate2::write::GzEncoder::new(
                writer,
                compression_level.flate2_compression_level(),
            ));

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

            Ok(())
        }
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

    let size = file.metadata().await?.len();
    let (part_size, part_urls) = server.config.client.backup_upload_urls(uuid, size).await?;

    let mut remaining_size = size;
    let mut parts = Vec::with_capacity(part_urls.len());
    for (i, url) in part_urls.into_iter().enumerate() {
        let offset = size - remaining_size;
        let part_size = std::cmp::min(remaining_size, part_size);

        let etag;
        let mut attempts = 0;
        loop {
            attempts += 1;
            if attempts > 50 {
                return Err(anyhow::anyhow!("Failed to upload part after 50 attempts"));
            }

            tracing::debug!(
                "uploading s3 backup part {} of size {} for backup {} for {}",
                i + 1,
                part_size,
                uuid,
                server.uuid
            );

            match get_client(&server)
                .await
                .put(&url)
                .header("Content-Length", part_size)
                .header("Content-Type", "application/x-gzip")
                .body(reqwest::Body::wrap_stream(
                    tokio_util::io::ReaderStream::new(Box::pin(
                        BoundedReader::new(&mut file, offset, part_size).await,
                    )),
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
                    }
                }
                Err(err) => {
                    tracing::error!(
                        "failed to upload s3 backup part {} for backup {} for {}: {}",
                        i + 1,
                        uuid,
                        server.uuid,
                        err
                    );

                    tokio::time::sleep(std::time::Duration::from_secs(attempts * 2)).await;
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
        return Err(anyhow::anyhow!("Failed to upload all parts"));
    }

    tokio::fs::remove_file(&file_name).await?;

    Ok(RawServerBackup {
        checksum: format!("{:x}", sha1.finalize()),
        checksum_type: "sha1".to_string(),
        size,
        successful: true,
        parts,
    })
}

pub async fn restore_backup(
    server: crate::server::Server,
    download_url: Option<String>,
) -> Result<(), anyhow::Error> {
    let response = get_client(&server)
        .await
        .get(download_url.unwrap())
        .send()
        .await?
        .bytes_stream();
    let reader =
        tokio_util::io::StreamReader::new(Box::pin(response.map_err(std::io::Error::other)));
    let reader = BufReader::with_capacity(1024 * 1024, reader);

    let runtime = tokio::runtime::Handle::current();
    tokio::task::spawn_blocking(move || -> Result<(), anyhow::Error> {
        let reader = SyncIoBridge::new(reader);
        let filesystem = server.filesystem.sync_base_dir()?;
        let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(reader));

        for entry in archive.entries()? {
            let mut entry = entry?;
            let path = entry.path()?;

            if path.is_absolute() {
                continue;
            }

            if server.filesystem.is_ignored_sync(
                &path,
                entry.header().entry_type() == tar::EntryType::Directory,
            ) {
                continue;
            }

            let header = entry.header();
            match header.entry_type() {
                tar::EntryType::Directory => {
                    filesystem.create_dir_all(&path)?;
                    filesystem.set_permissions(
                        &path,
                        cap_std::fs::Permissions::from_std(Permissions::from_mode(
                            header.mode().unwrap_or(0o755),
                        )),
                    )?;
                }
                tar::EntryType::Regular => {
                    runtime.block_on(server.log_daemon(format!("(restoring): {}", path.display())));

                    filesystem.create_dir_all(path.parent().unwrap())?;

                    let mut writer = crate::server::filesystem::writer::FileSystemWriter::new(
                        server.clone(),
                        path.to_path_buf(),
                        Some(Permissions::from_mode(header.mode().unwrap_or(0o644))),
                        header
                            .mtime()
                            .map(|t| std::time::UNIX_EPOCH + std::time::Duration::from_secs(t))
                            .ok(),
                    )?;

                    std::io::copy(&mut entry, &mut writer)?;
                    writer.flush()?;
                }
                tar::EntryType::Symlink => {
                    let link = entry.link_name().unwrap_or_default().unwrap_or_default();

                    filesystem.symlink(link, path).unwrap_or_else(|err| {
                        tracing::debug!("failed to create symlink from archive: {:#?}", err);
                    });
                }
                _ => {}
            }
        }

        Ok(())
    })
    .await??;

    Ok(())
}

pub async fn delete_backup(
    server: &crate::server::Server,
    uuid: uuid::Uuid,
) -> Result<(), anyhow::Error> {
    let file_name = get_file_name(server, uuid);
    if file_name.exists() {
        tokio::fs::remove_file(&file_name).await?;
    }

    Ok(())
}
