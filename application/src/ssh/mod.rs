use crate::routes::State;
use std::{
    collections::{HashMap, HashSet},
    net::SocketAddr,
    sync::Arc,
};

mod auth;
mod exec;
mod sftp;
mod shell;

pub struct Server {
    pub state: State,
}

impl russh::server::Server for Server {
    type Handler = auth::SshSession;

    fn new_client(&mut self, client: Option<SocketAddr>) -> Self::Handler {
        auth::SshSession {
            state: Arc::clone(&self.state),
            server: None,

            user_ip: client.map(|addr| addr.ip()),
            user_uuid: None,

            clients: HashMap::new(),
            shell_clients: HashSet::new(),
        }
    }
}
