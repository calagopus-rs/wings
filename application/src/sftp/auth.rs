use crate::{remote::AuthenticationType, routes::State, server::permissions::Permissions};
use russh::{
    Channel, ChannelId, MethodSet,
    server::{Auth, Msg, Session},
};
use russh_sftp::protocol::StatusCode;
use std::{collections::HashMap, net::IpAddr, sync::Arc};

pub struct SshSession {
    pub state: State,
    pub server: Option<crate::server::Server>,

    pub user_ip: Option<IpAddr>,
    pub user_uuid: Option<uuid::Uuid>,
    pub user_permissions: Permissions,

    pub clients: HashMap<ChannelId, Channel<Msg>>,
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

    pub async fn get_channel(&mut self, channel_id: ChannelId) -> Channel<Msg> {
        self.clients.remove(&channel_id).unwrap()
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

        let (user, server, permissions) = match self
            .state
            .config
            .client
            .get_sftp_auth(AuthenticationType::Password, username, password)
            .await
        {
            Ok((user, server, permissions)) => (user, server, permissions),
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
        self.user_permissions = permissions;

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

        if server.is_locked_state() {
            return Ok(Auth::reject());
        }

        self.server = Some(server);

        Ok(Auth::Accept)
    }

    async fn auth_publickey(
        &mut self,
        username: &str,
        public_key: &russh::keys::ssh_key::PublicKey,
    ) -> Result<Auth, Self::Error> {
        let (user, server, permissions) = match self
            .state
            .config
            .client
            .get_sftp_auth(
                AuthenticationType::PublicKey,
                username,
                &public_key.to_openssh().unwrap(),
            )
            .await
        {
            Ok((user, server, permissions)) => (user, server, permissions),
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
        self.user_permissions = permissions;

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

        if server.is_locked_state() {
            return Ok(Auth::reject());
        }

        self.server = Some(server);

        Ok(Auth::Accept)
    }

    async fn channel_open_session(
        &mut self,
        channel: Channel<Msg>,
        _session: &mut Session,
    ) -> Result<bool, Self::Error> {
        self.clients.insert(channel.id(), channel);

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
            Some(server) => server.clone(),
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
