use super::client::Client;
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
    pub successful: bool,
    pub parts: Vec<RawServerBackupPart>,
}

#[derive(Debug, Deserialize)]
pub struct BackupConfigurationsRestic {
    pub repository: String,
    pub retry_lock_seconds: u64,
    pub environment: BTreeMap<String, String>,
}

#[derive(Debug, Deserialize, Default)]
pub struct BackupConfigurations {
    pub restic: Option<BackupConfigurationsRestic>,
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
        .await?;

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
        .await?;

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

pub async fn backup_configurations(client: &Client) -> Result<BackupConfigurations, anyhow::Error> {
    let response: BackupConfigurations = super::into_json(
        client
            .client
            .get(format!("{}/backups", client.url))
            .send()
            .await?
            .text()
            .await?,
    )?;

    Ok(response)
}
