use crate::{
    io::compression::CompressionType,
    routes::State,
    server::{
        activity::{Activity, ActivityEvent},
        permissions::Permission,
    },
};
use russh::{Channel, server::Msg};
use serde_json::json;
use std::{
    net::IpAddr,
    path::{Path, PathBuf},
};
use tokio::io::AsyncWriteExt;

pub struct ExecSession {
    pub state: State,
    pub server: crate::server::Server,

    pub user_ip: Option<IpAddr>,
    pub user_uuid: uuid::Uuid,
}

impl ExecSession {
    #[inline]
    async fn has_permission(&self, permission: Permission) -> bool {
        self.server
            .user_permissions
            .has_permission(self.user_uuid, permission)
            .await
    }

    pub fn run(self, command: String, channel: Channel<Msg>) {
        tokio::spawn(async move {
            let run = async || -> Result<(), anyhow::Error> {
                channel.data(tokio::io::empty()).await?;

                let mut segments = command.split_whitespace();
                match segments.next() {
                    Some("tar") => {
                        if let Some("-xzpPf") = segments.next() {
                            if !self.has_permission(Permission::FileCreate).await {
                                channel
                                    .make_writer()
                                    .write_all(b"Permission denied.\r\n")
                                    .await?;
                                channel.exit_status(1).await?;
                                channel.close().await?;

                                return Ok(());
                            }

                            let mut path = String::new();
                            let mut destination = String::new();

                            let mut reached_destination = false;
                            for segment in segments {
                                if segment == "-C" {
                                    reached_destination = true;
                                    continue;
                                }

                                if reached_destination {
                                    destination.push_str(&segment.replace('\\', ""));
                                    destination.push(' ');
                                } else {
                                    path.push_str(&segment.replace('\\', ""));
                                    path.push(' ');
                                }
                            }

                            let archive = crate::server::filesystem::archive::Archive::open(
                                self.server.clone(),
                                PathBuf::from(path.trim()),
                            )
                            .await
                            .ok_or_else(|| {
                                anyhow::anyhow!("Failed to open archive at path: {}", path)
                            })?;

                            self.server
                                .activity
                                .log_activity(Activity {
                                    event: ActivityEvent::FileDecompress,
                                    user: Some(self.user_uuid),
                                    ip: self.user_ip,
                                    metadata: Some(json!({
                                        "directory": destination.trim(),
                                        "file": path.trim(),
                                    })),
                                    timestamp: chrono::Utc::now(),
                                })
                                .await;

                            archive
                                .extract(PathBuf::from(destination.trim()), None, None)
                                .await?;

                            channel.exit_status(0).await?;
                            channel.close().await?;

                            return Ok(());
                        }
                    }
                    Some("cd") => {
                        if let Some(base) = segments.next()
                            && segments.next() == Some("tar")
                            && segments.next().is_some()
                        {
                            if !self.has_permission(Permission::FileArchive).await {
                                channel
                                    .make_writer()
                                    .write_all(b"Permission denied.\r\n")
                                    .await?;
                                channel.exit_status(1).await?;
                                channel.close().await?;

                                return Ok(());
                            }

                            let base = Path::new(base.trim().trim_end_matches(';'));

                            let mut destination = String::new();
                            let mut path = String::new();
                            let mut paths = Vec::new();

                            let mut reached_path = false;
                            for segment in segments {
                                if reached_path {
                                    path.push_str(&segment.replace('\\', ""));
                                    path.push(' ');
                                } else {
                                    destination.push_str(&segment.replace('\\', ""));
                                    destination.push(' ');
                                }

                                if !segment.ends_with('\\') && !reached_path {
                                    reached_path = true;
                                } else if !segment.ends_with('\\') {
                                    paths.push(base.join(path.trim()));
                                    path.clear();
                                }
                            }

                            let destination = base.join(destination.trim());

                            self.server
                                .activity
                                .log_activity(Activity {
                                    event: ActivityEvent::FileCompress,
                                    user: Some(self.user_uuid),
                                    ip: self.user_ip,
                                    metadata: Some(json!({
                                        "files": paths.iter().map(|p| p.strip_prefix(base).unwrap_or(p).to_string_lossy().to_string()).collect::<Vec<_>>(),
                                        "directory": base.to_string_lossy(),
                                        "name": destination.to_string_lossy(),
                                    })),
                                    timestamp: chrono::Utc::now(),
                                })
                                .await;

                            let writer = tokio::task::spawn_blocking({
                                let server = self.server.clone();
                                let destination = destination.clone();

                                move || {
                                    crate::server::filesystem::writer::FileSystemWriter::new(
                                        server,
                                        &destination,
                                        None,
                                        None,
                                    )
                                }
                            })
                            .await??;
                            crate::server::filesystem::archive::create::create_tar(
                                self.server.filesystem.clone(),
                                writer,
                                base,
                                paths,
                                None,
                                vec![self.server.filesystem.get_ignored().await],
                                crate::server::filesystem::archive::create::CreateTarOptions {
                                    compression_type: match destination
                                        .extension()
                                        .and_then(|s| s.to_str())
                                    {
                                        Some("tar") => CompressionType::None,
                                        Some("gz") => CompressionType::Gz,
                                        Some("xz") => CompressionType::Xz,
                                        Some("bz2") => CompressionType::Bz2,
                                        Some("lz4") => CompressionType::Lz4,
                                        Some("zst") => CompressionType::Zstd,
                                        _ => {
                                            return Err(anyhow::anyhow!(
                                                "Unsupported archive format."
                                            ));
                                        }
                                    },
                                    compression_level: self
                                        .state
                                        .config
                                        .system
                                        .backups
                                        .compression_level,
                                    threads: self.state.config.api.file_compression_threads,
                                },
                            )
                            .await?;

                            channel
                                .make_writer()
                                .write_all(b"Archive created successfully.\r\n")
                                .await?;
                            channel.exit_status(0).await?;
                            channel.close().await?;

                            return Ok(());
                        }
                    }
                    _ => {}
                }

                if self.has_permission(Permission::ControlConsole).await {
                    if self.server.state.get_state() != crate::server::state::ServerState::Offline
                        && let Some(stdin) = self.server.container_stdin().await
                    {
                        if let Err(err) = stdin.send(format!("{command}\n")).await {
                            tracing::error!(
                                server = %self.server.uuid,
                                "failed to send command to server: {}",
                                err
                            );
                        } else {
                            self.server
                                .activity
                                .log_activity(Activity {
                                    event: ActivityEvent::ConsoleCommand,
                                    user: Some(self.user_uuid),
                                    ip: self.user_ip,
                                    metadata: Some(json!({
                                        "command": command,
                                    })),
                                    timestamp: chrono::Utc::now(),
                                })
                                .await;
                        }
                    } else {
                        channel
                            .make_writer()
                            .write_all(b"Server is not running.\r\n")
                            .await?;
                    }
                } else {
                    channel
                        .make_writer()
                        .write_all(b"Permission denied.\r\n")
                        .await?;
                }

                channel.exit_status(0).await?;
                channel.close().await?;

                Ok(())
            };

            if let Err(err) = run().await {
                tracing::error!(
                    server = %self.server.uuid,
                    "failed to execute command: {}",
                    err
                );

                channel.exit_status(1).await.unwrap_or_default();
                channel.close().await.unwrap_or_default();
            }
        });
    }
}
