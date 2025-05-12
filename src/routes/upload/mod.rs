use super::State;
use utoipa_axum::router::OpenApiRouter;

mod file;

pub fn router(state: &State) -> OpenApiRouter<State> {
    OpenApiRouter::new()
        .nest("/file", file::router(state))
        .with_state(state.clone())
}
