use colored::Colorize;
use human_bytes::human_bytes;
use ignore::WalkBuilder;
use sha2::Digest;
use std::{
    os::unix::fs::MetadataExt,
    sync::{Arc, atomic::AtomicU64},
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub struct OutgoingServerTransfer {
    pub bytes_archived: Arc<AtomicU64>,

    server: super::Server,
    task: Option<tokio::task::JoinHandle<()>>,
}

impl OutgoingServerTransfer {
    pub fn new(server: &super::Server) -> Self {
        Self {
            bytes_archived: Arc::new(AtomicU64::new(0)),
            server: server.clone(),
            task: None,
        }
    }

    fn log(server: &super::Server, message: &str) {
        server
            .websocket
            .send(super::websocket::WebsocketMessage::new(
                super::websocket::WebsocketEvent::ServerTransferLogs,
                &[format!(
                    "{} {}",
                    format!(
                        "{} [Transfer System] [Source Node]:",
                        chrono::Local::now().format("%Y-%m-%d %H:%M:%S")
                    )
                    .yellow()
                    .bold(),
                    message
                )],
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

        server
            .transferring
            .store(false, std::sync::atomic::Ordering::SeqCst);
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
        let server = self.server.clone();

        self.task.replace(tokio::spawn(async move {
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

                move || {
                    let writer = tokio_util::io::SyncIoBridge::new(checksummed_writer);
                    let writer = flate2::write::GzEncoder::new(writer, flate2::Compression::fast());

                    let mut tar = tar::Builder::new(writer);
                    tar.mode(tar::HeaderMode::Complete);

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
                        if path.display().to_string().is_empty() {
                            continue;
                        }

                        let metadata = match entry.metadata() {
                            Ok(metadata) => metadata,
                            Err(_) => {
                                continue;
                            }
                        };

                        if server
                            .filesystem
                            .is_ignored(entry.path(), metadata.is_dir())
                        {
                            continue;
                        }

                        if metadata.is_dir() {
                            let mut entry_header = tar::Header::new_gnu();
                            entry_header.set_mode(metadata.mode());
                            entry_header.set_mtime(metadata.mtime() as u64);
                            entry_header.set_entry_type(tar::EntryType::Directory);

                            if tar
                                .append_data(&mut entry_header, path, std::io::empty())
                                .is_err()
                            {
                                break;
                            }
                        } else if metadata.is_file() {
                            let mut entry_header = tar::Header::new_gnu();
                            entry_header.set_mode(metadata.mode());
                            entry_header.set_entry_type(tar::EntryType::Regular);
                            entry_header.set_mtime(metadata.mtime() as u64);
                            entry_header.set_size(metadata.len());

                            let file = std::fs::File::open(entry.path()).unwrap();
                            bytes_archived.fetch_add(
                                file.metadata().unwrap().len(),
                                std::sync::atomic::Ordering::Relaxed,
                            );

                            if tar.append_data(&mut entry_header, path, file).is_err() {
                                break;
                            }
                        } else {
                            let mut entry_header = tar::Header::new_gnu();
                            entry_header.set_mode(metadata.mode());
                            entry_header.set_mtime(metadata.mtime() as u64);
                            entry_header.set_entry_type(tar::EntryType::Symlink);

                            if tar
                                .append_link(&mut entry_header, path, entry.path())
                                .is_err()
                            {
                                break;
                            }
                        }
                    }

                    tar.finish()
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
                    let total_bytes = server.filesystem.cached_usage();
                    let mut last_bytes_archived = 0;

                    loop {
                        let bytes_archived =
                            bytes_archived.load(std::sync::atomic::Ordering::SeqCst);
                        let diff = bytes_archived - last_bytes_archived;
                        last_bytes_archived = bytes_archived;

                        let formatted_bytes_archived = human_bytes(bytes_archived as f64);
                        let formatted_total_bytes = human_bytes(total_bytes as f64);
                        let formatted_diff = human_bytes(diff as f64);
                        let formatted_percentage = format!(
                            "{:.2}%",
                            (bytes_archived as f64 / total_bytes as f64) * 100.0
                        );

                        Self::log(
                            &server,
                            &format!(
                                "Transferred {} of {} ({}/s, {})",
                                formatted_bytes_archived,
                                formatted_total_bytes,
                                formatted_diff,
                                formatted_percentage
                            ),
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
                    .file_name("archive.tar.gz")
                    .mime_str("application/gzip")
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
                crate::logger::log(
                    crate::logger::LoggerLevel::Error,
                    format!("Failed to create transfer archive: {}", err),
                );

                Self::transfer_failure(&server).await;
                return;
            }

            Self::log(&server, "Finished streaming archive to destination.");

            server
                .transferring
                .store(false, std::sync::atomic::Ordering::SeqCst);
            server
                .websocket
                .send(super::websocket::WebsocketMessage::new(
                    super::websocket::WebsocketEvent::ServerTransferStatus,
                    &["completed".to_string()],
                ))
                .ok();
        }));

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
