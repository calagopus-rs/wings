use chrono::Local;
use colored::Colorize;
use ignore::WalkBuilder;
use sha2::Digest;
use std::{
    os::unix::fs::MetadataExt,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};
use tokio::io::{AsyncReadExt, AsyncSeekExt};

pub struct OutgoingServerTransfer {
    pub bytes_sent: Arc<AtomicU64>,

    server: super::Server,
    task: Option<tokio::task::JoinHandle<()>>,
}

impl OutgoingServerTransfer {
    pub fn new(server: &super::Server) -> Self {
        Self {
            bytes_sent: Arc::new(AtomicU64::new(0)),
            server: server.clone(),
            task: None,
        }
    }

    fn log(server: &super::Server, message: &str) {
        let timestamp = Local::now().format("%Y-%m-%d %H:%M:%S");
        let formatted_message = format!(
            "{} {}",
            format!("{} [Transfer System] [Source Node]:", timestamp)
                .yellow()
                .bold(),
            message
        );

        server
            .websocket
            .send(super::websocket::WebsocketMessage::new(
                super::websocket::WebsocketEvent::ServerTransferLogs,
                &[formatted_message],
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
        let bytes_sent = Arc::clone(&self.bytes_sent);
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

            let archive_path = Path::new(&server.config.system.archive_directory)
                .join(format!("{}.tar.gz", server.uuid));

            let written_bytes = Arc::new(AtomicU64::new(0));
            let archive_task = tokio::task::spawn_blocking({
                let server = Arc::clone(&server);
                let written_bytes = Arc::clone(&written_bytes);
                let archive_path = archive_path.clone();

                move || {
                    let writer = std::fs::File::create(archive_path).unwrap();
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
                            written_bytes
                                .fetch_add(file.metadata().unwrap().len(), Ordering::Relaxed);

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

            let progress_task = tokio::task::spawn({
                let server = server.clone();
                let wriiten_bytes = Arc::clone(&written_bytes);

                async move {
                    let mut last_bytes_written = 0;

                    loop {
                        let bytes_written_value = wriiten_bytes.load(Ordering::SeqCst);
                        let diff_sent = bytes_written_value - last_bytes_written;
                        last_bytes_written = bytes_written_value;

                        let formatted_size = format_bytes(bytes_written_value);
                        let formatted_total = format_bytes(server.filesystem.cached_usage());
                        let formatted_diff = format_bytes(diff_sent);

                        Self::log(
                            &server,
                            &format!(
                                "Wrote {}/{} ({}/s)",
                                formatted_size, formatted_total, formatted_diff
                            ),
                        );
                        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                    }
                }
            });

            if let Err(err) = archive_task.await {
                crate::logger::log(
                    crate::logger::LoggerLevel::Error,
                    format!("Failed to create transfer archive: {}", err),
                );

                tokio::fs::remove_file(archive_path).await.ok();
                Self::transfer_failure(&server).await;
                progress_task.abort();
                return;
            }

            progress_task.abort();

            let mut hasher = sha2::Sha256::new();
            let mut file = tokio::fs::File::open(&archive_path).await.unwrap();

            let mut buffer = [0; 8192];
            loop {
                let bytes_read = file.read(&mut buffer).await.unwrap();
                if bytes_read == 0 {
                    break;
                }

                hasher.update(&buffer[..bytes_read]);
            }

            let checksum = format!("{:x}", hasher.finalize());
            let formatted_file_size = format_bytes(file.metadata().await.unwrap().len());

            file.seek(std::io::SeekFrom::Start(0)).await.unwrap();

            let progress_task = tokio::task::spawn({
                let server = server.clone();

                async move {
                    let mut last_bytes_sent = 0;

                    loop {
                        let bytes_sent = bytes_sent.load(Ordering::SeqCst);
                        let diff_sent = bytes_sent - last_bytes_sent;
                        last_bytes_sent = bytes_sent;

                        let formatted_size = format_bytes(bytes_sent);
                        let formatted_diff = format_bytes(diff_sent);

                        Self::log(
                            &server,
                            &format!(
                                "Transferred {}/{} ({}/s)",
                                formatted_size, formatted_file_size, formatted_diff
                            ),
                        );
                        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                    }
                }
            });

            let form = reqwest::multipart::Form::new()
                .part(
                    "archive",
                    reqwest::multipart::Part::stream(file)
                        .file_name("archive.tar.gz")
                        .mime_str("application/gzip")
                        .unwrap(),
                )
                .part(
                    "checksum",
                    reqwest::multipart::Part::text(checksum)
                        .file_name("checksum")
                        .mime_str("text/plain")
                        .unwrap(),
                );

            Self::log(&server, "Streaming archive to destination...");

            let client = reqwest::Client::new();
            if let Err(err) = client
                .post(url)
                .timeout(std::time::Duration::MAX)
                .header("Authorization", token)
                .multipart(form)
                .send()
                .await
            {
                crate::logger::log(
                    crate::logger::LoggerLevel::Error,
                    format!("Failed to send transfer to destination: {}", err),
                );

                progress_task.abort();
                Self::transfer_failure(&server).await;
                tokio::fs::remove_file(archive_path).await.ok();
                return;
            }

            progress_task.abort();
            tokio::fs::remove_file(archive_path).await.ok();

            Self::log(&server, "Finished streaming archive to destination.");

            server.transferring.store(false, Ordering::SeqCst);
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

fn format_bytes(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;

    if bytes >= GB {
        format!("{:.2} GiB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.2} MiB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.2} KiB", bytes as f64 / KB as f64)
    } else {
        format!("{} bytes", bytes)
    }
}

impl Drop for OutgoingServerTransfer {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}
