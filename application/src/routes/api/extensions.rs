use super::State;
use utoipa_axum::{router::OpenApiRouter, routes};

mod get {
    use crate::{extensions::ExtensionInfo, routes::GetState};
    use serde::Serialize;
    use utoipa::ToSchema;

    #[derive(ToSchema, Serialize)]
    struct Response {
        extensions: Vec<ExtensionInfo>,
    }

    #[utoipa::path(get, path = "/", responses(
        (status = OK, body = inline(Response)),
    ))]
    pub async fn route(state: GetState) -> axum::Json<serde_json::Value> {
        axum::Json(
            serde_json::to_value(Response {
                extensions: state
                    .extension_manager
                    .get_extensions()
                    .iter()
                    .map(|ext| ext.info())
                    .collect(),
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
