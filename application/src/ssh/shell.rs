use crate::{
    routes::State,
    server::{
        activity::{Activity, ActivityEvent},
        permissions::Permission,
        websocket::WebsocketEvent,
    },
};
use russh::{Channel, server::Msg};
use serde_json::json;
use std::{net::IpAddr, pin::Pin, sync::Arc};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::broadcast::error::RecvError,
};

pub struct ShellSession {
    pub state: State,
    pub server: crate::server::Server,

    pub user_ip: Option<IpAddr>,
    pub user_uuid: uuid::Uuid,
}

impl ShellSession {
    #[inline]
    async fn has_permission(&self, permission: Permission) -> bool {
        self.server
            .user_permissions
            .has_permission(self.user_uuid, permission)
            .await
    }

    pub fn run(self, channel: Channel<Msg>) {
        tokio::spawn(async move {
            let (mut reader, writer) = channel.split();
            let mut reader = reader.make_reader();

            let logs = self
                .server
                .read_log(
                    &self.state.docker,
                    self.state.config.system.websocket_log_count,
                )
                .await
                .unwrap_or_default();

            {
                let prelude = ansi_term::Color::Yellow
                    .bold()
                    .paint(format!("[{} Daemon]:", self.state.config.app_name));

                let state_str = serde_json::to_value(self.server.state.get_state()).unwrap();
                let state_str = state_str.as_str().unwrap();

                writer
                    .make_writer()
                    .write_all(
                        format!("{prelude} Server marked as {state_str}...\r\n\x1b[2K").as_bytes(),
                    )
                    .await
                    .unwrap_or_default();
            }

            if self.server.state.get_state() != crate::server::state::ServerState::Offline
                || self.state.config.api.send_offline_server_logs
            {
                writer
                    .make_writer()
                    .write_all(logs.as_bytes())
                    .await
                    .unwrap();
            }

            let mut futures: Vec<Pin<Box<dyn futures_util::Future<Output = ()> + Send>>> =
                Vec::with_capacity(3);

            // Server Listener
            futures.push({
                let mut reciever = self.server.websocket.subscribe();
                let state = Arc::clone(&self.state);
                let server = self.server.clone();
                let user_uuid = self.user_uuid;
                let mut writer = writer.make_writer();

                Box::pin(async move {
                    loop {
                        match reciever.recv().await {
                            Ok(message) => match message.event {
                                WebsocketEvent::ServerInstallOutput => {
                                    if server
                                        .user_permissions
                                        .has_permission(
                                            user_uuid,
                                            Permission::AdminWebsocketInstall,
                                        )
                                        .await
                                    {
                                        writer
                                            .write_all(
                                                format!("{}\r\n\x1b[2K", message.args.join(" "))
                                                    .as_bytes(),
                                            )
                                            .await
                                            .unwrap_or_default();
                                    }
                                }
                                WebsocketEvent::ServerTransferLogs => {
                                    if server
                                        .user_permissions
                                        .has_permission(
                                            user_uuid,
                                            Permission::AdminWebsocketTransfer,
                                        )
                                        .await
                                    {
                                        writer
                                            .write_all(
                                                format!("{}\r\n\x1b[2K", message.args.join(" "))
                                                    .as_bytes(),
                                            )
                                            .await
                                            .unwrap_or_default();
                                    }
                                }
                                WebsocketEvent::ServerConsoleOutput => {
                                    writer
                                        .write_all(
                                            format!("{}\r\n\x1b[2K", message.args.join(" "))
                                                .as_bytes(),
                                        )
                                        .await
                                        .unwrap_or_default();
                                }
                                WebsocketEvent::ServerDaemonMessage => {
                                    writer
                                        .write_all(
                                            format!("{}\r\n\x1b[2K", message.args.join(" "))
                                                .as_bytes(),
                                        )
                                        .await
                                        .unwrap_or_default();
                                }
                                WebsocketEvent::ServerStatus => {
                                    let prelude = ansi_term::Color::Yellow
                                        .bold()
                                        .paint(format!("[{} Daemon]:", state.config.app_name));

                                    writer
                                        .write_all(
                                            format!(
                                                "{prelude} Server marked as {}...\r\n\x1b[2K",
                                                message.args[0]
                                            )
                                            .as_bytes(),
                                        )
                                        .await
                                        .unwrap_or_default();
                                }
                                _ => {}
                            },
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
                let server = self.server.clone();
                let mut writer = writer.make_writer();

                Box::pin(async move {
                    loop {
                        if let Some(mut stdout) = server.container_stdout().await {
                            loop {
                                match stdout.recv().await {
                                    Ok(stdout) => {
                                        if let Err(err) = writer
                                            .write_all(format!("{stdout}\r\n\x1b[2K").as_bytes())
                                            .await
                                        {
                                            tracing::error!(error = %err, "failed to write stdout");
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

                        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                    }
                })
            });

            // Stdin Listener
            futures.push({
                let server = self.server.clone();
                let state = Arc::clone(&self.state);
                let mut writer = writer.make_writer();

                Box::pin(async move {
                    let mut buffer = [0; 1024];
                    let mut current_line = Vec::with_capacity(1024);
                    let mut command_history: Vec<Vec<u8>> = Vec::with_capacity(20);
                    let mut history_index: Option<usize> = None;
                    let mut escape_sequence = false;
                    let mut cursor_sequence = false;
                    let mut sequence_buffer = Vec::with_capacity(3);
                    let mut cursor_pos = 0;

                    loop {
                        match reader.read(&mut buffer).await {
                            Ok(0) => break,
                            Ok(n) => {
                                for &byte in &buffer[..n] {
                                    if escape_sequence {
                                        sequence_buffer.push(byte);

                                        if byte == b'[' && sequence_buffer.len() == 1 {
                                            cursor_sequence = true;
                                        } else if cursor_sequence && sequence_buffer.len() == 2 {
                                            match byte {
                                                b'A' => { // Up arrow
                                                    if !command_history.is_empty() {
                                                        if history_index.is_none() {
                                                            if !current_line.is_empty() {
                                                                if command_history.len() >= 20 {
                                                                    command_history.remove(0);
                                                                }
                                                                command_history.push(current_line.clone());
                                                            }
                                                            history_index = Some(command_history.len() - 1);
                                                        } else if history_index.unwrap() > 0 {
                                                            history_index = Some(history_index.unwrap() - 1);
                                                        } else {
                                                            writer.write_all(b"\x07").await.unwrap_or_default();
                                                            writer.flush().await.unwrap_or_default();
                                                            escape_sequence = false;
                                                            cursor_sequence = false;
                                                            sequence_buffer.clear();
                                                            continue;
                                                        }

                                                        let history_cmd = &command_history[history_index.unwrap()];

                                                        writer.write_all(b"\r").await.unwrap_or_default();
                                                        let mut output = Vec::with_capacity(history_cmd.len() + 3);
                                                        output.extend_from_slice(b"\x1b[2K");
                                                        output.extend_from_slice(history_cmd);
                                                        writer.write_all(&output).await.unwrap_or_default();
                                                        writer.flush().await.unwrap_or_default();

                                                        current_line.clear();
                                                        current_line.extend_from_slice(history_cmd);
                                                        cursor_pos = current_line.len();
                                                    } else {
                                                        writer.write_all(b"\x07").await.unwrap_or_default();
                                                        writer.flush().await.unwrap_or_default();
                                                    }
                                                },
                                                b'B' => { // Down arrow
                                                    if let Some(idx) = history_index {
                                                        if idx < command_history.len() - 1 {
                                                            history_index = Some(idx + 1);
                                                            let history_cmd = &command_history[history_index.unwrap()];

                                                            writer.write_all(b"\r").await.unwrap_or_default();
                                                            let mut output = Vec::with_capacity(history_cmd.len() + 3);
                                                            output.extend_from_slice(b"\x1b[2K");
                                                            output.extend_from_slice(history_cmd);
                                                            writer.write_all(&output).await.unwrap_or_default();
                                                            writer.flush().await.unwrap_or_default();

                                                            current_line.clear();
                                                            current_line.extend_from_slice(history_cmd);
                                                            cursor_pos = current_line.len();
                                                        } else {
                                                            history_index = None;
                                                            writer.write_all(b"\r\x1b[2K").await.unwrap_or_default();
                                                            writer.flush().await.unwrap_or_default();
                                                            current_line.clear();
                                                            cursor_pos = 0;
                                                        }
                                                    } else {
                                                        writer.write_all(b"\x07").await.unwrap_or_default();
                                                        writer.flush().await.unwrap_or_default();
                                                    }
                                                },
                                                b'C' => { // Right arrow
                                                    if cursor_pos < current_line.len() {
                                                        cursor_pos += 1;
                                                        writer.write_all(b"\x1b[C").await.unwrap_or_default();
                                                        writer.flush().await.unwrap_or_default();
                                                    } else {
                                                        writer.write_all(b"\x07").await.unwrap_or_default();
                                                        writer.flush().await.unwrap_or_default();
                                                    }
                                                },
                                                b'D' => { // Left arrow
                                                    if cursor_pos > 0 {
                                                        cursor_pos -= 1;
                                                        writer.write_all(b"\x1b[D").await.unwrap_or_default();
                                                        writer.flush().await.unwrap_or_default();
                                                    } else {
                                                        writer.write_all(b"\x07").await.unwrap_or_default();
                                                        writer.flush().await.unwrap_or_default();
                                                    }
                                                },
                                                _ => {
                                                    writer.write_all(b"\x1b").await.unwrap_or_default();
                                                    writer.write_all(&sequence_buffer).await.unwrap_or_default();
                                                    writer.flush().await.unwrap_or_default();
                                                }
                                            }

                                            escape_sequence = false;
                                            cursor_sequence = false;
                                            sequence_buffer.clear();
                                        } else if sequence_buffer.len() >= 3 || (!cursor_sequence && sequence_buffer.len() >= 2) {
                                            writer.write_all(b"\x1b").await.unwrap_or_default();
                                            writer.write_all(&sequence_buffer).await.unwrap_or_default();
                                            writer.flush().await.unwrap_or_default();
                                            escape_sequence = false;
                                            cursor_sequence = false;
                                            sequence_buffer.clear();
                                        }
                                    } else if byte == 0x1b { // ESC character
                                        escape_sequence = true;
                                        sequence_buffer.clear();
                                    } else {
                                        match byte {
                                            b'\r' | b'\n' => {
                                                if !current_line.is_empty() {
                                                    let line = String::from_utf8_lossy(&current_line);

                                                    if !command_history.is_empty() && command_history.last().unwrap() != &current_line {
                                                        if command_history.len() >= 20 {
                                                            command_history.remove(0);
                                                        }
                                                        command_history.push(current_line.clone());
                                                    } else if command_history.is_empty() {
                                                        command_history.push(current_line.clone());
                                                    }
                                                    history_index = None;

                                                    if line.starts_with(&state.config.system.sftp.shell.cli.name) {
                                                        let prefix = &state.config.system.sftp.shell.cli.name;
                                                        writer.write_all(b"\r\n").await.unwrap_or_default();

                                                        let prelude = ansi_term::Color::Yellow
                                                            .bold()
                                                            .paint(format!("[{} Daemon]:", state.config.app_name));

                                                        let mut writeln = async |line: &str| {
                                                            writer
                                                                .write_all(format!("{prelude} {line}\r\n\x1b[2K").as_bytes())
                                                                .await
                                                                .unwrap_or_default();
                                                        };

                                                        let mut segments = line.split_whitespace();
                                                        segments.next();

                                                        match segments.next() {
                                                            Some("help") => {
                                                                writeln("Available commands:").await;
                                                                writeln("  help    - Show this help message").await;
                                                                writeln("  version - Show the current version").await;
                                                                writeln("  power   - Send a power action to the server").await;
                                                            }
                                                            Some("version") => {
                                                                writeln(&format!("Current version: {}", crate::VERSION)).await;
                                                            }
                                                            Some("power") => {
                                                                match segments.next() {
                                                                    Some("start") => {
                                                                        if self.has_permission(Permission::ControlStart).await {
                                                                            if let Err(err) = server.start(&self.state.docker, None).await {
                                                                                tracing::error!(
                                                                                    server = %server.uuid,
                                                                                    "failed to start server: {:#?}",
                                                                                    err,
                                                                                );

                                                                                writeln("An unexpected error occurred while starting the server. Please contact an Administrator.")
                                                                                        .await;
                                                                            } else {
                                                                                server
                                                                                    .activity
                                                                                    .log_activity(Activity {
                                                                                        event: ActivityEvent::PowerStart,
                                                                                        user: Some(self.user_uuid),
                                                                                        ip: self.user_ip,
                                                                                        metadata: None,
                                                                                        timestamp: chrono::Utc::now(),
                                                                                    })
                                                                                    .await;
                                                                            }
                                                                        } else {
                                                                            writeln("You are missing the `control.start` permission to do this.").await;
                                                                        }
                                                                    }
                                                                    Some("restart") => {
                                                                        if self.has_permission(Permission::ControlRestart).await {
                                                                            if let Err(err) = server.restart(&self.state.docker, None).await {
                                                                                tracing::error!(
                                                                                    server = %server.uuid,
                                                                                    "failed to restart server: {:#?}",
                                                                                    err,
                                                                                );

                                                                                writeln("An unexpected error occurred while restarting the server. Please contact an Administrator.")
                                                                                        .await;
                                                                            } else {
                                                                                server
                                                                                    .activity
                                                                                    .log_activity(Activity {
                                                                                        event: ActivityEvent::PowerRestart,
                                                                                        user: Some(self.user_uuid),
                                                                                        ip: self.user_ip,
                                                                                        metadata: None,
                                                                                        timestamp: chrono::Utc::now(),
                                                                                    })
                                                                                    .await;
                                                                            }
                                                                        } else {
                                                                            writeln("You are missing the `control.restart` permission to do this.").await;
                                                                        }
                                                                    }
                                                                    Some("stop") => {
                                                                        if self.has_permission(Permission::ControlStop).await {
                                                                            if let Err(err) = server.stop(&self.state.docker, None).await {
                                                                                tracing::error!(
                                                                                    server = %server.uuid,
                                                                                    "failed to stop server: {:#?}",
                                                                                    err,
                                                                                );

                                                                                writeln("An unexpected error occurred while stopping the server. Please contact an Administrator.")
                                                                                        .await;
                                                                            } else {
                                                                                server
                                                                                    .activity
                                                                                    .log_activity(Activity {
                                                                                        event: ActivityEvent::PowerStop,
                                                                                        user: Some(self.user_uuid),
                                                                                        ip: self.user_ip,
                                                                                        metadata: None,
                                                                                        timestamp: chrono::Utc::now(),
                                                                                    })
                                                                                    .await;
                                                                            }
                                                                        } else {
                                                                            writeln("You are missing the `control.stop` permission to do this.").await;
                                                                        }
                                                                    }
                                                                    Some("kill") => {
                                                                        if self.has_permission(Permission::ControlStop).await {
                                                                            if let Err(err) = server.kill(&self.state.docker).await {
                                                                                tracing::error!(
                                                                                    server = %server.uuid,
                                                                                    "failed to kill server: {:#?}",
                                                                                    err,
                                                                                );

                                                                                writeln("An unexpected error occurred while killing the server. Please contact an Administrator.")
                                                                                        .await;
                                                                            } else {
                                                                                server
                                                                                    .activity
                                                                                    .log_activity(Activity {
                                                                                        event: ActivityEvent::PowerKill,
                                                                                        user: Some(self.user_uuid),
                                                                                        ip: self.user_ip,
                                                                                        metadata: None,
                                                                                        timestamp: chrono::Utc::now(),
                                                                                    })
                                                                                    .await;
                                                                            }
                                                                        } else {
                                                                            writeln("You are missing the `control.kill` permission to do this.").await;
                                                                        }
                                                                    }
                                                                    _ => {
                                                                        writeln(&format!("Usage: {prefix} power <start|restart|stop|kill>")).await;
                                                                    }
                                                                }
                                                            }
                                                            _ => {
                                                                writeln("Unknown command. Type '.wings help' for a list of commands.").await;
                                                            }
                                                        }
                                                    } else if self.has_permission(Permission::ControlConsole).await {
                                                        if let Some(stdin) = server.container_stdin().await {
                                                            if let Err(err) = stdin.send(format!("{line}\n")).await {
                                                                writer.write_all(b"\r\n").await.unwrap_or_default();

                                                                tracing::error!(
                                                                    server = %server.uuid,
                                                                    "failed to send command to server: {}",
                                                                    err
                                                                );
                                                            } else {
                                                                writer.write_all(b"\r").await.unwrap_or_default();

                                                                server
                                                                    .activity
                                                                    .log_activity(Activity {
                                                                        event: ActivityEvent::ConsoleCommand,
                                                                        user: Some(self.user_uuid),
                                                                        ip: self.user_ip,
                                                                        metadata: Some(json!({
                                                                            "command": line,
                                                                        })),
                                                                        timestamp: chrono::Utc::now(),
                                                                    })
                                                                    .await;
                                                            }
                                                        }
                                                    } else {
                                                        writer.write_all(b"\r\n\x1b[2K").await.unwrap_or_default();
                                                        writer
                                                            .write_all(b"You are missing the `control.console` permission to do this.\r\n\x1b[2K")
                                                            .await
                                                            .unwrap_or_default();
                                                    }

                                                    current_line.clear();
                                                    cursor_pos = 0;
                                                }

                                                writer.flush().await.unwrap_or_default();
                                            }
                                            8 | 127 => { // Backspace
                                                if cursor_pos > 0 {
                                                    if cursor_pos < current_line.len() {
                                                        current_line.remove(cursor_pos - 1);
                                                        cursor_pos -= 1;

                                                        writer.write_all(b"\x08").await.unwrap_or_default();
                                                        writer.write_all(b"\x1b[K").await.unwrap_or_default();
                                                        writer.write_all(&current_line[cursor_pos..]).await.unwrap_or_default();

                                                        if cursor_pos < current_line.len() {
                                                            let move_back = current_line.len() - cursor_pos;
                                                            writer.write_all(format!("\x1b[{move_back}D").as_bytes()).await.unwrap_or_default();
                                                        }
                                                    } else {
                                                        current_line.pop();
                                                        cursor_pos -= 1;
                                                        writer.write_all(b"\x08 \x08").await.unwrap_or_default();
                                                    }

                                                    writer.flush().await.unwrap_or_default();
                                                } else {
                                                    writer.write_all(b"\x07").await.unwrap_or_default();
                                                    writer.flush().await.unwrap_or_default();
                                                }
                                            }
                                            _ => {
                                                if current_line.len() < 1024 {
                                                    if cursor_pos < current_line.len() {
                                                        current_line.insert(cursor_pos, byte);
                                                        cursor_pos += 1;

                                                        writer.write_all(&[byte]).await.unwrap_or_default();
                                                        writer.write_all(&current_line[cursor_pos..]).await.unwrap_or_default();

                                                        if cursor_pos < current_line.len() {
                                                            let move_back = current_line.len() - cursor_pos;
                                                            writer.write_all(format!("\x1b[{move_back}D").as_bytes()).await.unwrap_or_default();
                                                        }
                                                    } else {
                                                        writer.write_all(&[byte]).await.unwrap_or_default();
                                                        current_line.push(byte);
                                                        cursor_pos += 1;
                                                    }
                                                    writer.flush().await.unwrap_or_default();
                                                } else {
                                                    writer.write_all(b"\x07").await.unwrap_or_default();
                                                    writer.flush().await.unwrap_or_default();
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                            Err(err) => {
                                tracing::debug!("error reading from SSH session: {:#?}", err);
                                break;
                            }
                        }
                    }
                })
            });

            futures_util::future::join_all(futures).await;
        });
    }
}
