use super::State;
use utoipa_axum::{router::OpenApiRouter, routes};

mod get {
    use crate::routes::{GetState, api::servers::_server_::GetServer};
    use axum::extract::Query;
    use serde::{Deserialize, Serialize};
    use utoipa::ToSchema;

    #[derive(ToSchema, Deserialize)]
    pub struct Params {
        size: Option<usize>,
    }

    #[derive(ToSchema, Serialize)]
    struct Response {
        data: String,
    }

    #[utoipa::path(get, path = "/", responses(
        (status = OK, body = inline(Response)),
    ))]
    pub async fn route(
        state: GetState,
        server: GetServer,
        Query(data): Query<Params>,
    ) -> axum::Json<serde_json::Value> {
        let size = data.size.unwrap_or(100).min(100);
        let log = server.read_log(&state.docker, size).await.unwrap();

        axum::Json(serde_json::to_value(&Response { data: log }).unwrap())
    }
}

pub fn router(state: &State) -> OpenApiRouter<State> {
    OpenApiRouter::new()
        .routes(routes!(get::route))
        .with_state(state.clone())
}
