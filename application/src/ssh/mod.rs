use crate::routes::State;
use std::{
    collections::{HashMap, HashSet},
    net::SocketAddr,
    sync::Arc,
};

mod auth;
mod exec;
mod ratelimiter;
mod sftp;
mod shell;

pub struct Server {
    ratelimiter: Arc<ratelimiter::SshRatelimiter>,
    state: State,
}

impl Server {
    pub fn new(state: Arc<crate::routes::AppState>) -> Self {
        Self {
            ratelimiter: Arc::new(ratelimiter::SshRatelimiter::new(
                state
                    .config
                    .system
                    .sftp
                    .limits
                    .authentication_password_attempts,
                state
                    .config
                    .system
                    .sftp
                    .limits
                    .authentication_pubkey_attempts,
                state.config.system.sftp.limits.authentication_cooldown,
            )),
            state,
        }
    }
}

impl russh::server::Server for Server {
    type Handler = auth::SshSession;

    fn new_client(&mut self, client: Option<SocketAddr>) -> Self::Handler {
        auth::SshSession {
            ratelimiter: Arc::clone(&self.ratelimiter),
            state: Arc::clone(&self.state),
            server: None,

            user_ip: client.map_or_else(
                || std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
                |s| s.ip(),
            ),
            user_uuid: None,

            clients: HashMap::new(),
            shell_clients: HashSet::new(),
        }
    }
}
