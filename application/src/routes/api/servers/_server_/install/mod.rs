use super::State;
use utoipa_axum::router::OpenApiRouter;

mod abort;

pub fn router(state: &State) -> OpenApiRouter<State> {
    OpenApiRouter::new()
        .nest("/abort", abort::router(state))
        .with_state(state.clone())
}
