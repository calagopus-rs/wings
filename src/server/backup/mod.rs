use crate::remote::backups::RawServerBackup;
use axum::{
    body::Body,
    http::{HeaderMap, StatusCode},
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use utoipa::ToSchema;

mod wings;

#[derive(ToSchema, Clone, Copy, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "lowercase")]
#[schema(rename_all = "lowercase")]
pub enum BackupAdapter {
    Wings,
    S3,
    DdupBak,
}

pub async fn create_backup(
    adapter: BackupAdapter,
    server: &Arc<crate::server::Server>,
    uuid: uuid::Uuid,
    mut ignore: String,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    if let Some(pteroignore) = server.filesystem.get_pteroignore().await {
        ignore.push('\n');
        ignore.push_str(&pteroignore);
    }

    let backup = match match adapter {
        BackupAdapter::Wings => wings::create_backup(server, uuid, ignore).await,
        BackupAdapter::S3 => todo!(),
        BackupAdapter::DdupBak => todo!(),
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

    match match adapter {
        BackupAdapter::Wings => wings::restore_backup(server, uuid, truncate_directory).await,
        BackupAdapter::S3 => todo!(),
        BackupAdapter::DdupBak => todo!(),
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
        BackupAdapter::DdupBak => todo!(),
    }
}

pub async fn delete_backup(
    adapter: BackupAdapter,
    server: &Arc<crate::server::Server>,
    uuid: uuid::Uuid,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    match adapter {
        BackupAdapter::Wings => wings::delete_backup(server, uuid).await,
        BackupAdapter::S3 => unimplemented!(),
        BackupAdapter::DdupBak => todo!(),
    }
}

pub async fn list_backups(
    adapter: BackupAdapter,
    server: &Arc<crate::server::Server>,
) -> Result<Vec<uuid::Uuid>, Box<dyn std::error::Error + Send + Sync>> {
    match adapter {
        BackupAdapter::Wings => wings::list_backups(server).await,
        BackupAdapter::S3 => unimplemented!(),
        BackupAdapter::DdupBak => todo!(),
    }
}
