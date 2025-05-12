use crate::server::state::ServerState;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

#[derive(ToSchema, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
#[schema(rename_all = "lowercase")]
pub enum ServerPowerAction {
    Start,
    Stop,
    Restart,
    Kill,
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

    pub progress: u64,
    pub total: u64,
}
