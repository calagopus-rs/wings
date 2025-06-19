use crate::server::{activity::ApiActivity, permissions::Permissions};
use client::Client;
use serde::{Deserialize, Serialize};
use serde_json::json;

pub mod backups;
pub mod client;
pub mod jwt;
pub mod servers;

#[derive(Deserialize, Serialize, Default)]
pub struct Pagination {
    current_page: usize,
    last_page: usize,
    total: usize,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthenticationType {
    Password,
    PublicKey,
}

pub async fn get_sftp_auth(
    client: &Client,
    r#type: AuthenticationType,
    username: &str,
    password: &str,
) -> Result<(uuid::Uuid, uuid::Uuid, Permissions), reqwest::Error> {
    let response: Response = client
        .client
        .post(format!("{}/sftp/auth", client.url))
        .json(&json!({
            "type": r#type,
            "username": username,
            "password": password,
        }))
        .send()
        .await?
        .json()
        .await?;

    #[derive(Deserialize)]
    pub struct Response {
        user: uuid::Uuid,
        server: uuid::Uuid,

        permissions: Permissions,
    }

    Ok((response.user, response.server, response.permissions))
}

pub async fn send_activity(
    client: &Client,
    activity: Vec<ApiActivity>,
) -> Result<(), reqwest::Error> {
    client
        .client
        .post(format!("{}/activity", client.url))
        .json(&json!({
            "data": activity,
        }))
        .send()
        .await?;

    Ok(())
}
