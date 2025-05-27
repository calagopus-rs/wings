pub mod commands;
pub mod config;
pub mod extensions;
pub mod models;
pub mod remote;
pub mod routes;
pub mod server;
pub mod sftp;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const GIT_COMMIT: &str = env!("CARGO_GIT_COMMIT");
