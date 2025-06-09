use chrono::Datelike;
use clap::ArgMatches;
use std::sync::Arc;

const TARGET: &str = env!("CARGO_TARGET");

pub async fn version(_matches: &ArgMatches, _config: Option<&Arc<crate::config::Config>>) -> i32 {
    println!(
        "wings-rs {}:{} ({})",
        crate::VERSION,
        crate::GIT_COMMIT,
        TARGET
    );
    println!(
        "copyright © 2025 - {} 0x7d8 & Contributors",
        chrono::Local::now().year()
    );

    0
}
