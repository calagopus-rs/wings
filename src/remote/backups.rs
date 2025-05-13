use super::client::Client;
use serde::{Deserialize, Serialize};
use serde_json::json;
use utoipa::ToSchema;

#[derive(ToSchema, Serialize)]
pub struct RawServerBackupPart {
    pub etag: String,
    pub part_number: usize,
}

#[derive(ToSchema, Serialize)]
pub struct RawServerBackup {
    pub checksum: String,
    pub checksum_type: String,
    pub size: u64,
    pub successful: bool,
    pub parts: Vec<RawServerBackupPart>,
}

pub async fn set_backup_status(
    client: &Client,
    uuid: uuid::Uuid,
    data: &RawServerBackup,
) -> Result<(), reqwest::Error> {
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
) -> Result<(), reqwest::Error> {
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
) -> Result<(u64, Vec<String>), reqwest::Error> {
    let response: Response = client
        .client
        .get(format!("{}/backups/{}?size={}", client.url, uuid, size))
        .send()
        .await?
        .json()
        .await?;

    #[derive(Deserialize)]
    struct Response {
        parts: Vec<String>,
        part_size: u64,
    }

    Ok((response.part_size, response.parts))
}
