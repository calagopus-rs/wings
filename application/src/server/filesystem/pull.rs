use anyhow::Context;
use rand::Rng;
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc, LazyLock,
        atomic::{AtomicU64, Ordering},
    },
};
use tokio::io::AsyncWriteExt;

static DOWNLOAD_CLIENT: LazyLock<reqwest::Client> = LazyLock::new(|| {
    reqwest::Client::builder()
        .user_agent("Pterodactyl Panel (https://pterodactyl.io)")
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .unwrap()
});

pub struct Download {
    pub identifier: uuid::Uuid,
    pub progress: Arc<AtomicU64>,
    pub total: u64,
    pub destination: PathBuf,
    pub server: crate::server::Server,
    pub response: Option<reqwest::Response>,

    pub task: Option<tokio::task::JoinHandle<()>>,
}

impl Download {
    pub async fn new(
        server: crate::server::Server,
        destination: &Path,
        file_name: Option<String>,
        url: String,
        use_header: bool,
    ) -> Result<Self, anyhow::Error> {
        let response = DOWNLOAD_CLIENT
            .get(&url)
            .send()
            .await
            .context("failed to send download request")?;
        let mut real_destination = destination.to_path_buf();

        if !response.status().is_success() {
            return Err(anyhow::anyhow!(
                "Failed to download file: code {}",
                response.status()
            ));
        }

        'header_check: {
            if use_header {
                if let Some(header) = response.headers().get("Content-Disposition") {
                    if let Ok(header) = header.to_str() {
                        if let Some(filename) = header.split("filename=").nth(1) {
                            real_destination.push(filename.trim_matches('"'));
                            break 'header_check;
                        }
                    }
                }

                real_destination.push(
                    response
                        .url()
                        .path_segments()
                        .and_then(|mut segments| segments.next_back())
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| {
                            let random_string: String = rand::rng()
                                .sample_iter(&rand::distr::Alphanumeric)
                                .take(8)
                                .map(char::from)
                                .collect();

                            format!("download_{}", random_string)
                        }),
                );
            } else if let Some(file_name) = file_name {
                real_destination.push(file_name);
            }
        }

        if !server.filesystem.is_safe_path(&real_destination).await {
            return Err(anyhow::anyhow!("unsafe path for pull"));
        }

        if server.filesystem.is_ignored(&real_destination, false).await {
            return Err(anyhow::anyhow!("file is ignored"));
        }

        Ok(Self {
            identifier: uuid::Uuid::new_v4(),
            progress: Arc::new(AtomicU64::new(0)),
            total: response.content_length().unwrap_or(0),
            destination: real_destination,
            server,
            response: Some(response),
            task: None,
        })
    }

    pub fn start(&mut self) {
        let progress = Arc::clone(&self.progress);
        let destination = self.destination.clone();
        let server = self.server.clone();
        let mut response = self.response.take().unwrap();

        let task = tokio::task::spawn(async move {
            let mut writer = super::writer::AsyncFileSystemWriter::new(server, destination, None)
                .await
                .unwrap();

            while let Ok(Some(chunk)) = response.chunk().await {
                writer.write_all(&chunk).await.unwrap();
                progress.fetch_add(chunk.len() as u64, Ordering::Relaxed);
            }

            writer.flush().await.unwrap();
        });

        self.task = Some(task);
    }

    pub fn to_api_response(&self) -> crate::models::Download {
        let progress = self.progress.load(Ordering::Relaxed);

        crate::models::Download {
            identifier: self.identifier,
            progress,
            total: self.total,
        }
    }
}
