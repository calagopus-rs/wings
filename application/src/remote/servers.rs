use super::client::Client;
use crate::server::installation::InstallationScript;
use serde::Deserialize;
use serde_json::json;
use utoipa::ToSchema;

#[derive(ToSchema, Deserialize)]
pub struct RawServer {
    pub settings: crate::server::configuration::ServerConfiguration,
    pub process_configuration: crate::server::configuration::process::ProcessConfiguration,
}

pub async fn get_servers_paged(
    client: &Client,
    page: usize,
) -> Result<(Vec<RawServer>, super::Pagination), anyhow::Error> {
    let response: Response = super::into_json(
        client
            .client
            .get(format!(
                "{}/servers?page={}&per_page={}",
                client.url, page, client.config.boot_servers_per_page
            ))
            .send()
            .await?
            .text()
            .await?,
    )?;

    #[derive(Deserialize, Default)]
    struct Response {
        data: Vec<RawServer>,
        meta: super::Pagination,
    }

    Ok((response.data, response.meta))
}

pub async fn get_server(client: &Client, uuid: uuid::Uuid) -> Result<RawServer, anyhow::Error> {
    let response = super::into_json(
        client
            .client
            .get(format!("{}/servers/{}", client.url, uuid))
            .send()
            .await?
            .text()
            .await?,
    )?;

    Ok(response)
}

pub async fn get_server_install_script(
    client: &Client,
    uuid: uuid::Uuid,
) -> Result<InstallationScript, anyhow::Error> {
    let response = super::into_json(
        client
            .client
            .get(format!("{}/servers/{}/install", client.url, uuid))
            .send()
            .await?
            .text()
            .await?,
    )?;

    Ok(response)
}

pub async fn set_server_install(
    client: &Client,
    uuid: uuid::Uuid,
    successful: bool,
    reinstalled: bool,
) -> Result<(), anyhow::Error> {
    client
        .client
        .post(format!("{}/servers/{}/install", client.url, uuid))
        .json(&json!({
            "successful": successful,
            "reinstall": reinstalled
        }))
        .send()
        .await?;

    Ok(())
}

pub async fn set_server_transfer(
    client: &Client,
    uuid: uuid::Uuid,
    successful: bool,
    backups: Vec<uuid::Uuid>,
) -> Result<(), anyhow::Error> {
    client
        .client
        .post(format!(
            "{}/servers/{}/transfer/{}",
            client.url,
            uuid,
            if successful { "success" } else { "failure" }
        ))
        .json(&json!({
            "backups": backups
        }))
        .send()
        .await?;

    Ok(())
}
