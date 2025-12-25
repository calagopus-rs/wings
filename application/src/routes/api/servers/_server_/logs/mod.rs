use super::State;
use utoipa_axum::{router::OpenApiRouter, routes};

mod install;

mod get {
    use crate::{
        response::{ApiResponse, ApiResponseResult},
        routes::api::servers::_server_::GetServer,
    };
    use axum::extract::Query;
    use futures::StreamExt;
    use serde::Deserialize;
    use tokio::io::AsyncWriteExt;
    use utoipa::ToSchema;

    #[derive(ToSchema, Deserialize)]
    pub struct Params {
        lines: Option<usize>,
    }

    #[utoipa::path(get, path = "/", responses(
        (status = OK, body = String),
    ), params(
        (
            "server" = uuid::Uuid,
            description = "The server uuid",
            example = "123e4567-e89b-12d3-a456-426614174000",
        ),
        (
            "lines" = Option<usize>, Query,
            description = "The number of lines to tail from the log",
            example = "100",
        ),
    ))]
    pub async fn route(server: GetServer, Query(data): Query<Params>) -> ApiResponseResult {
        let mut log_stream = server.read_log(data.lines).await;

        let (logs_reader, mut logs_writer) = tokio::io::duplex(crate::BUFFER_SIZE);

        tokio::spawn(async move {
            while let Some(Ok(line)) = log_stream.next().await {
                if logs_writer.write_all(line.as_bytes()).await.is_err() {
                    break;
                }
            }
        });

        ApiResponse::new_stream(logs_reader).ok()
    }
}

pub fn router(state: &State) -> OpenApiRouter<State> {
    OpenApiRouter::new()
        .routes(routes!(get::route))
        .nest("/install", install::router(state))
        .with_state(state.clone())
}
