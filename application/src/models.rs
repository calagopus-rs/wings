use crate::server::state::ServerState;
use serde::{Deserialize, Serialize};
use std::str::FromStr;
use utoipa::ToSchema;

#[derive(ToSchema, Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
#[schema(rename_all = "lowercase")]
pub enum ServerPowerAction {
    Start,
    Stop,
    Restart,
    Kill,
}

impl FromStr for ServerPowerAction {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "start" => Ok(Self::Start),
            "stop" => Ok(Self::Stop),
            "restart" => Ok(Self::Restart),
            "kill" => Ok(Self::Kill),
            _ => Err(anyhow::anyhow!(
                "invalid server power action provided: {}",
                s
            )),
        }
    }
}

#[derive(ToSchema, Serialize)]
pub struct Server {
    pub state: ServerState,
    pub is_suspended: bool,
    pub utilization: crate::server::resources::ResourceUsage,
    pub configuration: crate::server::configuration::ServerConfiguration,
}

#[derive(ToSchema, Serialize)]
pub struct DirectoryEntry {
    pub name: String,
    pub created: chrono::DateTime<chrono::Utc>,
    pub modified: chrono::DateTime<chrono::Utc>,
    pub mode: String,
    pub mode_bits: String,
    pub size: u64,
    pub directory: bool,
    pub file: bool,
    pub symlink: bool,
    pub mime: &'static str,
}

#[derive(ToSchema, Serialize)]
pub struct Download {
    pub identifier: uuid::Uuid,
    pub destination: String,

    pub progress: u64,
    pub total: u64,
}

#[derive(ToSchema, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PullProgressStatus {
    Pulling,
    Extracting,
}

#[derive(ToSchema, Serialize)]
pub struct PullProgress {
    pub status: PullProgressStatus,
    pub progress: i64,
    pub total: i64,
}

#[derive(ToSchema, Serialize)]
pub struct Progress {
    pub progress: u64,
    pub total: u64,
}
