use super::State;
use utoipa_axum::{router::OpenApiRouter, routes};

mod get {
    use crate::{
        response::{ApiResponse, ApiResponseResult},
        routes::GetState,
    };

    #[utoipa::path(get, path = "/", responses(
        (status = OK, body = inline(crate::config::InnerConfig)),
    ))]
    pub async fn route(state: GetState) -> ApiResponseResult {
        ApiResponse::json(&**state.config).ok()
    }
}

pub fn router(state: &State) -> OpenApiRouter<State> {
    OpenApiRouter::new()
        .routes(routes!(get::route))
        .with_state(state.clone())
}
