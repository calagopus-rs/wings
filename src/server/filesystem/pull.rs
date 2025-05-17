use rand::Rng;
use std::{
    path::{Path, PathBuf},
    sync::{Arc, LazyLock, atomic::AtomicU64},
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
    pub filesystem: Arc<crate::server::filesystem::Filesystem>,
    pub response: Option<reqwest::Response>,

    pub task: Option<tokio::task::JoinHandle<()>>,
}

impl Download {
    pub async fn new(
        destination: &Path,
        file_name: Option<String>,
        url: String,
        use_header: bool,
        filesystem: Arc<crate::server::filesystem::Filesystem>,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let response = DOWNLOAD_CLIENT.get(&url).send().await?;
        let mut real_destination = destination.to_path_buf();

        if !response.status().is_success() {
            return Err(format!("Failed to download file: code {}", response.status()).into());
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

        if !filesystem.is_safe_path(&real_destination) {
            return Err("Unsafe path generated".into());
        }

        if filesystem.is_ignored(&real_destination, false) {
            return Err("File is ignored".into());
        }

        Ok(Self {
            identifier: uuid::Uuid::new_v4(),
            progress: Arc::new(AtomicU64::new(0)),
            total: response.content_length().unwrap_or(0),
            destination: real_destination,
            filesystem,
            response: Some(response),
            task: None,
        })
    }

    pub fn start(&mut self) {
        let progress = Arc::clone(&self.progress);
        let destination = self.destination.clone();
        let filesystem = Arc::clone(&self.filesystem);
        let mut response = self.response.take().unwrap();

        let task = tokio::task::spawn(async move {
            let mut writer = super::writer::AsyncFileSystemWriter::new(
                Arc::clone(&filesystem),
                destination,
                None,
            )
            .await
            .unwrap();

            while let Ok(Some(chunk)) = response.chunk().await {
                writer.write_all(&chunk).await.unwrap();
                progress.fetch_add(chunk.len() as u64, std::sync::atomic::Ordering::Relaxed);
            }

            writer.flush().await.unwrap();
        });

        self.task = Some(task);
    }

    pub fn to_api_response(&self) -> crate::models::Download {
        let progress = self.progress.load(std::sync::atomic::Ordering::Relaxed);

        crate::models::Download {
            identifier: self.identifier,
            progress,
            total: self.total,
        }
    }
}
