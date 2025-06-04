use crate::remote::backups::RawServerBackup;
use axum::{
    body::Body,
    http::{HeaderMap, StatusCode},
};
use ignore::overrides::OverrideBuilder;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use utoipa::ToSchema;

mod btrfs;
pub mod ddup_bak;
mod s3;
mod wings;
mod zfs;

#[derive(ToSchema, Clone, Copy, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "kebab-case")]
#[schema(rename_all = "kebab-case")]
pub enum BackupAdapter {
    Wings,
    S3,
    DdupBak,
    Btrfs,
    Zfs,
}

impl BackupAdapter {
    pub fn variants() -> &'static [Self] {
        &[Self::Wings, Self::S3, Self::DdupBak, Self::Btrfs, Self::Zfs]
    }
}

pub struct InternalBackup {
    pub adapter: BackupAdapter,
    pub uuid: uuid::Uuid,
}

impl InternalBackup {
    pub async fn create(
        adapter: BackupAdapter,
        server: &crate::server::Server,
        uuid: uuid::Uuid,
        ignore: String,
    ) -> Result<Self, anyhow::Error> {
        tracing::info!(
            server = %server.uuid,
            backup = %uuid,
            adapter = ?adapter,
            "creating backup",
        );

        let mut override_builder = OverrideBuilder::new(&server.filesystem.base_path);
        let mut override_raw = String::new();

        for line in ignore.lines() {
            if line.trim().is_empty() {
                continue;
            }

            if let Some(line) = line.trim().strip_prefix('!') {
                override_builder.add(line).ok();
                override_raw.push_str(line);
            } else {
                override_builder.add(&format!("!{}", line.trim())).ok();
                override_raw.push('!');
                override_raw.push_str(line.trim());
            }

            override_raw.push('\n');
        }

        for file in &server.configuration.read().await.egg.file_denylist {
            if let Some(file) = file.strip_prefix('!') {
                override_builder.add(file).ok();
                override_raw.push_str(file);
            } else {
                override_builder.add(&format!("!{}", file)).ok();
                override_raw.push('!');
                override_raw.push_str(file);
            }

            override_raw.push('\n');
        }

        let internal_backup = Self { adapter, uuid };

        let backup = match match adapter {
            BackupAdapter::Wings => {
                wings::create_backup(server.clone(), uuid, override_builder.build()?).await
            }
            BackupAdapter::S3 => {
                s3::create_backup(server.clone(), uuid, override_builder.build()?).await
            }
            BackupAdapter::DdupBak => {
                ddup_bak::create_backup(server.clone(), uuid, override_builder.build()?).await
            }
            BackupAdapter::Btrfs => {
                btrfs::create_backup(
                    server.clone(),
                    uuid,
                    override_builder.build()?,
                    override_raw,
                )
                .await
            }
            BackupAdapter::Zfs => {
                zfs::create_backup(
                    server.clone(),
                    uuid,
                    override_builder.build()?,
                    override_raw,
                )
                .await
            }
        } {
            Ok(backup) => backup,
            Err(e) => {
                server
                    .config
                    .client
                    .set_backup_status(
                        uuid,
                        &RawServerBackup {
                            checksum: String::new(),
                            checksum_type: String::new(),
                            size: 0,
                            successful: false,
                            parts: vec![],
                        },
                    )
                    .await?;
                internal_backup.delete(server).await.ok();

                return Err(e);
            }
        };

        server
            .config
            .client
            .set_backup_status(uuid, &backup)
            .await?;
        server
            .websocket
            .send(crate::server::websocket::WebsocketMessage::new(
                crate::server::websocket::WebsocketEvent::ServerBackupCompleted,
                &[uuid.to_string(), serde_json::to_string(&backup).unwrap()],
            ))?;
        server.configuration.write().await.backups.push(uuid);

        tracing::info!(
            "completed backup {} (adapter = {:?}) for server {}",
            uuid,
            adapter,
            server.uuid
        );

        Ok(internal_backup)
    }

    pub async fn list(server: &crate::server::Server) -> Vec<Self> {
        let variants = BackupAdapter::variants();
        let mut results = Vec::with_capacity(variants.len());
        for adapter in variants.iter().copied() {
            results.push(match adapter {
                BackupAdapter::Wings => wings::list_backups(server).await,
                BackupAdapter::S3 => Ok(Vec::new()),
                BackupAdapter::DdupBak => ddup_bak::list_backups(server).await,
                BackupAdapter::Btrfs => btrfs::list_backups(server).await,
                BackupAdapter::Zfs => zfs::list_backups(server).await,
            });
        }

        let mut backups = Vec::new();

        for (adapter, result) in variants.iter().copied().zip(results) {
            match result {
                Ok(uuids) => {
                    for uuid in uuids {
                        backups.push(Self { adapter, uuid });
                    }
                }
                Err(e) => {
                    tracing::error!("Failed to list backups for adapter {:?}: {}", adapter, e);
                }
            }
        }

        backups
    }

    pub async fn restore(
        &self,
        client: &Arc<bollard::Docker>,
        server: &crate::server::Server,
        truncate_directory: bool,
        download_url: Option<String>,
    ) -> Result<(), anyhow::Error> {
        if server.is_locked_state() {
            return Err(anyhow::anyhow!("Server is in a locked state"));
        }

        server
            .restoring
            .store(true, std::sync::atomic::Ordering::SeqCst);
        server
            .stop_with_kill_timeout(client, std::time::Duration::from_secs(30))
            .await;

        tracing::info!(
            server = %server.uuid,
            backup = %self.uuid,
            adapter = ?self.adapter,
            "restoring backup",
        );

        if truncate_directory {
            server.filesystem.truncate_root().await;
        }

        match match self.adapter {
            BackupAdapter::Wings => wings::restore_backup(server.clone(), self.uuid).await,
            BackupAdapter::S3 => s3::restore_backup(server.clone(), download_url).await,
            BackupAdapter::DdupBak => ddup_bak::restore_backup(server.clone(), self.uuid).await,
            BackupAdapter::Btrfs => btrfs::restore_backup(server.clone(), self.uuid).await,
            BackupAdapter::Zfs => zfs::restore_backup(server.clone(), self.uuid).await,
        } {
            Ok(_) => {
                server
                    .restoring
                    .store(false, std::sync::atomic::Ordering::SeqCst);
                server
                    .log_daemon(format!(
                        "Completed server restoration from {} backup.",
                        serde_json::to_value(self.adapter)
                            .unwrap()
                            .as_str()
                            .unwrap()
                    ))
                    .await;
                server
                    .config
                    .client
                    .set_backup_restore_status(self.uuid, true)
                    .await?;
                server
                    .websocket
                    .send(crate::server::websocket::WebsocketMessage::new(
                        crate::server::websocket::WebsocketEvent::ServerBackupRestoreCompleted,
                        &[],
                    ))?;

                tracing::info!(
                    server = %server.uuid,
                    backup = %self.uuid,
                    adapter = ?self.adapter,
                    "completed restore of backup",
                );

                Ok(())
            }
            Err(e) => {
                server
                    .restoring
                    .store(false, std::sync::atomic::Ordering::SeqCst);
                server
                    .config
                    .client
                    .set_backup_restore_status(self.uuid, false)
                    .await?;

                Err(e)
            }
        }
    }

    pub async fn download(
        &self,
        server: &crate::server::Server,
    ) -> Result<(StatusCode, HeaderMap, Body), anyhow::Error> {
        tracing::info!(
            server = %server.uuid,
            backup = %self.uuid,
            adapter = ?self.adapter,
            "downloading backup",
        );

        match self.adapter {
            BackupAdapter::Wings => wings::download_backup(server, self.uuid).await,
            BackupAdapter::S3 => unimplemented!(),
            BackupAdapter::DdupBak => ddup_bak::download_backup(server, self.uuid).await,
            BackupAdapter::Btrfs => btrfs::download_backup(server, self.uuid).await,
            BackupAdapter::Zfs => zfs::download_backup(server, self.uuid).await,
        }
    }

    pub async fn delete(&self, server: &crate::server::Server) -> Result<(), anyhow::Error> {
        tracing::info!(
            server = %server.uuid,
            backup = %self.uuid,
            adapter = ?self.adapter,
            "deleting backup",
        );

        match self.adapter {
            BackupAdapter::Wings => wings::delete_backup(server, self.uuid).await,
            BackupAdapter::S3 => s3::delete_backup(server, self.uuid).await,
            BackupAdapter::DdupBak => ddup_bak::delete_backup(server, self.uuid).await,
            BackupAdapter::Btrfs => btrfs::delete_backup(server, self.uuid).await,
            BackupAdapter::Zfs => zfs::delete_backup(server, self.uuid).await,
        }
    }
}
