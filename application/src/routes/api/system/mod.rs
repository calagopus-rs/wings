use super::State;
use utoipa_axum::{router::OpenApiRouter, routes};

mod logs;
mod upgrade;

mod get {
    use crate::{
        response::{ApiResponse, ApiResponseResult},
        routes::GetState,
    };
    use serde::Serialize;
    use std::sync::LazyLock;
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
    pub async fn route(state: GetState) -> ApiResponseResult {
        static KERNEL_VERSION: LazyLock<String> = LazyLock::new(|| {
            rustix::system::uname()
                .release()
                .to_string_lossy()
                .to_string()
        });

        ApiResponse::json(Response {
            architecture: std::env::consts::ARCH,
            cpu_count: rayon::current_num_threads(),
            kernel_version: &KERNEL_VERSION,
            os: std::env::consts::OS,
            version: &state.version,
        })
        .ok()
    }
}

pub fn router(state: &State) -> OpenApiRouter<State> {
    OpenApiRouter::new()
        .routes(routes!(get::route))
        .nest("/logs", logs::router(state))
        .nest("/upgrade", upgrade::router(state))
        .with_state(state.clone())
}
