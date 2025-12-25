use super::State;
use utoipa_axum::{router::OpenApiRouter, routes};

mod get {
    use crate::{
        response::{ApiResponse, ApiResponseResult},
        routes::{ApiError, api::servers::_server_::GetServer},
    };
    use axum::http::StatusCode;

    #[utoipa::path(get, path = "/", responses(
        (status = OK, body = String),
        (status = NOT_FOUND, body = ApiError),
    ), params(
        (
            "server" = uuid::Uuid,
            description = "The server uuid",
            example = "123e4567-e89b-12d3-a456-426614174000",
        ),
    ))]
    pub async fn route(server: GetServer) -> ApiResponseResult {
        let log_file =
            match crate::server::installation::ServerInstaller::get_install_logs(&server).await {
                Ok(file) => file,
                Err(_) => {
                    return ApiResponse::error("unable to find installation log file")
                        .with_status(StatusCode::NOT_FOUND)
                        .ok();
                }
            };

        ApiResponse::new_stream(log_file).ok()
    }
}

pub fn router(state: &State) -> OpenApiRouter<State> {
    OpenApiRouter::new()
        .routes(routes!(get::route))
        .with_state(state.clone())
}
