use crate::{remote::AuthenticationType, routes::State};
use russh::{
    Channel, ChannelId, MethodSet,
    server::{Auth, Msg, Session},
};
use russh_sftp::protocol::StatusCode;
use std::{
    collections::{HashMap, HashSet},
    net::IpAddr,
    sync::Arc,
};

fn validate_username(username: &str) -> bool {
    let splits = username.split('.').collect::<Vec<_>>();
    if splits.len() < 2 {
        return false;
    }

    let server = match splits.last() {
        Some(server) => server,
        None => return false,
    };
    if server.len() != 8 || !server.chars().all(|c| c.is_ascii_hexdigit()) {
        return false;
    }

    true
}

pub struct SshSession {
    pub state: State,
    pub server: Option<crate::server::Server>,

    pub user_ip: Option<IpAddr>,
    pub user_uuid: Option<uuid::Uuid>,

    pub clients: HashMap<ChannelId, Channel<Msg>>,
    pub shell_clients: HashSet<ChannelId>,
}

impl SshSession {
    fn get_auth_methods(&self) -> MethodSet {
        let mut methods = MethodSet::empty();
        if !self.state.config.system.sftp.disable_password_auth {
            methods.push(russh::MethodKind::Password);
        }
        methods.push(russh::MethodKind::PublicKey);

        methods
    }

    pub async fn get_channel(&mut self, channel_id: ChannelId) -> Option<Channel<Msg>> {
        self.clients.remove(&channel_id)
    }
}

impl russh::server::Handler for SshSession {
    type Error = Box<dyn std::error::Error + Send + Sync>;

    async fn auth_none(&mut self, _user: &str) -> Result<Auth, Self::Error> {
        Ok(Auth::Reject {
            proceed_with_methods: Some(self.get_auth_methods()),
            partial_success: false,
        })
    }

    async fn auth_password(&mut self, username: &str, password: &str) -> Result<Auth, Self::Error> {
        if self.state.config.system.sftp.disable_password_auth {
            return Ok(Auth::Reject {
                proceed_with_methods: Some(self.get_auth_methods()),
                partial_success: false,
            });
        }

        if !validate_username(username) {
            return Ok(Auth::Reject {
                proceed_with_methods: Some(self.get_auth_methods()),
                partial_success: false,
            });
        }

        let (user, server, permissions, ignored_files) = match self
            .state
            .config
            .client
            .get_sftp_auth(AuthenticationType::Password, username, password)
            .await
        {
            Ok(data) => data,
            Err(err) => {
                tracing::debug!(
                    username = username,
                    "failed to authenticate (password): {:#?}",
                    err
                );

                return Ok(Auth::reject());
            }
        };

        self.user_uuid = Some(user);

        let server = match self
            .state
            .server_manager
            .get_servers()
            .await
            .iter()
            .find(|s| s.uuid == server)
            .cloned()
        {
            Some(server) => server,
            None => {
                return Ok(Auth::Reject {
                    proceed_with_methods: Some(self.get_auth_methods()),
                    partial_success: false,
                });
            }
        };

        tracing::debug!("user {} authenticated with password", username);

        if server.is_locked_state() {
            return Ok(Auth::reject());
        }

        server
            .user_permissions
            .set_permissions(user, permissions, &ignored_files)
            .await;

        self.server = Some(server);

        Ok(Auth::Accept)
    }

    async fn auth_publickey(
        &mut self,
        username: &str,
        public_key: &russh::keys::ssh_key::PublicKey,
    ) -> Result<Auth, Self::Error> {
        if !validate_username(username) {
            return Ok(Auth::Reject {
                proceed_with_methods: Some(self.get_auth_methods()),
                partial_success: false,
            });
        }

        let (user, server, permissions, ignored_files) = match self
            .state
            .config
            .client
            .get_sftp_auth(
                AuthenticationType::PublicKey,
                username,
                &public_key.to_openssh()?,
            )
            .await
        {
            Ok(data) => data,
            Err(err) => {
                tracing::debug!(
                    username = username,
                    "failed to authenticate (public_key): {:#?}",
                    err
                );

                return Ok(Auth::Reject {
                    proceed_with_methods: Some(self.get_auth_methods()),
                    partial_success: false,
                });
            }
        };

        self.user_uuid = Some(user);

        let server = match self
            .state
            .server_manager
            .get_servers()
            .await
            .iter()
            .find(|s| s.uuid == server)
            .cloned()
        {
            Some(server) => server,
            None => return Ok(Auth::reject()),
        };

        tracing::debug!("user {} authenticated with public key", username);

        if server.is_locked_state() {
            return Ok(Auth::reject());
        }

        server
            .user_permissions
            .set_permissions(user, permissions, &ignored_files)
            .await;

        self.server = Some(server);

        Ok(Auth::Accept)
    }

    async fn channel_open_session(
        &mut self,
        channel: Channel<Msg>,
        _session: &mut Session,
    ) -> Result<bool, Self::Error> {
        tracing::debug!("opening new channel: {}", channel.id());
        self.clients.insert(channel.id(), channel);

        Ok(true)
    }

    async fn channel_eof(
        &mut self,
        channel: ChannelId,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        tracing::debug!("channel eof: {}", channel);
        session.close(channel)?;

        self.clients.remove(&channel);
        self.shell_clients.retain(|&id| id != channel);

        Ok(())
    }

    async fn shell_request(
        &mut self,
        channel_id: ChannelId,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        tracing::debug!("channel shell request: {}", channel_id);

        if !self.state.config.system.sftp.shell.enabled {
            return Err(Box::new(StatusCode::PermissionDenied));
        }

        let user_uuid = match self.user_uuid {
            Some(uuid) => uuid,
            None => return Err(Box::new(StatusCode::PermissionDenied)),
        };

        let server = match &self.server {
            Some(server) => server.clone(),
            None => return Err(Box::new(StatusCode::PermissionDenied)),
        };

        let channel = match self.get_channel(channel_id).await {
            Some(channel) => channel,
            None => return Err(Box::new(StatusCode::PermissionDenied)),
        };

        self.shell_clients.insert(channel_id);

        session.channel_success(channel_id)?;
        let ssh = super::shell::ShellSession {
            state: Arc::clone(&self.state),
            server,

            user_ip: self.user_ip,
            user_uuid,
            mode: super::shell::ShellMode::Normal,
        };
        ssh.run(channel);

        Ok(())
    }

    async fn exec_request(
        &mut self,
        channel_id: ChannelId,
        data: &[u8],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        let command = String::from_utf8_lossy(data);

        let user_uuid = match self.user_uuid {
            Some(uuid) => uuid,
            None => return Err(Box::new(StatusCode::PermissionDenied)),
        };

        let server = match &self.server {
            Some(server) => server.clone(),
            None => return Err(Box::new(StatusCode::PermissionDenied)),
        };

        let channel = match self.get_channel(channel_id).await {
            Some(channel) => channel,
            None => return Err(Box::new(StatusCode::PermissionDenied)),
        };

        tracing::debug!("recieved command from exec: {}", command);

        session.channel_success(channel_id)?;
        let exec = super::exec::ExecSession {
            state: Arc::clone(&self.state),
            server,

            user_ip: self.user_ip,
            user_uuid,
        };
        exec.run(command.to_string(), channel);

        Ok(())
    }

    async fn data(
        &mut self,
        channel_id: ChannelId,
        data: &[u8],
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        if data == [3] && self.shell_clients.contains(&channel_id) {
            return Err(Box::new(russh::Error::Disconnect));
        }

        Ok(())
    }

    async fn subsystem_request(
        &mut self,
        channel_id: ChannelId,
        name: &str,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        let user_uuid = match self.user_uuid {
            Some(uuid) => uuid,
            None => return Err(Box::new(StatusCode::PermissionDenied)),
        };

        let server = match &self.server {
            Some(server) => server.clone(),
            None => return Err(Box::new(StatusCode::PermissionDenied)),
        };

        if name == "sftp" {
            let channel = match self.get_channel(channel_id).await {
                Some(channel) => channel,
                None => return Err(Box::new(StatusCode::PermissionDenied)),
            };
            let sftp = super::sftp::SftpSession {
                state: Arc::clone(&self.state),
                server,

                user_ip: self.user_ip,
                user_uuid,

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
