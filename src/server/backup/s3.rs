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
    sync::LazyLock,
    task::{Context, Poll},
};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncSeekExt, BufReader, ReadBuf};
use tokio_util::io::SyncIoBridge;

static CLIENT: LazyLock<reqwest::Client> = LazyLock::new(|| {
    reqwest::ClientBuilder::new()
        .timeout(std::time::Duration::from_secs(15))
        .danger_accept_invalid_certs(true)
        .build()
        .unwrap()
});

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
) -> Result<RawServerBackup, Box<dyn std::error::Error + Send + Sync>> {
    let file_name = get_file_name(&server, uuid);
    let writer = std::io::BufWriter::new(std::fs::File::create(&file_name)?);

    let compression_level = server.config.system.backups.compression_level;
    tokio::task::spawn_blocking({
        let server = server.clone();

        move || -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
            let mut tar = tar::Builder::new(flate2::write::GzEncoder::new(
                writer,
                flate2::Compression::new(compression_level.into()),
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
                return Err("Failed to upload part after 50 attempts".into());
            }

            tracing::debug!(
                "uploading s3 backup part {} of size {} for backup {} for {}",
                i + 1,
                part_size,
                uuid,
                server.uuid
            );

            match CLIENT
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
        return Err("Failed to upload all parts".into());
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
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let response = CLIENT
        .get(download_url.unwrap())
        .send()
        .await?
        .bytes_stream();
    let reader = tokio_util::io::StreamReader::new(Box::pin(
        response.map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e)),
    ));
    let reader = BufReader::with_capacity(1024 * 1024, reader);

    let runtime = tokio::runtime::Handle::current();
    tokio::task::spawn_blocking(
        move || -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
            let reader = SyncIoBridge::new(reader);
            let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(reader));

            for entry in archive.entries().unwrap() {
                let mut entry = entry.unwrap();
                let path = entry.path().unwrap();

                if path.is_absolute() {
                    continue;
                }

                let destination_path = server.filesystem.base_path.join(&path);
                if !server.filesystem.is_safe_path_sync(&destination_path) {
                    continue;
                }

                let header = entry.header();
                match header.entry_type() {
                    tar::EntryType::Directory => {
                        std::fs::create_dir_all(&destination_path).unwrap();
                        std::fs::set_permissions(
                            &destination_path,
                            Permissions::from_mode(header.mode().unwrap_or(0o755)),
                        )
                        .unwrap();
                        std::os::unix::fs::chown(
                            &destination_path,
                            header.uid().map(|u| u as u32).ok(),
                            header.gid().map(|g| g as u32).ok(),
                        )
                        .unwrap();
                    }
                    tar::EntryType::Regular => {
                        runtime.block_on(
                            server.log_daemon(format!("(restoring): {}", path.display())),
                        );

                        std::fs::create_dir_all(destination_path.parent().unwrap()).unwrap();

                        let mut writer = crate::server::filesystem::writer::FileSystemWriter::new(
                            server.clone(),
                            destination_path,
                            Some(Permissions::from_mode(header.mode().unwrap_or(0o644))),
                            header
                                .mtime()
                                .map(|t| std::time::UNIX_EPOCH + std::time::Duration::from_secs(t))
                                .ok(),
                        )
                        .unwrap();

                        std::io::copy(&mut entry, &mut writer).unwrap();
                        writer.flush().unwrap();
                    }
                    _ => {}
                }
            }

            Ok(())
        },
    )
    .await??;

    Ok(())
}

pub async fn delete_backup(
    server: &crate::server::Server,
    uuid: uuid::Uuid,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let file_name = get_file_name(server, uuid);
    if file_name.exists() {
        tokio::fs::remove_file(&file_name).await?;
    }

    Ok(())
}

pub async fn list_backups(
    server: &crate::server::Server,
) -> Result<Vec<uuid::Uuid>, Box<dyn std::error::Error + Send + Sync>> {
    let mut backups = Vec::new();
    let path = Path::new(&server.config.system.backup_directory);

    let mut entries = tokio::fs::read_dir(path).await?;
    while let Some(entry) = entries.next_entry().await? {
        let file_name = entry.file_name();

        if let Ok(uuid) = uuid::Uuid::parse_str(
            file_name
                .to_str()
                .unwrap_or_default()
                .trim_end_matches(".s3.tar.gz"),
        ) {
            backups.push(uuid);
        }
    }

    Ok(backups)
}
