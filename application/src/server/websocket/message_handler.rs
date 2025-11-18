use super::{WebsocketEvent, WebsocketMessage};
use crate::server::{
    activity::{Activity, ActivityEvent},
    permissions::Permission,
};
use anyhow::Context;
use serde_json::json;
use std::{net::IpAddr, str::FromStr};

pub async fn handle_message(
    state: &crate::routes::AppState,
    user_ip: IpAddr,
    server: &crate::server::Server,
    websocket_handler: &super::ServerWebsocketHandler,
    message: super::WebsocketMessage,
) -> Result<(), anyhow::Error> {
    let user_ip = Some(user_ip);

    match message.event {
        WebsocketEvent::SendStats => {
            websocket_handler
                .send_message(WebsocketMessage::new(
                    WebsocketEvent::ServerStats,
                    [serde_json::to_string(&server.resource_usage().await)?].into(),
                ))
                .await;
        }
        WebsocketEvent::SendServerLogs => {
            if server.state.get_state() != crate::server::state::ServerState::Offline
                || state.config.api.send_offline_server_logs
            {
                let socket_jwt = websocket_handler.get_jwt().await?;

                if socket_jwt.use_console_read_permission
                    && !socket_jwt
                        .permissions
                        .has_permission(Permission::ControlReadConsole)
                {
                    return Ok(());
                }
                drop(socket_jwt);

                let logs = server
                    .read_log(&state.docker, state.config.system.websocket_log_count)
                    .await
                    .context("failed to read server logs")?;

                for line in logs.lines() {
                    websocket_handler
                        .send_message(WebsocketMessage::new(
                            WebsocketEvent::ServerConsoleOutput,
                            [line.trim().to_string()].into(),
                        ))
                        .await;
                }
            }
        }
        WebsocketEvent::SetState => {
            let power_state = crate::models::ServerPowerAction::from_str(
                message.args.first().map_or("", |s| s.as_str()),
            )?;

            match power_state {
                crate::models::ServerPowerAction::Start => {
                    let socket_jwt = websocket_handler.get_jwt().await?;

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
                    drop(socket_jwt);

                    if server.state.get_state() != crate::server::state::ServerState::Offline {
                        websocket_handler
                            .send_error("Server is already running or starting.")
                            .await;

                        return Ok(());
                    }

                    if let Err(err) = server.start(None, false).await {
                        match err.downcast::<&str>() {
                            Ok(message) => {
                                websocket_handler.send_error(message).await;
                            }
                            Err(err) => {
                                tracing::error!(
                                    server = %server.uuid,
                                    "failed to start server: {:#?}",
                                    err,
                                );

                                websocket_handler.send_admin_error(err).await;
                            }
                        }
                    } else {
                        server
                            .activity
                            .log_activity(Activity {
                                event: ActivityEvent::PowerStart,
                                user: Some(websocket_handler.get_jwt().await?.user_uuid),
                                ip: user_ip,
                                metadata: None,
                                timestamp: chrono::Utc::now(),
                            })
                            .await;
                    }
                }
                crate::models::ServerPowerAction::Restart => {
                    let socket_jwt = websocket_handler.get_jwt().await?;

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
                    drop(socket_jwt);

                    if server.restarting.load(std::sync::atomic::Ordering::SeqCst) {
                        websocket_handler
                            .send_error("Server is already restarting.")
                            .await;

                        return Ok(());
                    }

                    let auto_kill = server.configuration.read().await.auto_kill;
                    if let Err(err) = if auto_kill.enabled && auto_kill.seconds > 0 {
                        server
                            .restart_with_kill_timeout(
                                None,
                                std::time::Duration::from_secs(auto_kill.seconds),
                            )
                            .await
                    } else {
                        server.restart(None).await
                    } {
                        match err.downcast::<&str>() {
                            Ok(message) => {
                                websocket_handler.send_error(message).await;
                            }
                            Err(err) => {
                                tracing::error!(
                                    server = %server.uuid,
                                    "failed to restart server: {:#?}",
                                    err
                                );

                                websocket_handler.send_admin_error(err).await;
                            }
                        }
                    } else {
                        server
                            .activity
                            .log_activity(Activity {
                                event: ActivityEvent::PowerRestart,
                                user: Some(websocket_handler.get_jwt().await?.user_uuid),
                                ip: user_ip,
                                metadata: None,
                                timestamp: chrono::Utc::now(),
                            })
                            .await;
                    }
                }
                crate::models::ServerPowerAction::Stop => {
                    let socket_jwt = websocket_handler.get_jwt().await?;

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
                    drop(socket_jwt);

                    if matches!(
                        server.state.get_state(),
                        crate::server::state::ServerState::Offline
                            | crate::server::state::ServerState::Stopping
                    ) {
                        websocket_handler
                            .send_error("Server is already offline or stopping.")
                            .await;

                        return Ok(());
                    }

                    let auto_kill = server.configuration.read().await.auto_kill;
                    if let Err(err) = if auto_kill.enabled && auto_kill.seconds > 0 {
                        server
                            .stop_with_kill_timeout(
                                std::time::Duration::from_secs(auto_kill.seconds),
                                false,
                            )
                            .await
                    } else {
                        server.stop(None, false).await
                    } {
                        match err.downcast::<&str>() {
                            Ok(message) => {
                                websocket_handler.send_error(message).await;
                            }
                            Err(err) => {
                                tracing::error!(
                                    server = %server.uuid,
                                    "failed to stop server: {:#?}",
                                    err
                                );

                                websocket_handler.send_admin_error(err).await;
                            }
                        }
                    } else {
                        server
                            .activity
                            .log_activity(Activity {
                                event: ActivityEvent::PowerStop,
                                user: Some(websocket_handler.get_jwt().await?.user_uuid),
                                ip: user_ip,
                                metadata: None,
                                timestamp: chrono::Utc::now(),
                            })
                            .await;
                    }
                }
                crate::models::ServerPowerAction::Kill => {
                    let socket_jwt = websocket_handler.get_jwt().await?;

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
                    drop(socket_jwt);

                    if server.state.get_state() == crate::server::state::ServerState::Offline {
                        websocket_handler
                            .send_error("Server is already offline.")
                            .await;

                        return Ok(());
                    }

                    if let Err(err) = server.kill(false).await {
                        tracing::error!(
                            server = %server.uuid,
                            "failed to kill server: {:#?}",
                            err
                        );

                        websocket_handler.send_admin_error(err).await;
                    } else {
                        server
                            .activity
                            .log_activity(Activity {
                                event: ActivityEvent::PowerKill,
                                user: Some(websocket_handler.get_jwt().await?.user_uuid),
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
            let socket_jwt = websocket_handler.get_jwt().await?;

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
            drop(socket_jwt);

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
                            user: Some(websocket_handler.get_jwt().await?.user_uuid),
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
