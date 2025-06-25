use utoipa_axum::router::OpenApiRouter;
use wings_rs::routes::State;

mod random;

pub fn router(state: &State) -> OpenApiRouter<State> {
    OpenApiRouter::new()
        .merge(random::router(state))
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            wings_rs::routes::api::auth,
        ))
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            wings_rs::routes::api::servers::_server_::auth,
        ))
        .with_state(state.clone())
}
