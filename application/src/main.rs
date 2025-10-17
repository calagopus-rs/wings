use anyhow::Context;
use axum::{
    body::Body,
    extract::Request,
    http::{HeaderMap, Method, Response, StatusCode},
    middleware::Next,
};
use clap::{Arg, Command};
use colored::Colorize;
use russh::{keys::ssh_key::rand_core::OsRng, server::Server};
use std::{net::SocketAddr, path::Path, sync::Arc, time::Instant};
use utoipa::openapi::security::{ApiKey, ApiKeyValue, SecurityScheme};
use utoipa_axum::router::OpenApiRouter;
use wings_rs::routes::ApiError;

#[cfg(target_os = "linux")]
#[global_allocator]
static ALLOC: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

fn cli() -> Command {
    Command::new("wings-rs")
        .about(
            "The API server allowing programmatic control of game servers for Calagopus/Pterodactyl Panel.",
        )
        .allow_external_subcommands(true)
        .arg(
            Arg::new("config")
                .help("set the location for the configuration file")
                .num_args(1)
                .short('c')
                .long("config")
                .alias("config-file")
                .alias("config-path")
                .default_value("/etc/pterodactyl/config.yml")
                .global(true)
                .required(false),
        )
        .arg(
            Arg::new("debug")
                .help("pass in order to run wings in debug mode")
                .num_args(0)
                .short('d')
                .long("debug")
                .default_value("false")
                .value_parser(clap::value_parser!(bool))
                .global(true)
                .required(false),
        )
        .arg(
            Arg::new("ignore_certificate_errors")
                .help("ignore certificate verification errors when executing API calls")
                .num_args(0)
                .long("ignore-certificate-errors")
                .default_value("false")
                .value_parser(clap::value_parser!(bool))
                .required(false),
        )
        .subcommand(
            Command::new("version")
                .about("Prints the current executable version and exits.")
                .arg_required_else_help(false),
        )
        .subcommand(
            Command::new("service-install")
                .about("Installs the Wings service on the system.")
                .arg(
                    Arg::new("override")
                        .help("set to true to override an existing service file")
                        .num_args(0)
                        .long("override")
                        .default_value("false")
                        .value_parser(clap::value_parser!(bool))
                        .required(false),
                )
                .arg_required_else_help(false),
        )
        .subcommand(
            Command::new("configure")
                .about("Use a token to configure wings automatically.")
                .arg(
                    Arg::new("allow_insecure")
                        .help("set to true to disable certificate checking")
                        .num_args(0)
                        .long("allow-insecure")
                        .default_value("false")
                        .value_parser(clap::value_parser!(bool))
                        .required(false),
                )
                .arg(
                    Arg::new("override")
                        .help("set to true to override an existing configuration for this node")
                        .num_args(0)
                        .long("override")
                        .default_value("false")
                        .value_parser(clap::value_parser!(bool))
                        .required(false),
                )
                .arg(
                    Arg::new("node")
                        .help("the ID of the node which will be connected to this daemon")
                        .num_args(1)
                        .short('n')
                        .long("node")
                        .value_parser(clap::value_parser!(usize))
                        .required(false),
                )
                .arg(
                    Arg::new("panel_url")
                        .help("the base URL for this daemon's panel")
                        .num_args(1)
                        .short('p')
                        .long("panel-url")
                        .required(false),
                )
                .arg(
                    Arg::new("join_data")
                        .help("the join data from this daemon's panel")
                        .num_args(1)
                        .short('j')
                        .long("join-data")
                        .required(false),
                )
                .arg(
                    Arg::new("token")
                        .help("the API key to use for fetching node information")
                        .num_args(1)
                        .short('t')
                        .long("token")
                        .required(false),
                )
                .arg_required_else_help(false),
        )
        .subcommand(
            Command::new("diagnostics")
                .about("Collect and report information about this Wings instance to assist in debugging.")
                .arg(
                    Arg::new("log_lines")
                        .help("the number of log lines to include in the report")
                        .num_args(1)
                        .short('l')
                        .long("log-lines")
                        .default_value("500")
                        .value_parser(clap::value_parser!(usize))
                        .required(false),
                )
                .arg_required_else_help(false),
        )
}

async fn handle_request(req: Request<Body>, next: Next) -> Result<Response<Body>, StatusCode> {
    tracing::info!(
        "http {} {}{}",
        req.method().to_string().to_lowercase(),
        req.uri().path().cyan(),
        if let Some(query) = req.uri().query() {
            format!("?{query}")
        } else {
            "".to_string()
        }
        .bright_cyan()
    );

    Ok(next.run(req).await)
}

async fn handle_cors(
    state: wings_rs::routes::GetState,
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

    if let Some(origin) = req.headers().get("Origin")
        && origin.to_str().ok() != Some(state.config.remote.as_str())
    {
        for o in state.config.allowed_origins.iter() {
            if o == "*" || origin.to_str().ok() == Some(o.as_str()) {
                headers.insert("Access-Control-Allow-Origin", o.parse().unwrap());
                break;
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
    let matches = cli().get_matches();

    let config_path = matches.get_one::<String>("config").unwrap();
    let debug = *matches.get_one::<bool>("debug").unwrap();
    let ignore_certificate_errors = matches
        .get_one::<bool>("ignore_certificate_errors")
        .copied()
        .unwrap_or(false);
    let config = wings_rs::config::Config::open(
        config_path,
        debug,
        matches.subcommand().is_some(),
        ignore_certificate_errors,
    );

    match matches.subcommand() {
        Some(("version", sub_matches)) => std::process::exit(
            wings_rs::commands::version::version(sub_matches, config.as_ref().ok().map(|c| &c.0))
                .await,
        ),
        Some(("service-install", sub_matches)) => std::process::exit(
            wings_rs::commands::service_install::service_install(
                sub_matches,
                config.as_ref().ok().map(|c| &c.0),
            )
            .await,
        ),
        Some(("configure", sub_matches)) => std::process::exit(
            wings_rs::commands::configure::configure(
                sub_matches,
                config.as_ref().ok().map(|c| &c.0),
            )
            .await,
        ),
        Some(("diagnostics", sub_matches)) => std::process::exit(
            wings_rs::commands::diagnostics::diagnostics(
                sub_matches,
                config.as_ref().ok().map(|c| &c.0),
            )
            .await,
        ),
        None => {
            tracing::info!(" __      ___ _ __   __ _ ___        ");
            tracing::info!(" \\ \\ /\\ / / | '_ \\ / _` / __|       ");
            tracing::info!("  \\ V  V /| | | | | (_| \\__ \\       ");
            tracing::info!("   \\_/\\_/ |_|_| |_|\\__, |___/__ ___ ");
            tracing::info!("                    __/ | | '__/ __|");
            tracing::info!("                   |___/  | |  \\__ \\");
            tracing::info!(
                "{: >25} |_|  |___/",
                format!("{} (git-{})", wings_rs::VERSION, wings_rs::GIT_COMMIT)
            );
            tracing::info!("github.com/calagopus-rs/wings\n");
        }
        _ => {
            cli().print_help().unwrap();
            std::process::exit(0);
        }
    }

    let (config, _guard) = match config {
        Ok(config) => config,
        Err(err) => {
            eprintln!("failed to load configuration: {err:#?}");
            std::process::exit(1);
        }
    };
    tracing::info!("config loaded from {}", config_path);

    tracing::info!("connecting to docker");
    let docker =
        Arc::new(
            if config.docker.socket.starts_with("http://")
                || config.docker.socket.starts_with("tcp://")
            {
                bollard::Docker::connect_with_http(
                    &config.docker.socket,
                    120,
                    bollard::API_DEFAULT_VERSION,
                )
            } else {
                bollard::Docker::connect_with_unix(
                    &config.docker.socket,
                    120,
                    bollard::API_DEFAULT_VERSION,
                )
            }
            .context("failed to connect to docker")
            .unwrap(),
        );

    tracing::info!("ensuring docker network exists");
    config
        .ensure_network(&docker)
        .await
        .context("failed to ensure docker network")
        .unwrap();

    match config.client.reset_state().await {
        Ok(_) => tracing::info!("remote state reset successfully"),
        Err(err) => {
            tracing::error!("failed to reset remote state: {:#?}", err);
            std::process::exit(1);
        }
    }

    tracing::info!("creating server manager");
    let servers = config
        .client
        .servers()
        .await
        .context("failed to fetch servers from remote")
        .unwrap();

    let state = Arc::new(wings_rs::routes::AppState {
        config: Arc::clone(&config),
        start_time: Instant::now(),
        version: format!("{}:{}", wings_rs::VERSION, wings_rs::GIT_COMMIT),

        docker: Arc::clone(&docker),
        server_manager: Arc::new(wings_rs::server::manager::Manager::new(&servers)),
        backup_manager: Arc::new(wings_rs::server::backup::manager::BackupManager::new(
            Arc::clone(&config),
        )),
    });

    state.server_manager.boot(&state, servers).await;

    let app = OpenApiRouter::new()
        .merge(wings_rs::routes::router(&state))
        .fallback(|| async {
            (
                StatusCode::NOT_FOUND,
                axum::Json(ApiError::new("route not found")),
            )
        })
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            handle_cors,
        ))
        .layer(axum::middleware::from_fn(handle_request))
        .with_state(state.clone());

    let (mut router, mut openapi) = app.split_for_parts();
    openapi.info.version = state.version.clone();
    openapi.info.description = None;
    openapi.info.title = format!("{} Wings API", config.app_name);
    openapi.info.contact = None;
    openapi.info.license = None;
    openapi.components.as_mut().unwrap().add_security_scheme(
        "api_key",
        SecurityScheme::ApiKey(ApiKey::Header(ApiKeyValue::new("Authorization"))),
    );

    if !config.api.disable_openapi_docs {
        router = router.route(
            "/openapi.json",
            axum::routing::get(|| async move { axum::Json(openapi) }),
        );
    }

    tracing::info!("starting http/sftp server");

    rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .expect("Failed to install rustls crypto provider");

    tokio::spawn({
        let state = Arc::clone(&state);

        async move {
            let mut server = wings_rs::ssh::Server {
                state: Arc::clone(&state),
            };

            let key_file = Path::new(&state.config.system.data_directory)
                .join(".sftp")
                .join(format!(
                    "id_{}",
                    state.config.system.sftp.key_algorithm.replace("-", "_")
                ));
            let key = match tokio::fs::read(&key_file)
                .await
                .map(russh::keys::PrivateKey::from_openssh)
            {
                Ok(Ok(key)) => {
                    tracing::info!(
                        algorithm = %key.algorithm().to_string(),
                        "loaded existing sftp host key"
                    );

                    key
                }
                _ => {
                    tracing::info!(
                        algorithm = %state.config.system.sftp.key_algorithm,
                        "generating new sftp host key"
                    );

                    let key = russh::keys::PrivateKey::random(
                        &mut OsRng,
                        state
                            .config
                            .system
                            .sftp
                            .key_algorithm
                            .parse()
                            .context("failed to parse sftp key algorithm")
                            .unwrap(),
                    )
                    .unwrap();

                    tokio::fs::create_dir_all(key_file.parent().unwrap())
                        .await
                        .context("failed to create sftp host key directory")
                        .unwrap();
                    tokio::fs::write(
                        key_file,
                        key.to_openssh(russh::keys::ssh_key::LineEnding::LF)
                            .unwrap(),
                    )
                    .await
                    .context("failed to write sftp host key")
                    .unwrap();

                    key
                }
            };

            let config = russh::server::Config {
                auth_rejection_time: std::time::Duration::from_secs(0),
                auth_rejection_time_initial: Some(std::time::Duration::from_secs(0)),
                maximum_packet_size: 32 * 1024,
                keepalive_interval: Some(std::time::Duration::from_secs(60)),
                max_auth_attempts: 6,
                channel_buffer_size: 1024,
                event_buffer_size: 1024,
                keys: vec![key],
                ..Default::default()
            };

            let address = SocketAddr::from((
                state.config.system.sftp.bind_address,
                state.config.system.sftp.bind_port,
            ));

            tracing::info!(
                "{} listening on {} {}",
                "ssh server".yellow(),
                address.to_string().cyan(),
                format!(
                    "(app@{}, {}ms)",
                    wings_rs::VERSION,
                    state.start_time.elapsed().as_millis()
                )
                .bright_black()
            );

            match server.run_on_address(Arc::new(config), address).await {
                Ok(_) => {}
                Err(err) => {
                    if err.kind() == std::io::ErrorKind::AddrInUse {
                        tracing::error!("failed to start ssh server (address already in use)");
                    } else {
                        tracing::error!("failed to start ssh server: {:#?}", err);
                    }

                    std::process::exit(1);
                }
            }
        }
    });

    tokio::spawn(async move {
        let mut signal =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup()).unwrap();

        loop {
            signal.recv().await;
            tracing::info!("received SIGHUP, ignoring");
        }
    });

    let address = SocketAddr::from((state.config.api.host, state.config.api.port));

    if config.api.ssl.enabled {
        tracing::info!("loading ssl certs");

        let config = axum_server::tls_rustls::RustlsConfig::from_pem_file(
            config.api.ssl.cert.as_str(),
            config.api.ssl.key.as_str(),
        )
        .await
        .context("failed to load SSL certificate and key")
        .unwrap();

        tracing::info!(
            "{} listening on {}",
            "https server".bright_red(),
            address.to_string().cyan(),
        );

        match axum_server::bind_rustls(address, config)
            .serve(router.into_make_service_with_connect_info::<SocketAddr>())
            .await
        {
            Ok(_) => {}
            Err(err) => {
                if err.kind() == std::io::ErrorKind::AddrInUse {
                    tracing::error!("failed to start https server (address already in use)");
                } else {
                    tracing::error!("failed to start https server: {:#?}", err,);
                }

                std::process::exit(1);
            }
        }
    } else {
        tracing::info!(
            "{} listening on {}",
            "http server".bright_red(),
            address.to_string().cyan(),
        );

        match axum::serve(
            match tokio::net::TcpListener::bind(address).await {
                Ok(listener) => listener,
                Err(err) => {
                    if err.kind() == std::io::ErrorKind::AddrInUse {
                        tracing::error!("failed to start http server (address already in use)");
                        std::process::exit(1);
                    } else {
                        tracing::error!("failed to start http server: {:#?}", err);
                        std::process::exit(1);
                    }
                }
            },
            router.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        {
            Ok(_) => {}
            Err(err) => {
                if err.kind() == std::io::ErrorKind::AddrInUse {
                    tracing::error!("failed to start http server (address already in use)");
                } else {
                    tracing::error!("failed to start http server: {:#?}", err);
                }

                std::process::exit(1);
            }
        }
    }
}
