use colored::Colorize;
use sha2::Digest;
use std::sync::{Arc, atomic::AtomicU64};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub struct OutgoingServerTransfer {
    pub bytes_sent: Arc<AtomicU64>,

    server: Arc<super::Server>,
    task: Option<tokio::task::JoinHandle<()>>,
}

impl OutgoingServerTransfer {
    pub fn new(server: &Arc<super::Server>) -> Self {
        Self {
            bytes_sent: Arc::new(AtomicU64::new(0)),
            server: Arc::clone(server),
            task: None,
        }
    }

    fn log(server: &Arc<super::Server>, message: &str) {
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

    async fn transfer_failure(server: &Arc<super::Server>) {
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
        let server = Arc::clone(&self.server);
        let bytes_sent = Arc::clone(&self.bytes_sent);

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

            let (mut checksum_writer, checksum_reader) = tokio::io::duplex(128);
            let (checksummed_writer, mut checksummed_reader) = tokio::io::duplex(65536);
            let (mut writer, reader) = tokio::io::duplex(65536);
            let archive_task = tokio::task::spawn_blocking({
                let server = Arc::clone(&server);

                move || {
                    let writer = tokio_util::io::SyncIoBridge::new(checksummed_writer);
                    let writer =
                        flate2::write::GzEncoder::new(writer, flate2::Compression::default());

                    let mut tar = tar::Builder::new(writer);
                    tar.mode(tar::HeaderMode::Complete);

                    tar.append_dir_all(".", &server.filesystem.base_path)
                }
            });

            let checksum_task = tokio::task::spawn({
                let bytes_sent = Arc::clone(&bytes_sent);

                async move {
                    let mut hasher = sha2::Sha256::new();

                    let mut buffer = [0; 8192];
                    loop {
                        let bytes_read = checksummed_reader.read(&mut buffer).await.unwrap();
                        if bytes_read == 0 {
                            break;
                        }

                        bytes_sent
                            .fetch_add(bytes_read as u64, std::sync::atomic::Ordering::SeqCst);

                        hasher.update(&buffer[..bytes_read]);
                        writer.write_all(&buffer[..bytes_read]).await.unwrap();
                    }

                    checksum_writer
                        .write_all(format!("{:x}", hasher.finalize()).as_bytes())
                        .await
                        .unwrap();
                }
            });

            let progress_task = tokio::task::spawn({
                let server = Arc::clone(&server);

                async move {
                    loop {
                        let bytes_sent = bytes_sent.load(std::sync::atomic::Ordering::SeqCst);

                        Self::log(&server, &format!("Transferred {} bytes", bytes_sent));
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
