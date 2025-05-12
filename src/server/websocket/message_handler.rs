use super::{WebsocketEvent, WebsocketJwtPayload, WebsocketMessage, WebsocketPermission};
use axum::extract::ws::{Message, WebSocket};
use futures_util::stream::SplitSink;
use tokio::sync::Mutex;

pub async fn handle_message(
    state: &crate::routes::AppState,
    server: &crate::server::Server,
    sender: &Mutex<SplitSink<WebSocket, Message>>,
    socket_jwt: &WebsocketJwtPayload,
    message: super::WebsocketMessage,
) -> Result<(), Box<dyn std::error::Error>> {
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
        WebsocketEvent::SetState => {
            let power_state = serde_json::from_value(serde_json::Value::from(
                message.args.first().map_or("", |v| v.as_str()),
            ))?;

            match power_state {
                crate::models::ServerPowerAction::Start => {
                    if let Err(err) = server.start(&state.docker, None).await {
                        crate::logger::log(
                            crate::logger::LoggerLevel::Error,
                            format!("Failed to start server: {}", err),
                        );
                    }
                }
                crate::models::ServerPowerAction::Kill => {
                    if let Err(err) = server.kill(&state.docker).await {
                        crate::logger::log(
                            crate::logger::LoggerLevel::Error,
                            format!("Failed to kill server: {}", err),
                        );
                    }
                }
                crate::models::ServerPowerAction::Stop => {
                    if let Err(err) = server.stop(&state.docker, None).await {
                        crate::logger::log(
                            crate::logger::LoggerLevel::Error,
                            format!("Failed to stop server: {}", err),
                        );
                    }
                }
                crate::models::ServerPowerAction::Restart => {
                    if let Err(err) = server.restart(&state.docker, None).await {
                        crate::logger::log(
                            crate::logger::LoggerLevel::Error,
                            format!("Failed to restart server: {}", err),
                        );
                    }
                }
            }
        }
        WebsocketEvent::SendCommand => {
            if !socket_jwt.has_permission(WebsocketPermission::ControlConsole) {
                crate::logger::log(
                    crate::logger::LoggerLevel::Debug,
                    format!(
                        "JWT does not have permission to send command: {:?}",
                        socket_jwt.permissions
                    ),
                );

                return Ok(());
            }

            let command = message.args.first().map_or("", |v| v.as_str());

            if let Some(stdin) = server.container_stdin().await {
                let mut command = command.to_string();
                command.push('\n');

                if let Err(err) = stdin.send(command).await {
                    crate::logger::log(
                        crate::logger::LoggerLevel::Debug,
                        format!("Failed to send command to docker: {}", err),
                    );
                }
            }
        }
        _ => {
            crate::logger::log(
                crate::logger::LoggerLevel::Debug,
                format!("Received message that will not be handled: {:?}", message),
            );
        }
    }

    Ok(())
}
