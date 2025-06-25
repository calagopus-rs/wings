use crate::routes::State;
use serde::Serialize;
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;

pub mod manager;

pub const API_VERSION: &str = env!("CARGO_GIT_COMMIT");

#[derive(Debug, ToSchema, Serialize)]
pub struct ExtensionInfo {
    pub name: &'static str,
    pub description: &'static str,
    pub version: &'static str,

    pub author: &'static str,
    pub license: &'static str,

    pub additional: serde_json::value::Map<String, serde_json::Value>,
}

#[allow(unused_variables)]
pub trait Extension: Send + Sync + 'static {
    fn info(&self) -> ExtensionInfo;

    fn on_init(&mut self, state: State);

    fn router(&mut self, state: State) -> OpenApiRouter<crate::routes::State> {
        OpenApiRouter::new().with_state(state)
    }
}

#[macro_export]
macro_rules! export_extension {
    ($struct_name:ident) => {
        #[unsafe(no_mangle)]
        #[allow(improper_ctypes_definitions)]
        pub extern "C" fn load_extension() -> Box<dyn wings_rs::extensions::Extension> {
            Box::new($struct_name::default())
        }

        #[unsafe(no_mangle)]
        pub extern "C" fn api_version() -> &'static str {
            wings_rs::extensions::API_VERSION
        }
    };
}
