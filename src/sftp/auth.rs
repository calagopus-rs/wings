use crate::{remote::AuthenticationType, routes::State, server::permissions::Permissions};
use russh::{
    Channel, ChannelId,
    server::{Auth, Msg, Session},
};
use russh_sftp::protocol::StatusCode;
use std::{collections::HashMap, net::IpAddr, sync::Arc};
use tokio::sync::Mutex;

pub struct SshSession {
    pub state: State,
    pub server: Option<Arc<crate::server::Server>>,

    pub user_ip: Option<IpAddr>,
    pub user_uuid: Option<uuid::Uuid>,
    pub user_permissions: Permissions,

    pub clients: Arc<Mutex<HashMap<ChannelId, Channel<Msg>>>>,
}

impl SshSession {
    pub async fn get_channel(&mut self, channel_id: ChannelId) -> Channel<Msg> {
        let mut clients = self.clients.lock().await;

        clients.remove(&channel_id).unwrap()
    }
}

impl russh::server::Handler for SshSession {
    type Error = Box<dyn std::error::Error + Send + Sync>;

    async fn auth_password(&mut self, username: &str, password: &str) -> Result<Auth, Self::Error> {
        let (user, server, permissions) = match self
            .state
            .config
            .client
            .get_sftp_auth(AuthenticationType::Password, username, password)
            .await
        {
            Ok((user, server, permissions)) => (user, server, permissions),
            Err(err) => {
                crate::logger::log(
                    crate::logger::LoggerLevel::Debug,
                    format!(
                        "Failed to authenticate user {} (password): {}",
                        username, err
                    ),
                );

                return Ok(Auth::reject());
            }
        };

        self.user_uuid = Some(user);
        self.user_permissions = permissions;

        let server = match self
            .state
            .server_manager
            .get_servers()
            .await
            .iter()
            .find(|s| s.uuid == server)
        {
            Some(server) => Arc::clone(server),
            None => return Ok(Auth::reject()),
        };

        self.server = Some(server);

        Ok(Auth::Accept)
    }

    async fn auth_publickey(
        &mut self,
        user: &str,
        public_key: &russh::keys::ssh_key::PublicKey,
    ) -> Result<Auth, Self::Error> {
        let (user, server, permissions) = match self
            .state
            .config
            .client
            .get_sftp_auth(
                AuthenticationType::PublicKey,
                user,
                &public_key.to_openssh().unwrap(),
            )
            .await
        {
            Ok((user, server, permissions)) => (user, server, permissions),
            Err(err) => {
                crate::logger::log(
                    crate::logger::LoggerLevel::Debug,
                    format!("Failed to authenticate user {} (public_key): {}", user, err),
                );

                return Ok(Auth::reject());
            }
        };

        self.user_uuid = Some(user);
        self.user_permissions = permissions;

        let server = match self
            .state
            .server_manager
            .get_servers()
            .await
            .iter()
            .find(|s| s.uuid == server)
        {
            Some(server) => Arc::clone(server),
            None => return Ok(Auth::reject()),
        };

        self.server = Some(server);

        Ok(Auth::Accept)
    }

    async fn channel_open_session(
        &mut self,
        channel: Channel<Msg>,
        _session: &mut Session,
    ) -> Result<bool, Self::Error> {
        {
            let mut clients = self.clients.lock().await;
            clients.insert(channel.id(), channel);
        }

        Ok(true)
    }

    async fn channel_eof(
        &mut self,
        channel: ChannelId,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        session.close(channel)?;

        Ok(())
    }

    async fn subsystem_request(
        &mut self,
        channel_id: ChannelId,
        name: &str,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        let server = match &self.server {
            Some(server) => Arc::clone(server),
            None => return Err(Box::new(StatusCode::PermissionDenied)),
        };

        if name == "sftp" {
            let channel = self.get_channel(channel_id).await;
            let sftp = super::SftpSession {
                state: Arc::clone(&self.state),
                server,

                user_ip: self.user_ip,
                user_uuid: self.user_uuid,
                user_permissions: self.user_permissions.clone(),

                handle_id: 0,
                handles: HashMap::new(),
            };

            session.channel_success(channel_id)?;
            russh_sftp::server::run(channel.into_stream(), sftp).await;
        } else {
            session.channel_failure(channel_id)?;
        }

        Ok(())
    }
}
