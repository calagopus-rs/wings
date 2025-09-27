use anyhow::Context;
use rand::Rng;
use std::{
    path::{Path, PathBuf},
    str::FromStr,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};
use tokio::{io::AsyncWriteExt, sync::RwLock};

mod resolver;

static DOWNLOAD_CLIENT: RwLock<Option<Arc<reqwest::Client>>> = RwLock::const_new(None);

async fn get_download_client(
    config: &Arc<crate::config::Config>,
) -> Result<Arc<reqwest::Client>, anyhow::Error> {
    let client = DOWNLOAD_CLIENT.read().await;
    if let Some(client) = client.as_ref() {
        return Ok(Arc::clone(client));
    }

    drop(client);
    let mut write_lock = DOWNLOAD_CLIENT.write().await;

    let new_client = reqwest::Client::builder()
        .user_agent("Pterodactyl Panel (https://pterodactyl.io)")
        .connect_timeout(std::time::Duration::from_secs(30))
        .dns_resolver(Arc::new(resolver::DnsResolver::new(config)))
        .build()
        .context("failed to build download client")?;

    let new_client = Arc::new(new_client);
    *write_lock = Some(Arc::clone(&new_client));

    Ok(new_client)
}

pub struct Download {
    pub identifier: uuid::Uuid,
    pub progress: Arc<AtomicU64>,
    pub total: u64,
    pub destination: PathBuf,
    pub server: crate::server::Server,
    pub response: Option<reqwest::Response>,
}

impl Download {
    pub async fn new(
        server: crate::server::Server,
        destination: &Path,
        file_name: Option<String>,
        url: String,
        use_header: bool,
    ) -> Result<Self, anyhow::Error> {
        let url = reqwest::Url::parse(&url).context("failed to parse download URL")?;

        if let Some(host) = url.host_str()
            && let Ok(ip) = std::net::IpAddr::from_str(host)
        {
            for cidr in server
                .app_state
                .config
                .api
                .remote_download_blocked_cidrs
                .iter()
            {
                if cidr.contains(&ip) {
                    tracing::warn!("blocking internal IP address in pull: {}", ip);
                    return Err(anyhow::anyhow!("IP address {} is blocked", ip));
                }
            }
        }

        let response = get_download_client(&server.app_state.config)
            .await?
            .get(url)
            .send()
            .await
            .context("failed to send download request")?;
        let mut real_destination = destination.to_path_buf();

        if !response.status().is_success() {
            return Err(anyhow::anyhow!(
                "failed to download file: code {}",
                response.status()
            ));
        }

        'header_check: {
            if use_header {
                if let Some(header) = response.headers().get("Content-Disposition")
                    && let Ok(header) = header.to_str()
                    && let Some(filename) =
                        content_disposition::parse_content_disposition(header).filename_full()
                {
                    real_destination.push(filename);
                    break 'header_check;
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

                            format!("download_{random_string}")
                        }),
                );
            } else if let Some(file_name) = file_name {
                real_destination.push(file_name);
            } else {
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

                            format!("download_{random_string}")
                        }),
                );
            }
        }

        if server.filesystem.is_ignored(&real_destination, false).await {
            return Err(anyhow::anyhow!("file is ignored"));
        }

        Ok(Self {
            identifier: uuid::Uuid::new_v4(),
            progress: Arc::new(AtomicU64::new(0)),
            total: response.content_length().unwrap_or_else(|| {
                response
                    .headers()
                    .get("Content-Length")
                    .and_then(|h| h.to_str().ok())
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0)
            }),
            destination: real_destination,
            server,
            response: Some(response),
        })
    }

    pub async fn start(
        &mut self,
    ) -> (
        uuid::Uuid,
        tokio::task::JoinHandle<Option<Result<(), anyhow::Error>>>,
    ) {
        let progress = Arc::clone(&self.progress);
        let destination = self.destination.clone();
        let server = self.server.clone();
        let mut response = self.response.take().unwrap();

        let (identifier, task) = self
            .server
            .filesystem
            .operations
            .add_operation(
                super::operations::FilesystemOperation::Pull {
                    path: self.destination.clone(),
                    progress: self.progress.clone(),
                    total: Arc::new(AtomicU64::new(self.total)),
                },
                async move {
                    let mut run_inner = async || -> Result<(), anyhow::Error> {
                        let mut writer = super::writer::AsyncFileSystemWriter::new(
                            server.clone(),
                            &destination,
                            None,
                            None,
                        )
                        .await?;

                        while let Some(chunk) = response.chunk().await? {
                            writer.write_all(&chunk).await?;
                            progress.fetch_add(chunk.len() as u64, Ordering::Relaxed);
                        }

                        writer.flush().await?;
                        Ok(())
                    };

                    match run_inner().await {
                        Ok(_) => {
                            tracing::info!(
                                server = %server.uuid,
                                "pull completed: {}",
                                destination.to_string_lossy()
                            );

                            Ok(())
                        }
                        Err(err) => {
                            tracing::error!(
                                server = %server.uuid,
                                "failed to pull file: {:#?}",
                                err
                            );

                            Err(err)
                        }
                    }
                },
            )
            .await;

        self.identifier = identifier;

        (identifier, task)
    }

    #[inline]
    pub fn to_api_response(&self) -> crate::models::Download {
        crate::models::Download {
            identifier: self.identifier,
            destination: self.destination.to_string_lossy().to_string(),
            progress: self.progress.load(Ordering::Relaxed),
            total: self.total,
        }
    }
}
