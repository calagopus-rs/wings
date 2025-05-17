use crate::remote::backups::RawServerBackup;
use axum::{
    body::Body,
    http::{HeaderMap, StatusCode},
};
use ignore::overrides::OverrideBuilder;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use utoipa::ToSchema;

mod ddup_bak;
mod s3;
mod wings;

#[derive(ToSchema, Clone, Copy, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "kebab-case")]
#[schema(rename_all = "kebab-case")]
pub enum BackupAdapter {
    Wings,
    S3,
    DdupBak,
}

pub async fn create_backup(
    adapter: BackupAdapter,
    server: &Arc<crate::server::Server>,
    uuid: uuid::Uuid,
    ignore: String,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut override_builder = OverrideBuilder::new(&server.filesystem.base_path);

    for line in ignore.lines() {
        if line.trim().is_empty() {
            continue;
        }

        if let Some(line) = line.trim().strip_prefix('!') {
            override_builder.add(line).ok();
        } else {
            override_builder.add(&format!("!{}", line.trim())).ok();
        }
    }

    for file in &server.configuration.read().await.egg.file_denylist {
        if let Some(file) = file.strip_prefix('!') {
            override_builder.add(file).ok();
        } else {
            override_builder.add(&format!("!{}", file)).ok();
        }
    }

    if let Some(pteroignore) = server.filesystem.get_pteroignore().await {
        for line in pteroignore.lines() {
            if line.trim().is_empty() {
                continue;
            }

            if let Some(line) = line.trim().strip_prefix('!') {
                override_builder.add(line).ok();
            } else {
                override_builder.add(&format!("!{}", line.trim())).ok();
            }
        }
    }

    let backup = match match adapter {
        BackupAdapter::Wings => wings::create_backup(server, uuid, override_builder.build()?).await,
        BackupAdapter::S3 => s3::create_backup(server, uuid, override_builder.build()?).await,
        BackupAdapter::DdupBak => {
            ddup_bak::create_backup(server, uuid, override_builder.build()?).await
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
            delete_backup(adapter, server, uuid).await.ok();

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
            &[uuid.to_string()],
        ))?;

    Ok(())
}

pub async fn restore_backup(
    adapter: BackupAdapter,
    client: &Arc<bollard::Docker>,
    server: &Arc<crate::server::Server>,
    uuid: uuid::Uuid,
    truncate_directory: bool,
    download_url: Option<String>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    if server.is_locked_state() {
        return Err("Server is in a locked state".into());
    }

    server
        .restoring
        .store(true, std::sync::atomic::Ordering::SeqCst);
    server
        .stop_with_kill_timeout(client, std::time::Duration::from_secs(30))
        .await;

    if truncate_directory {
        server.filesystem.truncate_root().await;
    }

    match match adapter {
        BackupAdapter::Wings => wings::restore_backup(server, uuid).await,
        BackupAdapter::S3 => s3::restore_backup(server, download_url).await,
        BackupAdapter::DdupBak => ddup_bak::restore_backup(server, uuid).await,
    } {
        Ok(_) => {
            server
                .restoring
                .store(false, std::sync::atomic::Ordering::SeqCst);
            server
                .log_daemon(format!(
                    "Completed server restoration from {} backup.",
                    serde_json::to_value(adapter).unwrap().as_str().unwrap()
                ))
                .await;
            server
                .config
                .client
                .set_backup_restore_status(uuid, true)
                .await?;
            server
                .websocket
                .send(crate::server::websocket::WebsocketMessage::new(
                    crate::server::websocket::WebsocketEvent::ServerBackupRestoreCompleted,
                    &[],
                ))?;

            Ok(())
        }
        Err(e) => {
            server
                .restoring
                .store(false, std::sync::atomic::Ordering::SeqCst);
            server
                .config
                .client
                .set_backup_restore_status(uuid, false)
                .await?;

            Err(e)
        }
    }
}

pub async fn download_backup(
    adapter: BackupAdapter,
    server: &Arc<crate::server::Server>,
    uuid: uuid::Uuid,
) -> Result<(StatusCode, HeaderMap, Body), Box<dyn std::error::Error + Send + Sync>> {
    match adapter {
        BackupAdapter::Wings => wings::download_backup(server, uuid).await,
        BackupAdapter::S3 => unimplemented!(),
        BackupAdapter::DdupBak => ddup_bak::download_backup(server, uuid).await,
    }
}

pub async fn delete_backup(
    adapter: BackupAdapter,
    server: &Arc<crate::server::Server>,
    uuid: uuid::Uuid,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    match adapter {
        BackupAdapter::Wings => wings::delete_backup(server, uuid).await,
        BackupAdapter::S3 => s3::delete_backup(server, uuid).await,
        BackupAdapter::DdupBak => ddup_bak::delete_backup(server, uuid).await,
    }
}

pub async fn list_backups(
    adapter: BackupAdapter,
    server: &Arc<crate::server::Server>,
) -> Result<Vec<uuid::Uuid>, Box<dyn std::error::Error + Send + Sync>> {
    match adapter {
        BackupAdapter::Wings => wings::list_backups(server).await,
        BackupAdapter::S3 => s3::list_backups(server).await,
        BackupAdapter::DdupBak => ddup_bak::list_backups(server).await,
    }
}
