use super::State;
use utoipa_axum::router::OpenApiRouter;

mod _schedule_;

pub fn router(state: &State) -> OpenApiRouter<State> {
    OpenApiRouter::new()
        .nest("/{schedule}", _schedule_::router(state))
        .with_state(state.clone())
}
