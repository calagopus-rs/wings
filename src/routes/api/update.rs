use super::State;
use utoipa_axum::{router::OpenApiRouter, routes};

mod post {
    use crate::routes::GetState;
    use serde::{Deserialize, Serialize};
    use utoipa::ToSchema;

    #[derive(ToSchema, Deserialize)]
    pub struct Payload {}

    #[derive(ToSchema, Serialize)]
    struct Response {
        applied: bool,
    }

    #[utoipa::path(post, path = "/", responses(
        (status = OK, body = inline(Response)),
    ), request_body = inline(Payload))]
    pub async fn route(
        state: GetState,
        axum::Json(data): axum::Json<Payload>,
    ) -> axum::Json<serde_json::Value> {
        if state.config.ignore_panel_config_updates {
            return axum::Json(serde_json::to_value(&Response { applied: false }).unwrap());
        }

        //state.config.update(data).unwrap();

        axum::Json(serde_json::to_value(&Response { applied: true }).unwrap())
    }
}

pub fn router(state: &State) -> OpenApiRouter<State> {
    OpenApiRouter::new()
        .routes(routes!(post::route))
        .with_state(state.clone())
}
