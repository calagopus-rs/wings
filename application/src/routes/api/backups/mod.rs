use super::State;
use utoipa_axum::router::OpenApiRouter;

mod _backup_;
mod sync;

pub fn router(state: &State) -> OpenApiRouter<State> {
    OpenApiRouter::new()
        .nest("/{backup}", _backup_::router(state))
        .nest("/sync", sync::router(state))
        .with_state(state.clone())
}
