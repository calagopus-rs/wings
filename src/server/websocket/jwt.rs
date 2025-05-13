use std::sync::Arc;

use super::{WebsocketEvent, WebsocketJwtPayload, WebsocketMessage, WebsocketPermission};
use axum::extract::ws::{Message, WebSocket};
use futures_util::stream::SplitSink;
use tokio::sync::{Mutex, RwLock};

pub enum JwtError {
    CloseSocket,
    Misc(Box<dyn std::error::Error>),
}

impl From<Box<dyn std::error::Error>> for JwtError {
    fn from(err: Box<dyn std::error::Error>) -> Self {
        JwtError::Misc(err)
    }
}

impl From<serde_json::Error> for JwtError {
    fn from(err: serde_json::Error) -> Self {
        JwtError::Misc(Box::new(err))
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
            let message = serde_json::from_str::<WebsocketMessage>(&text)?;

            match message.event {
                WebsocketEvent::Authentication => {
                    match state.config.jwt.verify::<WebsocketJwtPayload>(
                        message.args.first().map_or("", |v| v.as_str()),
                    ) {
                        Ok(jwt) => {
                            if !jwt.base.validate(&state.config.jwt)
                                || !jwt.has_permission(WebsocketPermission::WebsocketConnect)
                                || jwt.server_uuid != server.uuid
                            {
                                crate::logger::log(
                                    crate::logger::LoggerLevel::Debug,
                                    format!(
                                        "JWT does not have permission to connect to websocket: {:?}",
                                        jwt.permissions
                                    ),
                                );

                                if jwt.has_permission(WebsocketPermission::WebsocketConnect) {
                                    super::send_message(
                                        sender,
                                        WebsocketMessage::new(WebsocketEvent::TokenExpired, &[]),
                                    )
                                    .await;

                                    return Err(JwtError::Misc(Box::new(std::io::Error::new(
                                        std::io::ErrorKind::PermissionDenied,
                                        "JWT expired",
                                    ))));
                                }

                                return Err(JwtError::CloseSocket);
                            }

                            let mut permissions = Vec::new();
                            for permission in &jwt.permissions {
                                permissions.push(
                                    serde_json::to_value(permission)
                                        .unwrap()
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

                            socket_jwt.write().await.replace(Arc::new(jwt));

                            Ok(None)
                        }
                        Err(err) => {
                            crate::logger::log(
                                crate::logger::LoggerLevel::Debug,
                                format!("Failed to verify JWT: {}", err),
                            );

                            Err(JwtError::CloseSocket)
                        }
                    }
                }
                _ => {
                    if let Some(jwt) = socket_jwt.read().await.as_ref() {
                        if !jwt.base.validate(&state.config.jwt)
                            || !jwt.has_permission(WebsocketPermission::WebsocketConnect)
                        {
                            crate::logger::log(
                                crate::logger::LoggerLevel::Debug,
                                format!(
                                    "JWT does not have permission to connect to websocket: {:?}",
                                    jwt.permissions
                                ),
                            );

                            return Err(JwtError::CloseSocket);
                        }

                        Ok(Some((message, Arc::clone(jwt))))
                    } else {
                        crate::logger::log(
                            crate::logger::LoggerLevel::Debug,
                            "JWT is not set".to_string(),
                        );

                        Err(JwtError::CloseSocket)
                    }
                }
            }
        }
        _ => Err(JwtError::Misc(Box::new(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "Invalid message type",
        )))),
    }
}

pub async fn listen_jwt(
    sender: &Mutex<SplitSink<WebSocket, Message>>,
    socket_jwt: &RwLock<Option<Arc<WebsocketJwtPayload>>>,
) {
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(30)).await;

        if let Some(jwt) = socket_jwt.read().await.as_ref() {
            if let Some(expiration) = jwt.base.expiration_time {
                if expiration < chrono::Utc::now().timestamp() {
                    super::send_message(
                        sender,
                        WebsocketMessage::new(WebsocketEvent::TokenExpired, &[]),
                    )
                    .await;
                } else if expiration - 60 < chrono::Utc::now().timestamp() {
                    super::send_message(
                        sender,
                        WebsocketMessage::new(WebsocketEvent::TokenExpiring, &[]),
                    )
                    .await;
                }
            }
        } else {
            crate::logger::log(
                crate::logger::LoggerLevel::Debug,
                "JWT is not set".to_string(),
            );
        }
    }
}
