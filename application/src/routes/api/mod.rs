use super::{ApiError, GetState, State};
use axum::{
    body::Body,
    extract::Request,
    http::{Response, StatusCode},
    middleware::Next,
    routing::any,
};
use utoipa_axum::router::OpenApiRouter;

mod extensions;
pub mod servers;
mod stats;
mod system;
mod transfers;
mod update;

pub async fn auth(state: GetState, req: Request, next: Next) -> Result<Response<Body>, StatusCode> {
    let key = req
        .headers()
        .get("Authorization")
        .map(|v| v.to_str().unwrap())
        .unwrap_or("")
        .to_string();
    let mut parts = key.splitn(2, " ");
    let r#type = parts.next().unwrap();
    let token = parts.next();

    if r#type != "Bearer" {
        return Ok(Response::builder()
            .status(StatusCode::UNAUTHORIZED)
            .header("WWW-Authenticate", "Bearer")
            .header("Content-Type", "application/json")
            .body(Body::from(
                serde_json::to_string(&ApiError::new("invalid authorization token")).unwrap(),
            ))
            .unwrap());
    }

    let token = match token {
        Some(t) => t,
        None => {
            return Ok(Response::builder()
                .status(StatusCode::UNAUTHORIZED)
                .header("WWW-Authenticate", "Bearer")
                .header("Content-Type", "application/json")
                .body(Body::from(
                    serde_json::to_string(&ApiError::new("invalid authorization token")).unwrap(),
                ))
                .unwrap());
        }
    };

    if !constant_time_eq::constant_time_eq(token.as_bytes(), state.config.token.as_bytes()) {
        return Ok(Response::builder()
            .status(StatusCode::UNAUTHORIZED)
            .header("WWW-Authenticate", "Bearer")
            .header("Content-Type", "application/json")
            .body(Body::from(
                serde_json::to_string(&ApiError::new("invalid authorization token")).unwrap(),
            ))
            .unwrap());
    }

    Ok(next.run(req).await)
}

pub fn router(state: &State) -> OpenApiRouter<State> {
    OpenApiRouter::new()
        .nest(
            "/system",
            system::router(state)
                .route_layer(axum::middleware::from_fn_with_state(state.clone(), auth)),
        )
        .nest(
            "/stats",
            stats::router(state)
                .route_layer(axum::middleware::from_fn_with_state(state.clone(), auth)),
        )
        .nest(
            "/extensions",
            extensions::router(state)
                .route_layer(axum::middleware::from_fn_with_state(state.clone(), auth)),
        )
        .nest(
            "/update",
            update::router(state)
                .route_layer(axum::middleware::from_fn_with_state(state.clone(), auth)),
        )
        .nest("/transfers", transfers::router(state))
        .nest(
            "/servers",
            servers::router(state)
                .route_layer(axum::middleware::from_fn_with_state(state.clone(), auth)),
        )
        .route(
            "/servers/{server}/ws",
            any(crate::server::websocket::handler::handle_ws),
        )
        .with_state(state.clone())
}
