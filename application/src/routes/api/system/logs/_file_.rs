use super::State;
use utoipa_axum::{router::OpenApiRouter, routes};

mod get {
    use crate::{
        io::compression::reader::AsyncCompressionReader,
        response::{ApiResponse, ApiResponseResult},
        routes::{ApiError, GetState},
    };
    use axum::extract::Path;
    use tokio::io::AsyncRead;

    #[utoipa::path(get, path = "/", responses(
        (status = OK, body = String),
        (status = NOT_FOUND, body = ApiError)
    ), params(
        (
            "file" = String,
            description = "The log file name",
            example = "wings.log",
        ),
    ))]
    pub async fn route(state: GetState, Path(file_path): Path<String>) -> ApiResponseResult {
        if file_path.contains("..") {
            return ApiResponse::error("log file not found").ok();
        }

        let file = match tokio::fs::File::open(
            std::path::Path::new(&state.config.system.log_directory).join(&file_path),
        )
        .await
        {
            Ok(file) => file,
            Err(_) => return ApiResponse::error("log file not found").ok(),
        };

        let reader: Box<dyn AsyncRead + Send + Unpin> = if file_path.ends_with(".gz") {
            Box::new(AsyncCompressionReader::new(
                file.into_std().await,
                crate::io::compression::CompressionType::Gz,
            ))
        } else {
            Box::new(file)
        };

        ApiResponse::new(axum::body::Body::from_stream(
            tokio_util::io::ReaderStream::with_capacity(reader, crate::BUFFER_SIZE),
        ))
        .with_header("Content-Type", "text/plain")
        .ok()
    }
}

pub fn router(state: &State) -> OpenApiRouter<State> {
    OpenApiRouter::new()
        .routes(routes!(get::route))
        .with_state(state.clone())
}
