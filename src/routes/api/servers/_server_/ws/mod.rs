use super::State;
use utoipa_axum::router::OpenApiRouter;

mod deny;

pub fn router(state: &State) -> OpenApiRouter<State> {
    OpenApiRouter::new()
        .nest("/deny", deny::router(state))
        .with_state(state.clone())
}
