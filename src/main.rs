use axum::{
    body::Body,
    extract::{DefaultBodyLimit, Request},
    http::{HeaderMap, Method, Response, StatusCode},
    middleware::Next,
};
use bollard::Docker;
use colored::Colorize;
use routes::ApiError;
use std::{net::SocketAddr, sync::Arc, time::Instant};
use tower_http::catch_panic::CatchPanicLayer;
use utoipa::openapi::security::{ApiKey, ApiKeyValue, SecurityScheme};
use utoipa_axum::router::OpenApiRouter;

mod config;
mod logger;
mod models;
mod remote;
mod routes;
mod server;

const VERSION: &str = env!("CARGO_PKG_VERSION");
const GIT_COMMIT: &str = env!("CARGO_GIT_COMMIT");

fn handle_panic(_err: Box<dyn std::any::Any + Send + 'static>) -> Response<Body> {
    logger::log(
        logger::LoggerLevel::Error,
        "a request panic has occurred".bright_red().to_string(),
    );

    let body = serde_json::to_string(&ApiError::new("internal server error")).unwrap();

    Response::builder()
        .status(StatusCode::INTERNAL_SERVER_ERROR)
        .header("Content-Type", "application/json")
        .body(Body::from(body))
        .unwrap()
}

async fn handle_request(req: Request<Body>, next: Next) -> Result<Response<Body>, StatusCode> {
    logger::log(
        logger::LoggerLevel::Info,
        format!(
            "{} {}{}",
            format!("HTTP {}", req.method()).green().bold(),
            req.uri().path().cyan(),
            if let Some(query) = req.uri().query() {
                format!("?{}", query)
            } else {
                "".to_string()
            }
            .bright_cyan()
        ),
    );

    Ok(next.run(req).await)
}

async fn handle_cors(
    state: routes::GetState,
    req: Request,
    next: Next,
) -> Result<Response<Body>, StatusCode> {
    let method = req.method().clone();
    let mut headers = HeaderMap::new();

    headers.insert("Access-Control-Allow-Credentials", "true".parse().unwrap());
    headers.insert(
        "Access-Control-Allow-Methods",
        "GET, POST, PATCH, PUT, DELETE, OPTIONS".parse().unwrap(),
    );
    headers.insert("Access-Control-Allow-Headers", "Accept, Accept-Encoding, Authorization, Cache-Control, Content-Type, Content-Length, Origin, X-Real-IP, X-CSRF-Token".parse().unwrap());

    if state.config.allow_cors_private_network {
        headers.insert(
            "Access-Control-Request-Private-Network",
            "true".parse().unwrap(),
        );
    }

    headers.insert("Access-Control-Max-Age", "7200".parse().unwrap());

    if let Some(origin) = req.headers().get("Origin") {
        if origin.to_str().ok() != Some(state.config.remote.as_str()) {
            for o in state.config.allowed_origins.iter() {
                if o.as_str() == "*" || origin.to_str().ok() == Some(o.as_str()) {
                    headers.insert("Access-Control-Allow-Origin", o.parse().unwrap());
                    break;
                }
            }
        }
    }

    if !headers.contains_key("Access-Control-Allow-Origin") {
        headers.insert(
            "Access-Control-Allow-Origin",
            state.config.remote.parse().unwrap(),
        );
    }

    if method == Method::OPTIONS {
        let mut response = Response::new(Body::empty());
        response.headers_mut().extend(headers);
        *response.status_mut() = StatusCode::NO_CONTENT;

        return Ok(response);
    }

    let mut response = next.run(req).await;
    response.headers_mut().extend(headers);

    Ok(response)
}

#[tokio::main]
async fn main() {
    let config = config::Config::open("/etc/pterodactyl/config.yml").unwrap();
    let docker = Arc::new(Docker::connect_with_local_defaults().unwrap());
    config.ensure_network(&docker).await.unwrap();

    let server_manager = server::manager::Manager::new(
        Arc::clone(&config),
        Arc::clone(&docker),
        config.client.servers().await.unwrap(),
    )
    .await;

    let state = Arc::new(routes::AppState {
        config: Arc::clone(&config),
        start_time: Instant::now(),
        version: format!("{}:{}", VERSION, GIT_COMMIT),

        docker: Arc::clone(&docker),
        server_manager: Arc::clone(&server_manager),
    });

    let app = OpenApiRouter::new()
        .merge(routes::router(&state))
        .fallback(|| async {
            (
                StatusCode::NOT_FOUND,
                axum::Json(ApiError::new("route not found")),
            )
        })
        .layer(CatchPanicLayer::custom(handle_panic))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            handle_cors,
        ))
        .layer(axum::middleware::from_fn(handle_request))
        .layer(DefaultBodyLimit::max(config.api.upload_limit * 1000 * 1000))
        .with_state(state.clone());

    let (router, mut openapi) = app.split_for_parts();
    openapi.info.version = state.version.clone();
    openapi.info.description = None;
    openapi.info.title = "Pterodactyl Wings API".to_string();
    openapi.info.contact = None;
    openapi.info.license = None;
    openapi.components.as_mut().unwrap().add_security_scheme(
        "api_key",
        SecurityScheme::ApiKey(ApiKey::Header(ApiKeyValue::new("Authorization"))),
    );

    let router = router.route(
        "/openapi.json",
        axum::routing::get(|| async move { axum::Json(openapi) }),
    );

    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("Failed to install rustls crypto provider");

    let address = SocketAddr::from((
        state.config.api.host.parse::<std::net::IpAddr>().unwrap(),
        state.config.api.port,
    ));

    if config.api.ssl.enabled {
        logger::log(
            logger::LoggerLevel::Info,
            format!(
                "{} listening on {} {}",
                "https server".bright_red(),
                address.to_string().cyan(),
                format!(
                    "(app@{}, {}ms)",
                    VERSION,
                    state.start_time.elapsed().as_millis()
                )
                .bright_black()
            ),
        );

        let config = axum_server::tls_rustls::RustlsConfig::from_pem_file(
            config.api.ssl.cert.as_str(),
            config.api.ssl.key.as_str(),
        )
        .await
        .unwrap();

        axum_server::bind_rustls(address, config)
            .serve(router.into_make_service())
            .await
            .unwrap();
    } else {
        logger::log(
            logger::LoggerLevel::Info,
            format!(
                "{} listening on {} {}",
                "http server".bright_red(),
                address.to_string().cyan(),
                format!(
                    "(app@{}, {}ms)",
                    VERSION,
                    state.start_time.elapsed().as_millis()
                )
                .bright_black()
            ),
        );

        let listener = tokio::net::TcpListener::bind(address).await.unwrap();
        axum::serve(listener, router.into_make_service())
            .await
            .unwrap();
    }
}
