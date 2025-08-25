use crate::{
    response::ApiResponse,
    routes::GetState,
    server::{
        permissions::Permission,
        websocket::{self, send_message},
    },
};
use axum::{
    body::Bytes,
    extract::{ConnectInfo, Path, WebSocketUpgrade, ws::Message},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use futures_util::{SinkExt, StreamExt};
use std::{net::SocketAddr, pin::Pin, sync::Arc};
use tokio::sync::{Mutex, RwLock, broadcast::error::RecvError};

pub async fn handle_ws(
    ws: WebSocketUpgrade,
    headers: HeaderMap,
    connect_info: ConnectInfo<SocketAddr>,
    state: GetState,
    Path(server): Path<uuid::Uuid>,
) -> Response {
    let server = state
        .server_manager
        .get_servers()
        .await
        .iter()
        .find(|s| s.uuid == server)
        .cloned();

    let server = match server {
        Some(server) => server,
        None => {
            return ApiResponse::error("server not found")
                .with_status(StatusCode::NOT_FOUND)
                .into_response();
        }
    };

    let user_ip = state.config.find_ip(&headers, connect_info);

    ws.on_upgrade(move |socket| async move {
        let (sender, mut reciever) = socket.split();
        let sender = Arc::new(Mutex::new(sender));
        let socket_jwt = Arc::new(RwLock::new(None));

        let writer = {
            let state = Arc::clone(&state);
            let socket_jwt = Arc::clone(&socket_jwt);
            let sender = Arc::clone(&sender);
            let server = server.clone();

            async move {
                loop {
                    let ws_data = match reciever.next().await {
                        Some(Ok(data)) => data,
                        Some(Err(err)) => {
                            tracing::debug!(
                                server = %server.uuid,
                                "error receiving websocket message: {}",
                                err
                            );
                            break;
                        }
                        None => break,
                    };

                    if let Message::Close(_) = ws_data {
                        tracing::debug!(
                            server = %server.uuid,
                            "websocket closed",
                        );
                        break;
                    }

                    if matches!(ws_data, Message::Ping(_) | Message::Pong(_)) {
                        continue;
                    }

                    match super::jwt::handle_jwt(&state, &server, &sender, &socket_jwt, ws_data)
                        .await
                    {
                        Ok(Some((message, jwt))) => {
                            match super::message_handler::handle_message(
                                &state, user_ip, &server, &sender, &jwt, message,
                            )
                            .await
                            {
                                Ok(_) => {}
                                Err(err) => {
                                    tracing::error!(
                                        server = %server.uuid,
                                        "error handling websocket message: {}",
                                        err
                                    );
                                }
                            }
                        }
                        Ok(None) => {}
                        Err(websocket::jwt::JwtError::CloseSocket) => {
                            tracing::debug!(
                                server = %server.uuid,
                                "closing websocket due to jwt error",
                            );
                            break;
                        }
                        Err(websocket::jwt::JwtError::Misc(err)) => {
                            tracing::error!(
                                server = %server.uuid,
                                "error handling jwt: {}",
                                err,
                            );

                            send_message(
                                &sender,
                                websocket::WebsocketMessage::new(
                                    websocket::WebsocketEvent::JwtError,
                                    &[err.to_string()],
                                ),
                            )
                            .await;
                        }
                    }
                }
            }
        };

        let mut futures: Vec<Pin<Box<dyn futures_util::Future<Output = ()> + Send>>> =
            Vec::with_capacity(4);

        // Server Listener
        futures.push({
            let socket_jwt = Arc::clone(&socket_jwt);
            let sender = Arc::clone(&sender);
            let mut reciever = server.websocket.subscribe();
            let server = server.clone();

            Box::pin(async move {
                loop {
                    match reciever.recv().await {
                        Ok(message) => {
                            let socket_jwt = socket_jwt.read().await;
                            let socket_jwt = match socket_jwt.as_ref() {
                                Some(jwt) => jwt,
                                None => {
                                    tracing::debug!(
                                        server = %server.uuid,
                                        "no socket jwt found, ignoring websocket message",
                                    );
                                    continue;
                                }
                            };

                            match message.event {
                                websocket::WebsocketEvent::ServerInstallOutput => {
                                    if !socket_jwt
                                        .permissions
                                        .has_permission(Permission::AdminWebsocketInstall)
                                    {
                                        continue;
                                    }
                                }
                                websocket::WebsocketEvent::ServerOperationProgress
                                | websocket::WebsocketEvent::ServerOperationCompleted => {
                                    if !socket_jwt.permissions.has_permission(Permission::FileRead)
                                    {
                                        continue;
                                    }
                                }
                                websocket::WebsocketEvent::ServerBackupProgress
                                | websocket::WebsocketEvent::ServerBackupCompleted => {
                                    if !socket_jwt
                                        .permissions
                                        .has_permission(Permission::BackupRead)
                                    {
                                        continue;
                                    }
                                }
                                websocket::WebsocketEvent::ServerScheduleStatus
                                | websocket::WebsocketEvent::ServerScheduleStepError => {
                                    if !socket_jwt
                                        .permissions
                                        .has_permission(Permission::ScheduleRead)
                                    {
                                        continue;
                                    }
                                }
                                websocket::WebsocketEvent::ServerTransferLogs => {
                                    if !socket_jwt
                                        .permissions
                                        .has_permission(Permission::AdminWebsocketTransfer)
                                    {
                                        continue;
                                    }
                                }
                                _ => {}
                            }

                            super::send_message(&sender, message).await
                        }
                        Err(RecvError::Closed) => {
                            tracing::debug!(
                                server = %server.uuid,
                                "websocket channel closed, stopping listener"
                            );
                            break;
                        }
                        Err(RecvError::Lagged(_)) => {
                            tracing::debug!(
                                server = %server.uuid,
                                "websocket lagged behind, messages dropped"
                            );
                        }
                    }
                }
            })
        });

        // Stdout Listener
        futures.push({
            let state = Arc::clone(&state);
            let socket_jwt = Arc::clone(&socket_jwt);
            let sender = Arc::clone(&sender);
            let server = server.clone();

            Box::pin(async move {
                loop {
                    if let Some(mut stdout) = server.container_stdout().await {
                        loop {
                            match stdout.recv().await {
                                Ok(stdout) => {
                                    let socket_jwt = socket_jwt.read().await;

                                    if let Some(jwt) = socket_jwt.as_ref()
                                        && jwt.base.validate(&state.config.jwt).await
                                    {
                                        super::send_message(
                                            &sender,
                                            websocket::WebsocketMessage::new(
                                                websocket::WebsocketEvent::ServerConsoleOutput,
                                                &[stdout],
                                            ),
                                        )
                                        .await;
                                    }
                                }
                                Err(RecvError::Closed) => break,
                                Err(RecvError::Lagged(_)) => {
                                    tracing::debug!(
                                        server = %server.uuid,
                                        "stdout lagged behind, messages dropped"
                                    );
                                }
                            }
                        }
                    }

                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                }
            })
        });

        // Jwt Listener
        futures.push({
            let socket_jwt = Arc::clone(&socket_jwt);
            let sender = Arc::clone(&sender);

            Box::pin(async move {
                super::jwt::listen_jwt(&sender, &socket_jwt).await;
            })
        });

        // Pinger
        futures.push({
            let sender = Arc::clone(&sender);

            Box::pin(async move {
                loop {
                    tokio::time::sleep(std::time::Duration::from_secs(30)).await;

                    let ping = sender
                        .lock()
                        .await
                        .send(Message::Ping(Bytes::from_static(&[1, 2, 3])))
                        .await;

                    if ping.is_err() {
                        break;
                    }
                }
            })
        });

        tokio::select! {
            _ = writer => {
                tracing::debug!(
                    server = %server.uuid,
                    "websocket writer finished",
                );
            }
            _ = futures_util::future::join_all(futures) => {
                tracing::debug!(
                    server = %server.uuid,
                    "websocket handles finished",
                );
            }
        }
    })
}
