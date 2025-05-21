use super::{WebsocketEvent, WebsocketJwtPayload, WebsocketMessage};
use crate::server::{
    activity::{Activity, ActivityEvent},
    permissions::Permission,
};
use axum::extract::ws::{Message, WebSocket};
use futures_util::stream::SplitSink;
use serde_json::json;
use std::net::IpAddr;
use tokio::sync::Mutex;

pub async fn handle_message(
    state: &crate::routes::AppState,
    user_ip: IpAddr,
    server: &crate::server::Server,
    sender: &Mutex<SplitSink<WebSocket, Message>>,
    socket_jwt: &WebsocketJwtPayload,
    message: super::WebsocketMessage,
) -> Result<(), Box<dyn std::error::Error>> {
    let user_ip = Some(user_ip);

    match message.event {
        WebsocketEvent::SendStats => {
            super::send_message(
                sender,
                WebsocketMessage::new(
                    WebsocketEvent::ServerStats,
                    &[serde_json::to_string(&server.resource_usage().await).unwrap()],
                ),
            )
            .await;

            let state_str = serde_json::to_value(server.state.get_state()).unwrap();
            let state_str = state_str.as_str().unwrap();

            super::send_message(
                sender,
                WebsocketMessage::new(WebsocketEvent::ServerStatus, &[state_str.to_string()]),
            )
            .await;
        }
        WebsocketEvent::SendServerLogs => {
            if server.state.get_state() != crate::server::state::ServerState::Offline
                || state.config.api.send_offline_server_logs
            {
                let logs = server
                    .read_log(&state.docker, state.config.system.websocket_log_count)
                    .await
                    .unwrap();

                for line in logs.lines() {
                    super::send_message(
                        sender,
                        WebsocketMessage::new(
                            WebsocketEvent::ServerConsoleOutput,
                            &[line.trim().to_string()],
                        ),
                    )
                    .await;
                }
            }
        }
        WebsocketEvent::SetState => {
            let power_state = serde_json::from_value(serde_json::Value::from(
                message.args.first().map_or("", |v| v.as_str()),
            ))?;

            match power_state {
                crate::models::ServerPowerAction::Start => {
                    if !socket_jwt
                        .permissions
                        .has_permission(Permission::ControlStart)
                    {
                        tracing::debug!(
                            server = %server.uuid,
                            "jwt does not have permission to start server: {:?}",
                            socket_jwt.permissions
                        );

                        return Ok(());
                    }

                    if let Err(err) = server.start(&state.docker, None).await {
                        tracing::error!(
                            server = %server.uuid,
                            "failed to start server: {}",
                            err,
                        );
                    } else {
                        server
                            .activity
                            .log_activity(Activity {
                                event: ActivityEvent::PowerStart,
                                user: Some(socket_jwt.user_uuid),
                                ip: user_ip,
                                metadata: None,
                                timestamp: chrono::Utc::now(),
                            })
                            .await;
                    }
                }
                crate::models::ServerPowerAction::Restart => {
                    if !socket_jwt
                        .permissions
                        .has_permission(Permission::ControlRestart)
                    {
                        tracing::debug!(
                            server = %server.uuid,
                            "jwt does not have permission to start server: {:?}",
                            socket_jwt.permissions
                        );

                        return Ok(());
                    }

                    if let Err(err) = server.restart(&state.docker, None).await {
                        tracing::error!(
                            server = %server.uuid,
                            "failed to restart server: {}",
                            err
                        );
                    } else {
                        server
                            .activity
                            .log_activity(Activity {
                                event: ActivityEvent::PowerRestart,
                                user: Some(socket_jwt.user_uuid),
                                ip: user_ip,
                                metadata: None,
                                timestamp: chrono::Utc::now(),
                            })
                            .await;
                    }
                }
                crate::models::ServerPowerAction::Stop => {
                    if !socket_jwt
                        .permissions
                        .has_permission(Permission::ControlStop)
                    {
                        tracing::debug!(
                            server = %server.uuid,
                            "jwt does not have permission to start server: {:?}",
                            socket_jwt.permissions
                        );

                        return Ok(());
                    }

                    if let Err(err) = server.stop(&state.docker, None).await {
                        tracing::error!(
                            server = %server.uuid,
                            "failed to stop server: {}",
                            err
                        );
                    } else {
                        server
                            .activity
                            .log_activity(Activity {
                                event: ActivityEvent::PowerStop,
                                user: Some(socket_jwt.user_uuid),
                                ip: user_ip,
                                metadata: None,
                                timestamp: chrono::Utc::now(),
                            })
                            .await;
                    }
                }
                crate::models::ServerPowerAction::Kill => {
                    if !socket_jwt
                        .permissions
                        .has_permission(Permission::ControlStop)
                    {
                        tracing::debug!(
                            server = %server.uuid,
                            "jwt does not have permission to start server: {:?}",
                            socket_jwt.permissions,
                        );

                        return Ok(());
                    }

                    if let Err(err) = server.kill(&state.docker).await {
                        tracing::error!(
                            server = %server.uuid,
                            "failed to kill server: {}",
                            err
                        );
                    } else {
                        server
                            .activity
                            .log_activity(Activity {
                                event: ActivityEvent::PowerKill,
                                user: Some(socket_jwt.user_uuid),
                                ip: user_ip,
                                metadata: None,
                                timestamp: chrono::Utc::now(),
                            })
                            .await;
                    }
                }
            }
        }
        WebsocketEvent::SendCommand => {
            if !socket_jwt
                .permissions
                .has_permission(Permission::ControlConsole)
            {
                tracing::debug!(
                    server = %server.uuid,
                    "jwt does not have permission to send command to server: {:?}",
                    socket_jwt.permissions
                );

                return Ok(());
            }

            let raw_command = message.args.first().map_or("", |v| v.as_str());
            if let Some(stdin) = server.container_stdin().await {
                let mut command = raw_command.to_string();
                command.push('\n');

                if let Err(err) = stdin.send(command).await {
                    tracing::error!(
                        server = %server.uuid,
                        "failed to send command to server: {}",
                        err
                    );
                } else {
                    server
                        .activity
                        .log_activity(Activity {
                            event: ActivityEvent::ConsoleCommand,
                            user: Some(socket_jwt.user_uuid),
                            ip: user_ip,
                            metadata: Some(json!({
                                "command": raw_command,
                            })),
                            timestamp: chrono::Utc::now(),
                        })
                        .await;
                }
            }
        }
        _ => {
            tracing::debug!(
                "received websocket message that will not be handled: {:?}",
                message
            );
        }
    }

    Ok(())
}
