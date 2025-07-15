use serde::Serialize;
use std::{collections::VecDeque, net::IpAddr, sync::Arc};
use tokio::sync::Mutex;

#[derive(Debug, Serialize)]
pub enum ActivityEvent {
    #[serde(rename = "server:power.start")]
    PowerStart,
    #[serde(rename = "server:power.stop")]
    PowerStop,
    #[serde(rename = "server:power.restart")]
    PowerRestart,
    #[serde(rename = "server:power.kill")]
    PowerKill,

    #[serde(rename = "server:console.command")]
    ConsoleCommand,

    #[serde(rename = "server:sftp.write")]
    SftpWrite,
    #[serde(rename = "server:sftp.create")]
    SftpCreate,
    #[serde(rename = "server:sftp.create-directory")]
    SftpCreateDirectory,
    #[serde(rename = "server:sftp.rename")]
    SftpRename,
    #[serde(rename = "server:sftp.delete")]
    SftpDelete,

    #[serde(rename = "server:file.uploaded")]
    FileUploaded,
    #[serde(rename = "server:file.compress")]
    FileCompress,
    #[serde(rename = "server:file.decompress")]
    FileDecompress,
}

#[derive(Debug, Serialize)]
pub struct ApiActivity {
    user: Option<uuid::Uuid>,
    server: uuid::Uuid,
    event: ActivityEvent,
    metadata: Option<serde_json::Value>,

    ip: Option<String>,
    timestamp: chrono::DateTime<chrono::Utc>,
}

pub struct Activity {
    pub user: Option<uuid::Uuid>,
    pub event: ActivityEvent,
    pub metadata: Option<serde_json::Value>,

    pub ip: Option<IpAddr>,
    pub timestamp: chrono::DateTime<chrono::Utc>,
}

pub struct ActivityManager {
    activities: Arc<Mutex<VecDeque<Activity>>>,
    schedule_handle: tokio::task::JoinHandle<()>,
}

impl ActivityManager {
    pub fn new(server: uuid::Uuid, config: &Arc<crate::config::Config>) -> Self {
        let activities = Arc::new(Mutex::new(VecDeque::new()));

        Self {
            activities: Arc::clone(&activities),
            schedule_handle: tokio::spawn({
                let config = Arc::clone(config);

                async move {
                    loop {
                        tokio::time::sleep(tokio::time::Duration::from_secs(
                            config.system.activity_send_interval,
                        ))
                        .await;

                        let mut activities = activities.lock().await;
                        let activities_len = activities.len();
                        let activities = activities
                            .drain(..config.system.activity_send_count.min(activities_len))
                            .collect::<Vec<_>>();

                        if activities.is_empty() {
                            continue;
                        }

                        let len = activities.len();

                        if let Err(err) = config
                            .client
                            .send_activity(
                                activities
                                    .into_iter()
                                    .map(|activity| ApiActivity {
                                        user: activity.user,
                                        server,
                                        event: activity.event,
                                        metadata: activity.metadata,
                                        ip: activity.ip.map(|ip| ip.to_string()),
                                        timestamp: activity.timestamp,
                                    })
                                    .collect(),
                            )
                            .await
                        {
                            tracing::error!(
                                server = %server,
                                "failed to send {} activities to remote: {}",
                                len,
                                err
                            );
                        }
                    }
                }
            }),
        }
    }

    pub async fn log_activity(&self, activity: Activity) {
        self.activities.lock().await.push_back(activity);
    }
}

impl Drop for ActivityManager {
    fn drop(&mut self) {
        self.schedule_handle.abort();
    }
}
