use bollard::Docker;
use serde::Serialize;
use std::{sync::Arc, time::Instant};
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;

pub mod api;
mod download;
mod upload;

pub struct AppState {
    pub config: Arc<crate::config::Config>,
    pub start_time: Instant,
    pub version: String,

    pub docker: Arc<Docker>,
    pub stats_manager: Arc<crate::stats::StatsManager>,
    pub server_manager: Arc<crate::server::manager::ServerManager>,
    pub backup_manager: Arc<crate::server::backup::manager::BackupManager>,
    pub mime_cache: Arc<crate::server::filesystem::mime::MimeCache<(u64, u64)>>,
}

#[derive(ToSchema, Serialize)]
pub struct ApiError<'a> {
    pub error: &'a str,
}

impl<'a> ApiError<'a> {
    #[inline]
    pub fn new(error: &'a str) -> Self {
        Self { error }
    }

    #[inline]
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap()
    }
}

pub type State = Arc<AppState>;
pub type GetState = axum::extract::State<State>;

pub fn router(state: &State) -> OpenApiRouter<State> {
    OpenApiRouter::new()
        .nest("/download", download::router(state))
        .nest("/upload", upload::router(state))
        .nest("/api", api::router(state))
        .with_state(state.clone())
}
