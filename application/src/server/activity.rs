use serde::Serialize;
use std::{
    collections::{HashMap, VecDeque},
    net::IpAddr,
    sync::Arc,
};
use tokio::sync::Mutex;

#[derive(Debug, Serialize, Clone, Copy, PartialEq, Eq, Hash)]
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
    #[serde(rename = "server:file.create-directory")]
    FileCreateDirectory,
    #[serde(rename = "server:file.write")]
    FileWrite,
    #[serde(rename = "server:file.copy")]
    FileCopy,
    #[serde(rename = "server:file.delete")]
    FileDelete,
    #[serde(rename = "server:file.rename")]
    FileRename,
    #[serde(rename = "server:file.pull")]
    FilePull,
}

impl ActivityEvent {
    #[inline]
    pub fn is_sftp_event(self) -> bool {
        matches!(
            self,
            ActivityEvent::SftpWrite
                | ActivityEvent::SftpCreate
                | ActivityEvent::SftpCreateDirectory
                | ActivityEvent::SftpRename
                | ActivityEvent::SftpDelete
        )
    }
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

#[derive(Debug, Clone)]
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

                        if activities_len == 0 {
                            continue;
                        }

                        let mut merged_activities = VecDeque::new();
                        let mut sftp_events: HashMap<
                            (ActivityEvent, Option<uuid::Uuid>),
                            (Activity, Vec<usize>),
                        > = HashMap::new();
                        let mut file_upload_events: HashMap<
                            (Option<uuid::Uuid>, String),
                            (Activity, Vec<usize>),
                        > = HashMap::new();

                        for (idx, activity) in activities.iter().enumerate() {
                            if activity.event.is_sftp_event()
                                && let Some(metadata) = &activity.metadata
                                && metadata.get("files").is_some()
                            {
                                let key = (activity.event, activity.user);
                                let mut found_match = false;

                                for ((event_type, user), (existing, indices)) in &mut sftp_events {
                                    if *event_type == activity.event && *user == activity.user {
                                        let duration = activity
                                            .timestamp
                                            .signed_duration_since(existing.timestamp);
                                        if duration.num_seconds().abs() <= 60 {
                                            indices.push(idx);
                                            found_match = true;
                                            break;
                                        }
                                    }
                                }

                                if !found_match {
                                    sftp_events.insert(key, (activity.clone(), vec![idx]));
                                }

                                continue;
                            }

                            if activity.event == ActivityEvent::FileUploaded
                                && let Some(metadata) = &activity.metadata
                                && metadata.get("files").is_some()
                                && let Some(directory) = metadata.get("directory")
                                && let Some(dir_str) = directory.as_str()
                            {
                                let key = (activity.user, dir_str.to_string());
                                let mut found_match = false;

                                for ((user, dir), (existing, indices)) in &mut file_upload_events {
                                    if *user == activity.user && dir == dir_str {
                                        let duration = activity
                                            .timestamp
                                            .signed_duration_since(existing.timestamp);
                                        if duration.num_seconds().abs() <= 60 {
                                            indices.push(idx);
                                            found_match = true;
                                            break;
                                        }
                                    }
                                }

                                if !found_match {
                                    file_upload_events.insert(key, (activity.clone(), vec![idx]));
                                }

                                continue;
                            }

                            merged_activities.push_back(activity.clone());
                        }

                        for (mut base_activity, indices) in sftp_events.into_values() {
                            if indices.len() > 1 {
                                let mut all_files = Vec::new();

                                for idx in indices {
                                    if let Some(activity) = activities.get(idx)
                                        && let Some(metadata) = &activity.metadata
                                        && let Some(files) = metadata.get("files")
                                        && let Some(files_array) = files.as_array()
                                    {
                                        for file in files_array {
                                            all_files.push(file.clone());
                                        }
                                    }
                                }

                                if let Some(metadata) = &mut base_activity.metadata
                                    && let Some(files) = metadata.get_mut("files")
                                {
                                    *files = serde_json::Value::Array(all_files);
                                }
                            }

                            merged_activities.push_back(base_activity);
                        }

                        for (mut base_activity, indices) in file_upload_events.into_values() {
                            if indices.len() > 1 {
                                let mut all_files = Vec::new();

                                for idx in indices {
                                    if let Some(activity) = activities.get(idx)
                                        && let Some(metadata) = &activity.metadata
                                        && let Some(files) = metadata.get("files")
                                        && let Some(files_array) = files.as_array()
                                    {
                                        for file in files_array {
                                            all_files.push(file.clone());
                                        }
                                    }
                                }

                                if let Some(metadata) = &mut base_activity.metadata
                                    && let Some(files) = metadata.get_mut("files")
                                {
                                    *files = serde_json::Value::Array(all_files);
                                }
                            }

                            merged_activities.push_back(base_activity);
                        }

                        *activities = merged_activities;

                        let activities_len = activities.len();
                        let activities_to_send = activities
                            .drain(..config.system.activity_send_count.min(activities_len))
                            .collect::<Vec<_>>();

                        if activities_to_send.is_empty() {
                            continue;
                        }

                        let len = activities_to_send.len();

                        if let Err(err) = config
                            .client
                            .send_activity(
                                activities_to_send
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
                                "failed to send {} activities to remote: {:#?}",
                                len,
                                err
                            );
                        }
                    }
                }
            }),
        }
    }

    #[inline]
    pub async fn log_activity(&self, activity: Activity) {
        self.activities.lock().await.push_back(activity);
    }
}

impl Drop for ActivityManager {
    fn drop(&mut self) {
        self.schedule_handle.abort();
    }
}
