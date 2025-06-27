use crate::server::filesystem::archive::CompressionLevel;
use human_bytes::human_bytes;
use ignore::WalkBuilder;
use serde::Deserialize;
use sha2::Digest;
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use utoipa::ToSchema;

#[derive(Clone, Copy, ToSchema, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
#[schema(rename_all = "snake_case")]
pub enum ArchiveFormat {
    Tar,
    #[default]
    TarGz,
    TarZstd,
}

impl ArchiveFormat {
    pub fn extension(&self) -> &'static str {
        match self {
            ArchiveFormat::Tar => "tar",
            ArchiveFormat::TarGz => "tar.gz",
            ArchiveFormat::TarZstd => "tar.zst",
        }
    }
}

impl std::str::FromStr for ArchiveFormat {
    type Err = &'static str;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.ends_with(".tar") {
            Ok(ArchiveFormat::Tar)
        } else if s.ends_with(".tar.gz") {
            Ok(ArchiveFormat::TarGz)
        } else if s.ends_with(".tar.zst") {
            Ok(ArchiveFormat::TarZstd)
        } else {
            Err("Invalid archive format")
        }
    }
}

pub struct OutgoingServerTransfer {
    pub bytes_archived: Arc<AtomicU64>,

    server: super::Server,
    archive_format: ArchiveFormat,
    compression_level: CompressionLevel,
    task: Option<tokio::task::JoinHandle<()>>,
}

impl OutgoingServerTransfer {
    pub fn new(
        server: &super::Server,
        archive_format: ArchiveFormat,
        compression_level: CompressionLevel,
    ) -> Self {
        Self {
            bytes_archived: Arc::new(AtomicU64::new(0)),
            server: server.clone(),
            archive_format,
            compression_level,
            task: None,
        }
    }

    fn log(server: &super::Server, message: &str) {
        let prelude = ansi_term::Color::Yellow.bold().paint(format!(
            "{} [Transfer System] [Source Node]:",
            chrono::Local::now().format("%Y-%m-%d %H:%M:%S")
        ));

        server
            .websocket
            .send(super::websocket::WebsocketMessage::new(
                super::websocket::WebsocketEvent::ServerTransferLogs,
                &[format!("{prelude} {message}")],
            ))
            .ok();
    }

    async fn transfer_failure(server: &super::Server) {
        server
            .config
            .client
            .set_server_transfer(server.uuid, false)
            .await
            .ok();
        server.outgoing_transfer.write().await.take();

        server.transferring.store(false, Ordering::SeqCst);
        server
            .websocket
            .send(super::websocket::WebsocketMessage::new(
                super::websocket::WebsocketEvent::ServerTransferStatus,
                &["failure".to_string()],
            ))
            .ok();
    }

    pub fn start(
        &mut self,
        client: &Arc<bollard::Docker>,
        url: String,
        token: String,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let client = Arc::clone(client);
        let bytes_archived = Arc::clone(&self.bytes_archived);
        let archive_format = self.archive_format;
        let compression_level = self.compression_level;
        let server = self.server.clone();

        tracing::info!(
            server = %server.uuid,
            "starting outgoing server transfer"
        );

        let old_task = self.task.replace(tokio::spawn(async move {
            if server.state.get_state() != super::state::ServerState::Offline {
                server
                    .stop_with_kill_timeout(&client, std::time::Duration::from_secs(15))
                    .await;
            }

            Self::log(&server, "Preparing to stream server data to destination...");
            server
                .websocket
                .send(super::websocket::WebsocketMessage::new(
                    super::websocket::WebsocketEvent::ServerTransferStatus,
                    &["processing".to_string()],
                ))
                .ok();

            let (mut checksum_writer, checksum_reader) = tokio::io::duplex(256);
            let (checksummed_writer, mut checksummed_reader) = tokio::io::duplex(65536);
            let (mut writer, reader) = tokio::io::duplex(65536);
            let archive_task = tokio::task::spawn_blocking({
                let bytes_archived = Arc::clone(&bytes_archived);
                let server = Arc::clone(&server);

                move || -> Result<(), anyhow::Error> {
                    let writer = tokio_util::io::SyncIoBridge::new(checksummed_writer);
                    let writer: Box<dyn std::io::Write> = match archive_format {
                        ArchiveFormat::Tar => Box::new(writer),
                        ArchiveFormat::TarGz => Box::new(flate2::write::GzEncoder::new(
                            writer,
                            compression_level.flate2_compression_level(),
                        )),
                        ArchiveFormat::TarZstd => Box::new(
                            zstd::Encoder::new(writer, compression_level.zstd_compression_level())
                                .unwrap(),
                        ),
                    };

                    let mut tar = tar::Builder::new(writer);
                    tar.mode(tar::HeaderMode::Complete);
                    tar.follow_symlinks(false);

                    for entry in WalkBuilder::new(&server.filesystem.base_path)
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
                            .strip_prefix(&server.filesystem.base_path)
                            .unwrap_or(entry.path());

                        let metadata = match entry.metadata() {
                            Ok(metadata) => metadata,
                            Err(_) => continue,
                        };

                        if server
                            .filesystem
                            .is_ignored_sync(entry.path(), metadata.is_dir())
                        {
                            continue;
                        }

                        if metadata.is_file() {
                            bytes_archived.fetch_add(metadata.len(), Ordering::Relaxed);
                        }

                        if metadata.is_dir() {
                            tar.append_dir(path, entry.path()).ok();
                        } else {
                            tar.append_path_with_name(entry.path(), path).ok();
                        }
                    }

                    let mut inner = tar.into_inner()?;
                    inner.flush()?;

                    Ok(())
                }
            });

            let checksum_task = tokio::task::spawn(async move {
                let mut hasher = sha2::Sha256::new();

                let mut buffer = [0; 8192];
                loop {
                    let bytes_read = checksummed_reader.read(&mut buffer).await.unwrap();
                    if bytes_read == 0 {
                        break;
                    }

                    hasher.update(&buffer[..bytes_read]);
                    writer.write_all(&buffer[..bytes_read]).await.unwrap();
                }

                checksum_writer
                    .write_all(format!("{:x}", hasher.finalize()).as_bytes())
                    .await
                    .unwrap();
            });

            let progress_task = tokio::task::spawn({
                let server = server.clone();

                async move {
                    let total_bytes = server.filesystem.limiter_usage().await;
                    let mut total_n_bytes_archived = 0.0;

                    loop {
                        let bytes_archived = bytes_archived.load(Ordering::SeqCst);
                        total_n_bytes_archived += 1.0;

                        let formatted_bytes_archived = human_bytes(bytes_archived as f64);
                        let formatted_total_bytes = human_bytes(total_bytes as f64);
                        let formatted_diff =
                            human_bytes(bytes_archived as f64 / total_n_bytes_archived);
                        let formatted_percentage = format!(
                            "{:.2}%",
                            (bytes_archived as f64 / total_bytes as f64) * 100.0
                        );

                        Self::log(
                            &server,
                            &format!(
                                "Transferred {formatted_bytes_archived} of {formatted_total_bytes} ({formatted_diff}/s, {formatted_percentage})"
                            ),
                        );
                        tracing::debug!(
                            server = %server.uuid,
                            "transferred {} of {} ({}/s, {})",
                            formatted_bytes_archived,
                            formatted_total_bytes,
                            formatted_diff,
                            formatted_percentage
                        );

                        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                    }
                }
            });

            let form = reqwest::multipart::Form::new()
                .part(
                    "archive",
                    reqwest::multipart::Part::stream(reqwest::Body::wrap_stream(
                        tokio_util::io::ReaderStream::new(Box::pin(reader)),
                    ))
                    .file_name(format!("archive.{}", archive_format.extension()))
                    .mime_str("application/x-tar")
                    .unwrap(),
                )
                .part(
                    "checksum",
                    reqwest::multipart::Part::stream(reqwest::Body::wrap_stream(
                        tokio_util::io::ReaderStream::new(Box::pin(checksum_reader)),
                    ))
                    .file_name("checksum")
                    .mime_str("text/plain")
                    .unwrap(),
                );

            let client = reqwest::Client::new();
            let response = client
                .post(url)
                .header("Authorization", token)
                .multipart(form)
                .send();

            Self::log(&server, "Streaming archive to destination...");

            let (archive, _, _) = tokio::join!(archive_task, checksum_task, response);
            progress_task.abort();

            if let Ok(Err(err)) = archive {
                tracing::error!(
                    server = %server.uuid,
                    "failed to create transfer archive: {}",
                    err
                );

                Self::transfer_failure(&server).await;
                return;
            }

            Self::log(&server, "Finished streaming archive to destination.");

            server.transferring.store(false, Ordering::SeqCst);
            server
                .websocket
                .send(super::websocket::WebsocketMessage::new(
                    super::websocket::WebsocketEvent::ServerTransferStatus,
                    &["completed".to_string()],
                ))
                .ok();

            tracing::info!(
                server = %server.uuid,
                "finished outgoing server transfer"
            );
        }));

        if let Some(old_task) = old_task {
            old_task.abort();
        }

        Ok(())
    }
}

impl Drop for OutgoingServerTransfer {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}
