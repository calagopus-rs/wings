use super::State;
use utoipa_axum::{router::OpenApiRouter, routes};

mod get {
    use crate::routes::GetState;
    use serde::Serialize;
    use tokio::process::Command;
    use utoipa::ToSchema;

    #[derive(ToSchema, Serialize)]
    struct Response<'a> {
        architecture: &'static str,
        cpu_count: usize,
        kernel_version: &'a str,
        os: &'static str,
        version: &'a str,
    }

    #[utoipa::path(get, path = "/", responses(
        (status = OK, body = inline(Response)),
    ))]
    pub async fn route(state: GetState) -> axum::Json<serde_json::Value> {
        let kernel_version = Command::new("uname")
            .arg("-r")
            .output()
            .await
            .map(|output| String::from_utf8_lossy(&output.stdout).to_string())
            .unwrap_or_else(|_| "unknown".to_string());

        axum::Json(
            serde_json::to_value(Response {
                architecture: std::env::consts::ARCH,
                cpu_count: rayon::current_num_threads(),
                kernel_version: kernel_version.trim(),
                os: std::env::consts::OS,
                version: &state.version,
            })
            .unwrap(),
        )
    }
}

pub fn router(state: &State) -> OpenApiRouter<State> {
    OpenApiRouter::new()
        .routes(routes!(get::route))
        .with_state(state.clone())
}
