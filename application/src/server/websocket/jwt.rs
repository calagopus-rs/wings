use super::{WebsocketEvent, WebsocketJwtPayload, WebsocketMessage};
use crate::server::permissions::Permission;
use axum::extract::ws::{Message, WebSocket};
use futures_util::stream::SplitSink;
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock};

pub enum JwtError {
    CloseSocket,
    Misc(anyhow::Error),
}

impl From<anyhow::Error> for JwtError {
    fn from(err: anyhow::Error) -> Self {
        JwtError::Misc(err)
    }
}

impl From<serde_json::Error> for JwtError {
    fn from(err: serde_json::Error) -> Self {
        JwtError::Misc(err.into())
    }
}

pub async fn handle_jwt(
    state: &crate::routes::AppState,
    server: &crate::server::Server,
    sender: &Mutex<SplitSink<WebSocket, Message>>,
    socket_jwt: &RwLock<Option<Arc<WebsocketJwtPayload>>>,
    message: Message,
) -> Result<Option<(WebsocketMessage, Arc<WebsocketJwtPayload>)>, JwtError> {
    match message {
        Message::Text(text) => {
            let message: WebsocketMessage = serde_json::from_str(&text)?;

            match message.event {
                WebsocketEvent::Authentication => {
                    match state.config.jwt.verify::<WebsocketJwtPayload>(
                        message.args.first().map_or("", |v| v.as_str()),
                    ) {
                        Ok(jwt) => {
                            if !jwt.base.validate(&state.config.jwt).await
                                || !jwt.permissions.has_permission(Permission::WebsocketConnect)
                                || jwt.server_uuid != server.uuid
                            {
                                tracing::debug!(
                                    server = %server.uuid,
                                    "jwt does not have permission to connect to websocket: {:?}",
                                    jwt.permissions
                                );

                                if jwt.permissions.has_permission(Permission::WebsocketConnect) {
                                    super::send_message(
                                        sender,
                                        WebsocketMessage::new(WebsocketEvent::TokenExpired, &[]),
                                    )
                                    .await;

                                    return Err(JwtError::Misc(anyhow::anyhow!("JWT expired")));
                                }

                                return Err(JwtError::CloseSocket);
                            }

                            let mut permissions = Vec::new();
                            for permission in jwt.permissions.iter() {
                                permissions.push(
                                    serde_json::to_value(permission)?
                                        .as_str()
                                        .unwrap()
                                        .to_string(),
                                );
                            }

                            super::send_message(
                                sender,
                                WebsocketMessage::new(
                                    WebsocketEvent::AuthenticationSuccess,
                                    &permissions,
                                ),
                            )
                            .await;

                            if socket_jwt.write().await.replace(Arc::new(jwt)).is_none() {
                                super::send_message(
                                    sender,
                                    WebsocketMessage::new(
                                        WebsocketEvent::ServerStatus,
                                        &[server.state.get_state().to_str().to_string()],
                                    ),
                                )
                                .await;
                            }

                            Ok(None)
                        }
                        Err(err) => {
                            tracing::debug!(
                                server = %server.uuid,
                                "failed to verify jwt when connecting to websocket: {}",
                                err
                            );

                            Err(JwtError::CloseSocket)
                        }
                    }
                }
                _ => {
                    if let Some(jwt) = socket_jwt.read().await.as_ref() {
                        if !jwt.base.validate(&state.config.jwt).await
                            || !jwt.permissions.has_permission(Permission::WebsocketConnect)
                        {
                            tracing::debug!(
                                server = %server.uuid,
                                "jwt does not have permission to connect to websocket: {:?}",
                                jwt.permissions
                            );

                            return Err(JwtError::CloseSocket);
                        }

                        Ok(Some((message, Arc::clone(jwt))))
                    } else {
                        tracing::debug!(
                            server = %server.uuid,
                            "jwt is not set when connecting to websocket",
                        );

                        Err(JwtError::CloseSocket)
                    }
                }
            }
        }
        _ => Err(JwtError::Misc(anyhow::anyhow!("invalid message type"))),
    }
}

pub async fn listen_jwt(
    sender: &Mutex<SplitSink<WebSocket, Message>>,
    socket_jwt: &RwLock<Option<Arc<WebsocketJwtPayload>>>,
) {
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(30)).await;

        let socket_jwt_guard = socket_jwt.read().await;
        if let Some(jwt) = socket_jwt_guard.as_ref() {
            if let Some(expiration) = jwt.base.expiration_time {
                if expiration < chrono::Utc::now().timestamp() {
                    super::send_message(
                        sender,
                        WebsocketMessage::new(WebsocketEvent::TokenExpired, &[]),
                    )
                    .await;

                    drop(socket_jwt_guard);
                    socket_jwt.write().await.take();

                    tracing::debug!("jwt expired for websocket connection, removing jwt");
                } else if expiration - 60 < chrono::Utc::now().timestamp() {
                    super::send_message(
                        sender,
                        WebsocketMessage::new(WebsocketEvent::TokenExpiring, &[]),
                    )
                    .await;

                    tracing::debug!(
                        "jwt is expiring soon for websocket connection, notifying client"
                    );
                }
            }
        } else {
            tracing::debug!("jwt is not set when connecting to websocket");
        }
    }
}
