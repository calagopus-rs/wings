use crate::{remote::backups::RawServerBackup, server::backup::adapters::BackupAdapter};
use ignore::gitignore::GitignoreBuilder;
use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};
use tokio::sync::RwLock;

type BackupValue = (Arc<super::Backup>, std::time::Instant);
type BrowseBackupValue = (Arc<super::BrowseBackup>, std::time::Instant);
pub struct BackupManager {
    config: Arc<crate::config::Config>,
    cached_backups: Arc<RwLock<HashMap<uuid::Uuid, BackupValue>>>,
    cached_browse_backups: Arc<RwLock<HashMap<uuid::Uuid, BrowseBackupValue>>>,
    cached_browse_backup_locks: Arc<RwLock<HashMap<uuid::Uuid, Arc<tokio::sync::Mutex<()>>>>>,
    cached_backup_adapters: RwLock<HashMap<uuid::Uuid, BackupAdapter>>,

    task: tokio::task::JoinHandle<()>,
}

impl BackupManager {
    pub fn new(config: Arc<crate::config::Config>) -> Self {
        let cached_backups = Arc::new(RwLock::new(HashMap::new()));
        let cached_browse_backups = Arc::new(RwLock::new(HashMap::new()));
        let cached_browse_backup_locks = Arc::new(RwLock::new(HashMap::new()));

        Self {
            config,
            cached_backups: Arc::clone(&cached_backups),
            cached_browse_backups: Arc::clone(&cached_browse_backups),
            cached_backup_adapters: RwLock::new(HashMap::new()),
            cached_browse_backup_locks: Arc::clone(&cached_browse_backup_locks),
            task: tokio::spawn({
                async move {
                    loop {
                        tokio::time::sleep(std::time::Duration::from_secs(60)).await;

                        let mut cached_backups = cached_backups.write().await;
                        cached_backups.retain(|_, (_, last_accessed)| {
                            last_accessed.elapsed() < std::time::Duration::from_secs(300)
                        });
                        drop(cached_backups);

                        let mut cached_browse_backups = cached_browse_backups.write().await;
                        cached_browse_backups.retain(|_, (_, last_accessed)| {
                            last_accessed.elapsed() < std::time::Duration::from_secs(300)
                        });

                        let mut cached_browse_backup_locks =
                            cached_browse_backup_locks.write().await;
                        cached_browse_backup_locks
                            .retain(|uuid, _| cached_browse_backups.contains_key(uuid));
                        drop(cached_browse_backups);
                        drop(cached_browse_backup_locks);
                    }
                }
            }),
        }
    }

    pub async fn fast_contains(&self, server: &crate::server::Server, uuid: uuid::Uuid) -> bool {
        self.cached_backups.read().await.contains_key(&uuid)
            || server.configuration.read().await.backups.contains(&uuid)
    }

    pub async fn adapter_contains(&self, uuid: uuid::Uuid) -> bool {
        if let Some(adapter) = self.cached_backup_adapters.read().await.get(&uuid) {
            match adapter.exists(&self.config, uuid).await {
                Ok(exists) => exists,
                Err(err) => {
                    tracing::error!(adapter = ?adapter, "failed to check if backup {} exists: {:#?}", uuid, err);
                    false
                }
            }
        } else {
            match BackupAdapter::exists_any(&self.config, uuid).await {
                Ok(exists) => exists,
                Err(err) => {
                    tracing::error!("failed to check if backup {} exists: {:#?}", uuid, err);
                    false
                }
            }
        }
    }

    pub async fn create(
        &self,
        adapter: BackupAdapter,
        server: &crate::server::Server,
        uuid: uuid::Uuid,
        ignore: String,
    ) -> Result<RawServerBackup, anyhow::Error> {
        tracing::info!(
            server = %server.uuid,
            backup = %uuid,
            adapter = ?adapter,
            "creating backup",
        );

        let mut ignore_builder = GitignoreBuilder::new("");
        let mut ignore_raw = String::new();

        for line in ignore.lines() {
            if ignore_builder.add_line(None, line).is_ok() {
                ignore_raw.push_str(line);
                ignore_raw.push('\n');
            }
        }

        if let Ok(pteroignore) = server.filesystem.async_read_to_string(".pteroignore").await {
            for line in pteroignore.lines() {
                if ignore_builder.add_line(None, line).is_ok() {
                    ignore_raw.push_str(line);
                    ignore_raw.push('\n');
                }
            }
        }

        for line in server.configuration.read().await.egg.file_denylist.iter() {
            if ignore_builder.add_line(None, line).is_ok() {
                ignore_raw.push_str(line);
                ignore_raw.push('\n');
            }
        }

        ignore_raw.shrink_to_fit();

        let progress = Arc::new(AtomicU64::new(0));
        let total = Arc::new(AtomicU64::new(0));

        let progress_task = tokio::spawn({
            let progress = Arc::clone(&progress);
            let total = Arc::clone(&total);
            let server = server.clone();

            async move {
                loop {
                    let progress = progress.load(Ordering::SeqCst);
                    let total = total.load(Ordering::SeqCst);

                    server
                        .websocket
                        .send(crate::server::websocket::WebsocketMessage::new(
                            crate::server::websocket::WebsocketEvent::ServerBackupProgress,
                            &[
                                uuid.to_string(),
                                serde_json::to_string(&crate::models::Progress { progress, total })
                                    .unwrap(),
                            ],
                        ))
                        .ok();

                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                }
            }
        });

        let backup = match adapter
            .create(
                server,
                uuid,
                Arc::clone(&progress),
                Arc::clone(&total),
                ignore_builder.build()?,
                ignore_raw,
            )
            .await
        {
            Ok(backup) => {
                progress_task.abort();

                backup
            }
            Err(e) => {
                progress_task.abort();

                server
                    .app_state
                    .config
                    .client
                    .set_backup_status(
                        uuid,
                        &RawServerBackup {
                            checksum: String::new(),
                            checksum_type: String::new(),
                            size: 0,
                            files: 0,
                            successful: false,
                            parts: vec![],
                        },
                    )
                    .await?;
                server
                    .websocket
                    .send(crate::server::websocket::WebsocketMessage::new(
                        crate::server::websocket::WebsocketEvent::ServerBackupCompleted,
                        &[
                            uuid.to_string(),
                            serde_json::json!({
                                "checksum_type": "",
                                "checksum": "",
                                "size": 0,
                                "files": 0,
                                "successful": false,
                            })
                            .to_string(),
                        ],
                    ))?;
                self.cached_backup_adapters
                    .write()
                    .await
                    .insert(uuid, adapter);

                if let Err(err) = adapter.clean(server, uuid).await {
                    tracing::error!(server = %server.uuid, adapter = ?adapter, "failed to clean up backup {} after error: {:#?}", uuid, err);
                }

                return Err(e);
            }
        };

        server
            .app_state
            .config
            .client
            .set_backup_status(uuid, &backup)
            .await?;
        server
            .websocket
            .send(crate::server::websocket::WebsocketMessage::new(
                crate::server::websocket::WebsocketEvent::ServerBackupCompleted,
                &[
                    uuid.to_string(),
                    serde_json::json!({
                        "checksum_type": backup.checksum_type,
                        "checksum": backup.checksum,
                        "size": backup.size,
                        "files": backup.files,
                        "successful": backup.successful,
                    })
                    .to_string(),
                ],
            ))?;
        server.configuration.write().await.backups.push(uuid);
        self.cached_backup_adapters
            .write()
            .await
            .insert(uuid, adapter);

        tracing::info!(
            server = %server.uuid,
            adapter = ?adapter,
            "completed backup {}",
            uuid,
        );

        Ok(backup)
    }

    pub async fn restore(
        &self,
        backup: &super::Backup,
        server: &crate::server::Server,
        truncate_directory: bool,
        download_url: Option<String>,
    ) -> Result<(), anyhow::Error> {
        if server.is_locked_state() {
            return Err(anyhow::anyhow!("Server is in a locked state"));
        }

        server.restoring.store(true, Ordering::SeqCst);
        if let Err(err) = server
            .stop_with_kill_timeout(std::time::Duration::from_secs(30), false)
            .await
        {
            tracing::error!(
                server = %server.uuid,
                "failed to stop server before restoring backup: {:#?}",
                err
            );

            server.restoring.store(false, Ordering::SeqCst);
            server
                .app_state
                .config
                .client
                .set_backup_restore_status(backup.uuid(), false)
                .await?;

            return Err(err);
        }

        tracing::info!(
            server = %server.uuid,
            backup = %backup.uuid(),
            adapter = ?backup.adapter(),
            "restoring backup",
        );

        if truncate_directory && let Err(err) = server.filesystem.truncate_root().await {
            tracing::error!(
                server = %server.uuid,
                backup = %backup.uuid(),
                "failed to truncate root directory before restoring backup: {:#?}",
                err
            );

            server.restoring.store(false, Ordering::SeqCst);
            server
                .app_state
                .config
                .client
                .set_backup_restore_status(backup.uuid(), false)
                .await?;

            return Err(err);
        }

        let progress = Arc::new(AtomicU64::new(0));
        let total = Arc::new(AtomicU64::new(1));

        let progress_task = tokio::spawn({
            let progress = Arc::clone(&progress);
            let total = Arc::clone(&total);
            let server = server.clone();

            async move {
                loop {
                    let progress_value = progress.load(Ordering::SeqCst);
                    let total_value = total.load(Ordering::SeqCst);

                    server
                        .websocket
                        .send(crate::server::websocket::WebsocketMessage::new(
                            crate::server::websocket::WebsocketEvent::ServerBackupRestoreProgress,
                            &[serde_json::to_string(&crate::models::Progress {
                                progress: progress_value,
                                total: total_value,
                            })
                            .unwrap()],
                        ))
                        .ok();

                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                }
            }
        });

        match backup
            .restore(
                server,
                Arc::clone(&progress),
                Arc::clone(&total),
                download_url,
            )
            .await
        {
            Ok(_) => {
                progress_task.abort();

                server.restoring.store(false, Ordering::SeqCst);
                server
                    .log_daemon(format!(
                        "Completed server restoration from {} backup.",
                        serde_json::to_value(backup.adapter())
                            .unwrap()
                            .as_str()
                            .unwrap()
                    ))
                    .await;
                server
                    .app_state
                    .config
                    .client
                    .set_backup_restore_status(backup.uuid(), true)
                    .await?;
                server
                    .websocket
                    .send(crate::server::websocket::WebsocketMessage::new(
                        crate::server::websocket::WebsocketEvent::ServerBackupRestoreCompleted,
                        &[],
                    ))?;

                tracing::info!(
                    server = %server.uuid,
                    backup = %backup.uuid(),
                    adapter = ?backup.adapter(),
                    "completed restore of backup",
                );

                Ok(())
            }
            Err(err) => {
                progress_task.abort();

                server.restoring.store(false, Ordering::SeqCst);
                server
                    .app_state
                    .config
                    .client
                    .set_backup_restore_status(backup.uuid(), false)
                    .await?;

                Err(err)
            }
        }
    }

    pub async fn find(
        &self,
        uuid: uuid::Uuid,
    ) -> Result<Option<Arc<super::Backup>>, anyhow::Error> {
        if let Some(backup) = self.cached_backups.write().await.get_mut(&uuid) {
            backup.1 = std::time::Instant::now();

            return Ok(Some(Arc::clone(&backup.0)));
        }

        if let Some(adapter) = self.cached_backup_adapters.read().await.get(&uuid)
            && let Some(backup) = adapter.find(&self.config, uuid).await?
        {
            let backup = Arc::new(backup);

            let mut cache = self.cached_backups.write().await;
            cache.insert(uuid, (Arc::clone(&backup), std::time::Instant::now()));
            drop(cache);

            return Ok(Some(backup));
        }

        if let Some((adapter, backup)) = BackupAdapter::find_all(&self.config, uuid).await? {
            let backup = Arc::new(backup);

            let mut cache = self.cached_backups.write().await;
            cache.insert(uuid, (Arc::clone(&backup), std::time::Instant::now()));
            drop(cache);
            let mut adapter_cache = self.cached_backup_adapters.write().await;
            adapter_cache.insert(uuid, adapter);
            drop(adapter_cache);

            return Ok(Some(backup));
        }

        Ok(None)
    }

    pub async fn find_adapter(
        &self,
        adapter: BackupAdapter,
        uuid: uuid::Uuid,
    ) -> Result<Option<Arc<super::Backup>>, anyhow::Error> {
        if let Some(backup) = self.cached_backups.write().await.get_mut(&uuid) {
            backup.1 = std::time::Instant::now();

            return Ok(Some(Arc::clone(&backup.0)));
        }

        if let Some(backup) = adapter.find(&self.config, uuid).await? {
            let backup = Arc::new(backup);

            let mut cache = self.cached_backups.write().await;
            cache.insert(uuid, (Arc::clone(&backup), std::time::Instant::now()));
            drop(cache);

            return Ok(Some(backup));
        }

        Ok(None)
    }

    pub async fn browse(
        &self,
        server: &crate::server::Server,
        uuid: uuid::Uuid,
    ) -> Result<Option<Arc<super::BrowseBackup>>, anyhow::Error> {
        if let Some(backup) = self.cached_browse_backups.write().await.get_mut(&uuid) {
            backup.1 = std::time::Instant::now();

            return Ok(Some(Arc::clone(&backup.0)));
        }

        if let Some(backup) = self.find(uuid).await? {
            let server = server.clone();
            let cached_browse_backup_locks = Arc::clone(&self.cached_browse_backup_locks);
            let cached_browse_backups = Arc::clone(&self.cached_browse_backups);

            return tokio::spawn(async move {
                let read_cached_browse_backup_locks = cached_browse_backup_locks.read().await;
                let _guard = if let Some(lock) = read_cached_browse_backup_locks.get(&uuid) {
                    Arc::clone(lock)
                } else {
                    drop(read_cached_browse_backup_locks);

                    let lock = Arc::new(tokio::sync::Mutex::new(()));
                    cached_browse_backup_locks
                        .write()
                        .await
                        .insert(uuid, Arc::clone(&lock));

                    Arc::clone(&lock)
                };
                let _guard = _guard.lock().await;

                if let Some(browse_backup) = cached_browse_backups.read().await.get(&uuid) {
                    return Ok(Some(Arc::clone(&browse_backup.0)));
                }

                let browse_backup = Arc::new(backup.browse(&server).await?);

                let mut cache = cached_browse_backups.write().await;
                cache.insert(
                    uuid,
                    (Arc::clone(&browse_backup), std::time::Instant::now()),
                );
                drop(cache);

                Ok(Some(browse_backup))
            })
            .await?;
        }

        Ok(None)
    }
}

impl Drop for BackupManager {
    fn drop(&mut self) {
        self.task.abort();
    }
}
