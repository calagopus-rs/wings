use super::client::Client;
use crate::server::backup::adapters::BackupAdapter;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::BTreeMap;
use utoipa::ToSchema;

#[derive(Debug, ToSchema, Serialize)]
pub struct RawServerBackupPart {
    pub etag: String,
    pub part_number: usize,
}

#[derive(Debug, ToSchema, Serialize)]
pub struct RawServerBackup {
    pub checksum: String,
    pub checksum_type: String,
    pub size: u64,
    pub files: u64,
    pub successful: bool,
    pub parts: Vec<RawServerBackupPart>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ResticBackupConfiguration {
    pub repository: String,
    pub password_file: Option<String>,
    pub retry_lock_seconds: u64,
    pub environment: BTreeMap<String, String>,
}

impl ResticBackupConfiguration {
    #[inline]
    pub fn password(&self) -> Vec<String> {
        if let Some(password_file) = &self.password_file {
            vec!["--password-file".into(), password_file.clone()]
        } else {
            Vec::new()
        }
    }
}

pub async fn set_backup_status(
    client: &Client,
    uuid: uuid::Uuid,
    data: &RawServerBackup,
) -> Result<(), anyhow::Error> {
    client
        .client
        .post(format!("{}/backups/{}", client.url, uuid))
        .json(data)
        .send()
        .await?
        .error_for_status()?;

    Ok(())
}

pub async fn set_backup_restore_status(
    client: &Client,
    uuid: uuid::Uuid,
    successful: bool,
) -> Result<(), anyhow::Error> {
    client
        .client
        .post(format!("{}/backups/{}/restore", client.url, uuid))
        .json(&json!({
            "successful": successful,
        }))
        .send()
        .await?
        .error_for_status()?;

    Ok(())
}

pub async fn backup_upload_urls(
    client: &Client,
    uuid: uuid::Uuid,
    size: u64,
) -> Result<(u64, Vec<String>), anyhow::Error> {
    let response: Response = super::into_json(
        client
            .client
            .get(format!("{}/backups/{}?size={}", client.url, uuid, size))
            .send()
            .await?
            .error_for_status()?
            .text()
            .await?,
    )?;

    #[derive(Deserialize)]
    struct Response {
        parts: Vec<String>,
        part_size: u64,
    }

    Ok((response.part_size, response.parts))
}

pub async fn backup_restic_configuration(
    client: &Client,
    uuid: uuid::Uuid,
) -> Result<ResticBackupConfiguration, anyhow::Error> {
    let response: ResticBackupConfiguration = super::into_json(
        client
            .client
            .get(format!("{}/backups/{}/restic", client.url, uuid))
            .send()
            .await?
            .error_for_status()?
            .text()
            .await?,
    )?;

    Ok(response)
}

pub async fn create_backup(
    client: &Client,
    server: uuid::Uuid,
    name: Option<&str>,
    ignored_files: &[String],
) -> Result<(BackupAdapter, uuid::Uuid), anyhow::Error> {
    let response: Response = super::into_json(
        client
            .client
            .post(format!("{}/servers/{}/backups", client.url, server))
            .json(&json!({
                "name": name,
                "ignored_files": ignored_files,
            }))
            .send()
            .await?
            .error_for_status()?
            .text()
            .await?,
    )?;

    #[derive(Deserialize)]
    struct Response {
        adapter: BackupAdapter,
        uuid: uuid::Uuid,
    }

    Ok((response.adapter, response.uuid))
}
