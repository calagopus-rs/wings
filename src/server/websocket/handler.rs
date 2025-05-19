use crate::{
    routes::GetState,
    server::{permissions::Permission, websocket},
};
use axum::{
    body::Bytes,
    extract::{ConnectInfo, Path, WebSocketUpgrade, ws::Message},
    http::{HeaderMap, StatusCode},
    response::Response,
};
use futures_util::{SinkExt, StreamExt};
use std::{net::SocketAddr, sync::Arc};
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
            return Response::builder()
                .status(StatusCode::NOT_FOUND)
                .header("Content-Type", "application/json")
                .body(axum::body::Body::from(
                    serde_json::to_string(&crate::routes::ApiError::new("server not found"))
                        .unwrap(),
                ))
                .unwrap();
        }
    };

    let user_ip = state.config.find_ip(&headers, connect_info);

    ws.on_upgrade(move |socket| async move {
        let (sender, mut reciever) = socket.split();
        let sender = Arc::new(Mutex::new(sender));
        let socket_jwt = Arc::new(RwLock::new(None));

        let writer = tokio::spawn({
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

                    if let Message::Ping(_) = ws_data {
                        continue;
                    }

                    if let Message::Pong(_) = ws_data {
                        continue;
                    }

                    match super::jwt::handle_jwt(&state, &server, &sender, &socket_jwt, ws_data)
                        .await
                    {
                        Ok(Some((message, jwt))) => {
                            tokio::spawn({
                                let sender = Arc::clone(&sender);
                                let state = Arc::clone(&state);
                                let server = server.clone();

                                async move {
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
                            });
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
                        }
                    }
                }
            }
        });

        let mut handles = Vec::with_capacity(4);

        // Server Listener
        handles.push(tokio::spawn({
            let socket_jwt = Arc::clone(&socket_jwt);
            let sender = Arc::clone(&sender);
            let server = Arc::clone(&server);
            let mut reciever = server.websocket.subscribe();

            async move {
                loop {
                    if let Ok(message) = reciever.recv().await {
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
                            websocket::WebsocketEvent::ServerBackupCompleted => {
                                if !socket_jwt
                                    .permissions
                                    .has_permission(Permission::BackupRead)
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

                        super::send_message(&sender, message).await;
                    }
                }
            }
        }));

        // Stdout Listener
        handles.push(tokio::spawn({
            let state = Arc::clone(&state);
            let socket_jwt = Arc::clone(&socket_jwt);
            let sender = Arc::clone(&sender);
            let server = server.clone();

            async move {
                loop {
                    if let Some(mut stdout) = server.container_stdout().await {
                        let thread = tokio::spawn({
                            let socket_jwt = Arc::clone(&socket_jwt);
                            let sender = Arc::clone(&sender);
                            let state = Arc::clone(&state);
                            let server = server.clone();

                            async move {
                                loop {
                                    match stdout.recv().await {
                                        Ok(stdout) => {
                                            let socket_jwt = socket_jwt.read().await;

                                            if let Some(jwt) = socket_jwt.as_ref() {
                                                if jwt.base.validate(&state.config.jwt) {
                                                    super::send_message(
                                                &sender,
                                                websocket::WebsocketMessage::new(
                                                    websocket::WebsocketEvent::ServerConsoleOutput,
                                                    &[stdout],
                                                ),
                                            ).await;
                                                }
                                            }
                                        }
                                        Err(RecvError::Closed) => {
                                            break;
                                        }
                                        Err(RecvError::Lagged(_)) => {
                                            tracing::debug!(
                                                server = %server.uuid,
                                                "stdout lagged behind, messages dropped"
                                            );
                                        }
                                    }
                                }
                            }
                        });

                        thread.await.unwrap_or_default();
                    }

                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                }
            }
        }));

        // Jwt Listener
        handles.push(tokio::spawn({
            let socket_jwt = Arc::clone(&socket_jwt);
            let sender = Arc::clone(&sender);

            async move {
                loop {
                    super::jwt::listen_jwt(&sender, &socket_jwt).await;
                }
            }
        }));

        // Pinger
        handles.push(tokio::spawn({
            let sender = Arc::clone(&sender);

            async move {
                loop {
                    tokio::time::sleep(std::time::Duration::from_secs(10)).await;

                    let ping = sender
                        .lock()
                        .await
                        .send(Message::Ping(Bytes::from_static(&[1, 2, 3])))
                        .await;

                    if ping.is_err() {
                        break;
                    }
                }
            }
        }));

        writer.await.unwrap_or_default();

        for handle in handles {
            handle.abort();
        }
    })
}
