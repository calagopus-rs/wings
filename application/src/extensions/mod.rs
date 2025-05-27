use crate::routes::State;
use serde::Serialize;
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;

pub mod manager;

pub const API_VERSION: u32 = 1;

#[repr(C)]
#[derive(Debug, ToSchema, Serialize)]
pub struct ExtensionInfo {
    pub name: &'static str,
    pub description: &'static str,
    pub version: &'static str,

    pub author: &'static str,
    pub license: &'static str,

    pub additional: serde_json::value::Map<String, serde_json::Value>,
}

pub trait Extension: Send + Sync + 'static {
    fn info(&self) -> ExtensionInfo;

    #[allow(unused_variables)]
    fn on_init(&self, state: State) {}

    fn router(&self, state: State) -> OpenApiRouter<crate::routes::State> {
        OpenApiRouter::new().with_state(state)
    }
}
