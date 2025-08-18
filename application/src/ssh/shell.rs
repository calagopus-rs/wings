use crate::{
    routes::State,
    server::{
        activity::{Activity, ActivityEvent},
        permissions::Permission,
        websocket::WebsocketEvent,
    },
};
use russh::{Channel, ChannelWriteHalf, server::Msg};
use serde_json::json;
use std::{net::IpAddr, pin::Pin, sync::Arc};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::broadcast::error::RecvError,
};

#[derive(Debug, Clone, PartialEq, Eq, Hash, Copy)]
pub enum ShellMode {
    Normal,
    WinScp,
}

pub struct ShellSession {
    pub state: State,
    pub server: crate::server::Server,

    pub user_ip: Option<IpAddr>,
    pub user_uuid: uuid::Uuid,
    pub mode: ShellMode,
}

impl ShellSession {
    #[inline]
    async fn has_permission(&self, permission: Permission) -> bool {
        self.server
            .user_permissions
            .has_permission(self.user_uuid, permission)
            .await
    }

    async fn handle_cli_command(
        &mut self,
        line: &str,
        writer: &mut Pin<Box<impl tokio::io::AsyncWrite>>,
    ) {
        let prefix = &self.state.config.system.sftp.shell.cli.name;
        writer.write_all(b"\r\n").await.unwrap_or_default();

        let prelude = ansi_term::Color::Yellow
            .bold()
            .paint(format!("[{} Daemon]:", self.state.config.app_name));

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
                writeln("  stats   - Show server statistics").await;
            }
            Some("version") => {
                writeln(&format!("Current version: {}", crate::VERSION)).await;
            }
            Some("power") => match segments.next() {
                Some("start") => {
                    if self.has_permission(Permission::ControlStart).await {
                        if self.server.state.get_state()
                            != crate::server::state::ServerState::Offline
                        {
                            writeln("Server is already online.").await;
                            return;
                        }

                        if let Err(err) = self.server.start(None, false).await {
                            match err.downcast::<&str>() {
                                Ok(message) => writeln(message).await,
                                Err(err) => {
                                    tracing::error!(
                                        server = %self.server.uuid,
                                        "failed to start server: {:#?}",
                                        err,
                                    );

                                    writeln("An unexpected error occurred while starting the server. Please contact an Administrator.")
                                        .await;
                                }
                            }
                        } else {
                            self.server
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
                        if self
                            .server
                            .restarting
                            .load(std::sync::atomic::Ordering::SeqCst)
                        {
                            writeln("Server is already restarting.").await;
                            return;
                        }

                        let auto_kill = self.server.configuration.read().await.auto_kill;
                        if let Err(err) = if auto_kill.enabled && auto_kill.seconds > 0 {
                            self.server
                                .restart_with_kill_timeout(
                                    None,
                                    std::time::Duration::from_secs(auto_kill.seconds),
                                )
                                .await
                        } else {
                            self.server.restart(None).await
                        } {
                            match err.downcast::<&str>() {
                                Ok(message) => writeln(message).await,
                                Err(err) => {
                                    tracing::error!(
                                        server = %self.server.uuid,
                                        "failed to restart server: {:#?}",
                                        err,
                                    );

                                    writeln("An unexpected error occurred while restarting the server. Please contact an Administrator.")
                                        .await;
                                }
                            }
                        } else {
                            self.server
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
                        writeln("You are missing the `control.restart` permission to do this.")
                            .await;
                    }
                }
                Some("stop") => {
                    if self.has_permission(Permission::ControlStop).await {
                        if matches!(
                            self.server.state.get_state(),
                            crate::server::state::ServerState::Offline
                                | crate::server::state::ServerState::Stopping
                        ) {
                            writeln("Server is already offline or stopping.").await;
                            return;
                        }

                        let auto_kill = self.server.configuration.read().await.auto_kill;
                        if let Err(err) = if auto_kill.enabled && auto_kill.seconds > 0 {
                            self.server
                                .stop_with_kill_timeout(
                                    std::time::Duration::from_secs(auto_kill.seconds),
                                    false,
                                )
                                .await
                        } else {
                            self.server.stop(None, false).await
                        } {
                            match err.downcast::<&str>() {
                                Ok(message) => writeln(message).await,
                                Err(err) => {
                                    tracing::error!(
                                        server = %self.server.uuid,
                                        "failed to stop server: {:#?}",
                                        err,
                                    );

                                    writeln("An unexpected error occurred while stopping the server. Please contact an Administrator.")
                                        .await;
                                }
                            }
                        } else {
                            self.server
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
                        if self.server.state.get_state()
                            == crate::server::state::ServerState::Offline
                        {
                            writeln("Server is already offline.").await;
                            return;
                        }

                        if let Err(err) = self.server.kill(false).await {
                            tracing::error!(
                                server = %self.server.uuid,
                                "failed to kill server: {:#?}",
                                err,
                            );

                            writeln("An unexpected error occurred while killing the server. Please contact an Administrator.")
                                        .await;
                        } else {
                            self.server
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
            },
            Some("stats") => {
                let resource_usage = self.server.resource_usage().await;

                writeln("Server Statistics:").await;
                writeln(&format!(
                    "  CPU Usage: {:.2}% / {}",
                    resource_usage.cpu_absolute,
                    if self.server.configuration.read().await.build.cpu_limit == 0 {
                        "Unlimited".to_string()
                    } else {
                        format!(
                            "{}%",
                            self.server.configuration.read().await.build.cpu_limit
                        )
                    }
                ))
                .await;
                writeln(&format!(
                    "  Memory Usage: {} / {} ({:.2}%)",
                    human_bytes::human_bytes(resource_usage.memory_bytes as f64),
                    if self.server.configuration.read().await.build.memory_limit == 0 {
                        "Unlimited".to_string()
                    } else {
                        human_bytes::human_bytes(
                            (self.server.configuration.read().await.build.memory_limit
                                * 1024
                                * 1024) as f64,
                        )
                    },
                    (resource_usage.memory_bytes as f64 / resource_usage.memory_limit_bytes as f64
                        * 100.0)
                        .min(100.0)
                ))
                .await;
                writeln(&format!(
                    "  Disk Usage: {} / {} ({:.2}%)",
                    human_bytes::human_bytes(resource_usage.disk_bytes as f64),
                    if self.server.filesystem.disk_limit() == 0 {
                        "Unlimited".to_string()
                    } else {
                        human_bytes::human_bytes(self.server.filesystem.disk_limit() as f64)
                    },
                    (resource_usage.disk_bytes as f64 / self.server.filesystem.disk_limit() as f64
                        * 100.0)
                        .min(100.0)
                ))
                .await;
                writeln("  Network Usage:").await;
                writeln(&format!(
                    "    Received: {}",
                    human_bytes::human_bytes(resource_usage.network.rx_bytes as f64)
                ))
                .await;
                writeln(&format!(
                    "    Sent: {}",
                    human_bytes::human_bytes(resource_usage.network.tx_bytes as f64)
                ))
                .await;
            }
            _ => {
                writeln("Unknown command. Type '.wings help' for a list of commands.").await;
            }
        }
    }

    async fn handle_special_keys(
        &mut self,
        byte: u8,
        current_line: &mut Vec<u8>,
        cursor_pos: &mut usize,
        command_history: &mut Vec<Vec<u8>>,
        history_index: &mut Option<usize>,
        data_writer: &mut Pin<Box<impl tokio::io::AsyncWrite>>,
    ) {
        match byte {
            b'A' => {
                if !command_history.is_empty() {
                    if history_index.is_none() {
                        if !current_line.is_empty() {
                            if command_history.len() >= 20 {
                                command_history.remove(0);
                            }
                            command_history.push(current_line.clone());
                        }
                        *history_index = Some(command_history.len() - 1);
                    } else if history_index.unwrap() > 0 {
                        *history_index = Some(history_index.unwrap() - 1);
                    } else {
                        data_writer.write_all(b"\x07").await.unwrap_or_default();
                        data_writer.flush().await.unwrap_or_default();
                        return;
                    }

                    let history_cmd = &command_history[history_index.unwrap()];

                    data_writer.write_all(b"\r").await.unwrap_or_default();
                    let mut output = Vec::with_capacity(history_cmd.len() + 3);
                    output.extend_from_slice(b"\x1b[2K");
                    output.extend_from_slice(history_cmd);
                    data_writer.write_all(&output).await.unwrap_or_default();
                    data_writer.flush().await.unwrap_or_default();

                    current_line.clear();
                    current_line.extend_from_slice(history_cmd);
                    *cursor_pos = current_line.len();
                } else {
                    data_writer.write_all(b"\x07").await.unwrap_or_default();
                    data_writer.flush().await.unwrap_or_default();
                }
            }
            b'B' => {
                if let Some(idx) = *history_index {
                    if idx < command_history.len() - 1 {
                        *history_index = Some(idx + 1);
                        let history_cmd = &command_history[history_index.unwrap()];

                        data_writer.write_all(b"\r").await.unwrap_or_default();
                        let mut output = Vec::with_capacity(history_cmd.len() + 3);
                        output.extend_from_slice(b"\x1b[2K");
                        output.extend_from_slice(history_cmd);
                        data_writer.write_all(&output).await.unwrap_or_default();
                        data_writer.flush().await.unwrap_or_default();

                        current_line.clear();
                        current_line.extend_from_slice(history_cmd);
                        *cursor_pos = current_line.len();
                    } else {
                        *history_index = None;
                        data_writer
                            .write_all(b"\r\x1b[2K")
                            .await
                            .unwrap_or_default();
                        data_writer.flush().await.unwrap_or_default();
                        current_line.clear();
                        *cursor_pos = 0;
                    }
                } else {
                    data_writer.write_all(b"\x07").await.unwrap_or_default();
                    data_writer.flush().await.unwrap_or_default();
                }
            }
            b'C' => {
                if *cursor_pos < current_line.len() {
                    *cursor_pos += 1;
                    data_writer.write_all(b"\x1b[C").await.unwrap_or_default();
                    data_writer.flush().await.unwrap_or_default();
                } else {
                    data_writer.write_all(b"\x07").await.unwrap_or_default();
                    data_writer.flush().await.unwrap_or_default();
                }
            }
            b'D' => {
                if *cursor_pos > 0 {
                    *cursor_pos -= 1;
                    data_writer.write_all(b"\x1b[D").await.unwrap_or_default();
                    data_writer.flush().await.unwrap_or_default();
                } else {
                    data_writer.write_all(b"\x07").await.unwrap_or_default();
                    data_writer.flush().await.unwrap_or_default();
                }
            }
            _ => {}
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn handle_input_byte(
        &mut self,
        byte: u8,
        current_line: &mut Vec<u8>,
        cursor_pos: &mut usize,
        command_history: &mut Vec<Vec<u8>>,
        history_index: &mut Option<usize>,
        data_writer: &mut Pin<Box<impl tokio::io::AsyncWrite>>,
        writer: &ChannelWriteHalf<Msg>,
    ) {
        match byte {
            b'\r' | b'\n' => {
                if !current_line.is_empty() {
                    let line = String::from_utf8_lossy(current_line);

                    if !command_history.is_empty()
                        && command_history.last().unwrap() != current_line
                    {
                        if command_history.len() >= 16 {
                            command_history.remove(0);
                        }
                        command_history.push(current_line.clone());
                    } else if command_history.is_empty() {
                        command_history.push(current_line.clone());
                    }
                    *history_index = None;

                    tracing::debug!(
                        server = %self.server.uuid,
                        "received command from shell: {}",
                        line
                    );

                    if line.starts_with("echo \"WinSCP: this is end-of-file:0\"") {
                        tracing::debug!(
                            server = %self.server.uuid,
                            "received WinSCP end-of-file command, switching to WinSCP mode"
                        );
                        self.mode = ShellMode::WinScp;
                    }

                    match self.mode {
                        ShellMode::Normal => {
                            if line.starts_with(&self.state.config.system.sftp.shell.cli.name) {
                                self.handle_cli_command(&line, data_writer).await;
                            } else if self.has_permission(Permission::ControlConsole).await {
                                if self.server.state.get_state()
                                    != crate::server::state::ServerState::Offline
                                    && let Some(stdin) = self.server.container_stdin().await
                                {
                                    if let Err(err) = stdin.send(format!("{line}\n")).await {
                                        data_writer.write_all(b"\r\n").await.unwrap_or_default();

                                        tracing::error!(
                                            server = %self.server.uuid,
                                            "failed to send command to server: {}",
                                            err
                                        );
                                    } else {
                                        data_writer.write_all(b"\r").await.unwrap_or_default();

                                        self.server
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
                                } else {
                                    let prelude = ansi_term::Color::Yellow
                                        .bold()
                                        .paint(format!("[{} Daemon]:", self.state.config.app_name));

                                    data_writer.write_all(b"\r\n").await.unwrap_or_default();
                                    data_writer
                                        .write_all(
                                            format!(
                                                "{prelude} The server is currently offline.\r\n\x1b[2K"
                                            )
                                            .as_bytes(),
                                        )
                                        .await
                                        .unwrap_or_default();
                                }
                            } else {
                                let prelude = ansi_term::Color::Yellow
                                    .bold()
                                    .paint(format!("[{} Daemon]:", self.state.config.app_name));

                                data_writer.write_all(b"\r\n").await.unwrap_or_default();
                                data_writer
                                    .write_all(format!("{prelude} You are missing the `control.console` permission to do this.\r\n\x1b[2K").as_bytes())
                                    .await
                                    .unwrap_or_default();
                            }
                        }
                        ShellMode::WinScp => {
                            let mut segments = line.split_whitespace();

                            if let Some("echo") = segments.next() {
                                let content = segments.collect::<Vec<&str>>().join(" ");

                                data_writer.write_all(b"\r\n").await.unwrap_or_default();
                                data_writer
                                    .write_all(content.trim_matches('"').as_bytes())
                                    .await
                                    .unwrap_or_default();
                                writer.exit_status(0).await.unwrap_or_default();
                            }
                        }
                    }

                    current_line.clear();
                    *cursor_pos = 0;
                }

                data_writer.flush().await.unwrap_or_default();
            }
            8 | 127 => {
                if *cursor_pos > 0 {
                    if *cursor_pos < current_line.len() {
                        current_line.remove(*cursor_pos - 1);
                        *cursor_pos -= 1;

                        data_writer.write_all(b"\x08").await.unwrap_or_default();
                        data_writer.write_all(b"\x1b[K").await.unwrap_or_default();
                        data_writer
                            .write_all(&current_line[*cursor_pos..])
                            .await
                            .unwrap_or_default();

                        if *cursor_pos < current_line.len() {
                            let move_back = current_line.len() - *cursor_pos;
                            data_writer
                                .write_all(format!("\x1b[{move_back}D").as_bytes())
                                .await
                                .unwrap_or_default();
                        }
                    } else {
                        current_line.pop();
                        *cursor_pos -= 1;
                        data_writer
                            .write_all(b"\x08 \x08")
                            .await
                            .unwrap_or_default();
                    }

                    data_writer.flush().await.unwrap_or_default();
                } else {
                    data_writer.write_all(b"\x07").await.unwrap_or_default();
                    data_writer.flush().await.unwrap_or_default();
                }
            }
            _ => {
                if current_line.len() < 1024 {
                    if *cursor_pos < current_line.len() {
                        current_line.insert(*cursor_pos, byte);
                        *cursor_pos += 1;

                        data_writer.write_all(&[byte]).await.unwrap_or_default();
                        data_writer
                            .write_all(&current_line[*cursor_pos..])
                            .await
                            .unwrap_or_default();

                        if *cursor_pos < current_line.len() {
                            let move_back = current_line.len() - *cursor_pos;
                            data_writer
                                .write_all(format!("\x1b[{move_back}D").as_bytes())
                                .await
                                .unwrap_or_default();
                        }
                    } else {
                        data_writer.write_all(&[byte]).await.unwrap_or_default();
                        current_line.push(byte);
                        *cursor_pos += 1;
                    }
                    data_writer.flush().await.unwrap_or_default();
                } else {
                    data_writer.write_all(b"\x07").await.unwrap_or_default();
                    data_writer.flush().await.unwrap_or_default();
                }
            }
        }
    }

    pub fn run(mut self, channel: Channel<Msg>) {
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

                writer
                    .make_writer()
                    .write_all(
                        format!(
                            "{prelude} Server marked as {}...\r\n\x1b[2K",
                            self.server.state.get_state().to_str()
                        )
                        .as_bytes(),
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
                Vec::with_capacity(2);

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

            let stdin_task = {
                let mut data_writer = writer.make_writer();

                Box::pin(async move {
                    let mut buffer = [0; 1024];
                    let mut current_line = Vec::with_capacity(1024);
                    let mut command_history: Vec<Vec<u8>> = Vec::with_capacity(16);
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
                                            self.handle_special_keys(
                                                byte,
                                                &mut current_line,
                                                &mut cursor_pos,
                                                &mut command_history,
                                                &mut history_index,
                                                &mut Box::pin(&mut data_writer),
                                            )
                                            .await;

                                            escape_sequence = false;
                                            cursor_sequence = false;
                                            sequence_buffer.clear();
                                        } else if sequence_buffer.len() >= 3
                                            || (!cursor_sequence && sequence_buffer.len() >= 2)
                                        {
                                            data_writer
                                                .write_all(b"\x1b")
                                                .await
                                                .unwrap_or_default();
                                            data_writer
                                                .write_all(&sequence_buffer)
                                                .await
                                                .unwrap_or_default();
                                            data_writer.flush().await.unwrap_or_default();
                                            escape_sequence = false;
                                            cursor_sequence = false;
                                            sequence_buffer.clear();
                                        }
                                    } else if byte == 0x1b {
                                        escape_sequence = true;
                                        sequence_buffer.clear();
                                    } else {
                                        self.handle_input_byte(
                                            byte,
                                            &mut current_line,
                                            &mut cursor_pos,
                                            &mut command_history,
                                            &mut history_index,
                                            &mut Box::pin(&mut data_writer),
                                            &writer,
                                        )
                                        .await;
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
            };

            tokio::select! {
                _ = stdin_task => {
                    tracing::debug!("shell stdin task finished");
                }
                _ = futures_util::future::join_all(futures) => {
                    tracing::debug!("shell handles finished");
                }
            }
        });
    }
}
