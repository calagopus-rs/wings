use crate::{
    io::compression::CompressionType,
    routes::State,
    server::activity::{Activity, ActivityEvent},
};
use cap_std::fs::OpenOptions;
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio::io::AsyncWriteExt;

#[derive(Clone, Deserialize, Serialize)]
pub struct RenameFile {
    pub from: String,
    pub to: String,
}

#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ArchiveFormat {
    Tar,
    TarGz,
    TarXz,
    TarBz2,
    TarLz4,
    TarZstd,
    Zip,
    SevenZip,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum ScheduleAction {
    Sleep {
        duration: u64,
    },
    SendPower {
        ignore_failure: bool,

        action: crate::models::ServerPowerAction,
    },
    SendCommand {
        ignore_failure: bool,

        command: String,
    },
    CreateBackup {
        ignore_failure: bool,
        foreground: bool,

        name: Option<String>,
        ignored_files: Vec<String>,
    },
    CreateDirectory {
        ignore_failure: bool,

        root: String,
        name: String,
    },
    WriteFile {
        ignore_failure: bool,
        append: bool,

        file: String,
        content: String,
    },
    CopyFile {
        ignore_failure: bool,
        foreground: bool,

        file: String,
        destination: String,
    },
    DeleteFiles {
        root: String,
        files: Vec<String>,
    },
    RenameFiles {
        root: String,
        files: Vec<RenameFile>,
    },
    CompressFiles {
        ignore_failure: bool,
        foreground: bool,

        root: String,
        files: Vec<String>,
        format: ArchiveFormat,
        name: String,
    },
    DecompressFile {
        ignore_failure: bool,
        foreground: bool,

        root: String,
        file: String,
    },
    UpdateStartupVariable {
        ignore_failure: bool,

        env_variable: String,
        value: String,
    },
    UpdateStartupCommand {
        ignore_failure: bool,

        command: String,
    },
    UpdateStartupDockerImage {
        ignore_failure: bool,

        image: String,
    },
}

impl ScheduleAction {
    #[inline]
    pub fn ignore_failure(&self) -> bool {
        match self {
            ScheduleAction::Sleep { .. } => false,
            ScheduleAction::SendPower { ignore_failure, .. } => *ignore_failure,
            ScheduleAction::SendCommand { ignore_failure, .. } => *ignore_failure,
            ScheduleAction::CreateBackup { ignore_failure, .. } => *ignore_failure,
            ScheduleAction::CreateDirectory { ignore_failure, .. } => *ignore_failure,
            ScheduleAction::WriteFile { ignore_failure, .. } => *ignore_failure,
            ScheduleAction::CopyFile { ignore_failure, .. } => *ignore_failure,
            ScheduleAction::DeleteFiles { .. } => false,
            ScheduleAction::RenameFiles { .. } => false,
            ScheduleAction::CompressFiles { ignore_failure, .. } => *ignore_failure,
            ScheduleAction::DecompressFile { ignore_failure, .. } => *ignore_failure,
            ScheduleAction::UpdateStartupVariable { ignore_failure, .. } => *ignore_failure,
            ScheduleAction::UpdateStartupCommand { ignore_failure, .. } => *ignore_failure,
            ScheduleAction::UpdateStartupDockerImage { ignore_failure, .. } => *ignore_failure,
        }
    }

    pub async fn execute(
        &self,
        state: &State,
        server: &crate::server::Server,
    ) -> Result<(), String> {
        if server.is_locked_state() {
            return Err("server is in a locked state.".into());
        }

        match self {
            ScheduleAction::Sleep { duration } => {
                tokio::time::sleep(std::time::Duration::from_millis(*duration)).await;
            }
            ScheduleAction::SendPower { action, .. } => match action {
                crate::models::ServerPowerAction::Start => {
                    if server.state.get_state() != crate::server::state::ServerState::Offline {
                        return Err("server is already running or starting.".into());
                    }

                    if let Err(err) = server.start(None, false).await {
                        match err.downcast::<&str>() {
                            Ok(message) => {
                                let mut message = message.to_string();
                                message.make_ascii_lowercase();
                                return Err(message);
                            }
                            Err(err) => {
                                tracing::error!(
                                    server = %server.uuid,
                                    "failed to start server: {:#?}",
                                    err,
                                );

                                return Err(
                                    "an unexpected error occurred while starting the server."
                                        .into(),
                                );
                            }
                        }
                    } else {
                        server
                            .activity
                            .log_activity(Activity {
                                event: ActivityEvent::PowerStart,
                                user: None,
                                ip: None,
                                metadata: None,
                                timestamp: chrono::Utc::now(),
                            })
                            .await;
                    }
                }
                crate::models::ServerPowerAction::Restart => {
                    if server.restarting.load(std::sync::atomic::Ordering::SeqCst) {
                        return Err("server is already restarting.".into());
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
                                let mut message = message.to_string();
                                message.make_ascii_lowercase();
                                return Err(message);
                            }
                            Err(err) => {
                                tracing::error!(
                                    server = %server.uuid,
                                    "failed to restart server: {:#?}",
                                    err
                                );

                                return Err(
                                    "an unexpected error occurred while restarting the server."
                                        .into(),
                                );
                            }
                        }
                    } else {
                        server
                            .activity
                            .log_activity(Activity {
                                event: ActivityEvent::PowerRestart,
                                user: None,
                                ip: None,
                                metadata: None,
                                timestamp: chrono::Utc::now(),
                            })
                            .await;
                    }
                }
                crate::models::ServerPowerAction::Stop => {
                    if matches!(
                        server.state.get_state(),
                        crate::server::state::ServerState::Offline
                            | crate::server::state::ServerState::Stopping
                    ) {
                        return Err("server is already offline or stopping.".into());
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
                                let mut message = message.to_string();
                                message.make_ascii_lowercase();
                                return Err(message);
                            }
                            Err(err) => {
                                tracing::error!(
                                    server = %server.uuid,
                                    "failed to stop server: {:#?}",
                                    err
                                );

                                return Err(
                                    "an unexpected error occurred while stopping the server."
                                        .into(),
                                );
                            }
                        }
                    } else {
                        server
                            .activity
                            .log_activity(Activity {
                                event: ActivityEvent::PowerStop,
                                user: None,
                                ip: None,
                                metadata: None,
                                timestamp: chrono::Utc::now(),
                            })
                            .await;
                    }
                }
                crate::models::ServerPowerAction::Kill => {
                    if server.state.get_state() == crate::server::state::ServerState::Offline {
                        return Err("server is already offline.".into());
                    }

                    if let Err(err) = server.kill(false).await {
                        tracing::error!(
                            server = %server.uuid,
                            "failed to kill server: {:#?}",
                            err
                        );

                        return Err("an unexpected error occurred while killing the server.".into());
                    } else {
                        server
                            .activity
                            .log_activity(Activity {
                                event: ActivityEvent::PowerKill,
                                user: None,
                                ip: None,
                                metadata: None,
                                timestamp: chrono::Utc::now(),
                            })
                            .await;
                    }
                }
            },
            ScheduleAction::SendCommand { command, .. } => {
                if server.state.get_state() == crate::server::state::ServerState::Offline {
                    return Err("server is not running.".into());
                }

                if let Some(stdin) = server.container_stdin().await {
                    if stdin.send(format!("{}\n", command)).await.is_ok() {
                        server
                            .activity
                            .log_activity(Activity {
                                event: ActivityEvent::ConsoleCommand,
                                user: None,
                                ip: None,
                                metadata: Some(serde_json::json!({
                                    "command": command,
                                })),
                                timestamp: chrono::Utc::now(),
                            })
                            .await;
                    }
                } else {
                    return Err("failed to get stdin (is server offline?)".into());
                }
            }
            ScheduleAction::CreateBackup {
                foreground,
                name,
                ignored_files,
                ..
            } => {
                let (adapter, uuid) = match state
                    .config
                    .client
                    .create_backup(server.uuid, name.as_deref(), ignored_files)
                    .await
                {
                    Ok(result) => result,
                    Err(err) => {
                        tracing::error!(
                            server = %server.uuid,
                            "failed to create backup: {:#?}",
                            err
                        );

                        return Err("failed to create backup".into());
                    }
                };

                if state.backup_manager.fast_contains(server, uuid).await {
                    return Err("backup already exists".into());
                }

                let thread = tokio::spawn({
                    let state = Arc::clone(state);
                    let ignored_files = ignored_files.join("\n");
                    let server = server.clone();

                    async move {
                        if let Err(err) = state
                            .backup_manager
                            .create(adapter, &server, uuid, ignored_files)
                            .await
                        {
                            tracing::error!(
                                "failed to create backup {} (adapter = {:?}) for {}: {}",
                                uuid,
                                adapter,
                                server.uuid,
                                err
                            );

                            return Err("failed to create backup".into());
                        }

                        Ok::<_, String>(())
                    }
                });

                if *foreground && let Ok(Err(err)) = thread.await {
                    return Err(err);
                }
            }
            ScheduleAction::CreateDirectory { root, name, .. } => {
                let path = match server.filesystem.async_canonicalize(root).await {
                    Ok(path) => path,
                    Err(_) => PathBuf::from(root),
                };

                let metadata = server.filesystem.async_metadata(&root).await;
                if !metadata.map(|m| m.is_dir()).unwrap_or(true) {
                    return Err("root is not a directory".into());
                }

                if server.filesystem.is_ignored(&path, true).await {
                    return Err("root not found".into());
                }

                let destination = path.join(name);

                if server.filesystem.is_ignored(&destination, true).await {
                    return Err("destination not found".into());
                }

                if let Err(err) = server.filesystem.async_create_dir_all(&destination).await {
                    tracing::error!(path = %destination.display(), "failed to create directory: {:#?}", err);

                    return Err("failed to create directory".into());
                }

                server
                    .activity
                    .log_activity(Activity {
                        event: ActivityEvent::FileCreateDirectory,
                        user: None,
                        ip: None,
                        metadata: Some(serde_json::json!({
                            "directory": root,
                            "name": name,
                        })),
                        timestamp: chrono::Utc::now(),
                    })
                    .await;

                if let Err(err) = server.filesystem.chown_path(&destination).await {
                    tracing::error!(path = %destination.display(), "failed to change ownership: {:#?}", err);

                    return Err("failed to change ownership".into());
                }
            }
            ScheduleAction::WriteFile {
                file: file_path,
                content,
                append,
                ..
            } => {
                let path = match server.filesystem.async_canonicalize(file_path).await {
                    Ok(path) => path,
                    Err(_) => PathBuf::from(file_path),
                };

                let metadata = server.filesystem.async_metadata(&path).await;

                if server
                    .filesystem
                    .is_ignored(
                        &path,
                        metadata.as_ref().map(|m| m.is_dir()).unwrap_or(false),
                    )
                    .await
                {
                    return Err("file not found".into());
                }

                let old_content_size = if let Ok(metadata) = metadata {
                    if !metadata.is_file() {
                        return Err("file is not a file".into());
                    }

                    metadata.len() as i64
                } else {
                    0
                };

                let parent = match path.parent() {
                    Some(parent) => parent,
                    None => {
                        return Err("file has no parent".into());
                    }
                };

                if server.filesystem.is_ignored(parent, true).await {
                    return Err("parent directory not found".into());
                }

                if let Err(err) = server.filesystem.async_create_dir_all(parent).await {
                    tracing::error!(path = %parent.display(), "failed to create parent directory: {:#?}", err);

                    return Err("failed to create parent directory".into());
                }

                let added_content_size = if *append {
                    content.len() as i64
                } else {
                    content.len() as i64 - old_content_size
                };
                if !server
                    .filesystem
                    .async_allocate_in_path(parent, added_content_size, false)
                    .await
                {
                    return Err("failed to allocate space".into());
                }

                let mut options = OpenOptions::new();
                options
                    .write(true)
                    .create(true)
                    .truncate(!*append)
                    .append(*append);

                let mut file = match server.filesystem.async_open_with(&path, options).await {
                    Ok(file) => file,
                    Err(err) => {
                        tracing::error!(path = %path.display(), "failed to open file: {:#?}", err);
                        return Err("failed to open file".into());
                    }
                };

                if let Err(err) = file.write_all(content.as_bytes()).await {
                    tracing::error!(path = %path.display(), "failed to write file: {:#?}", err);
                    return Err("failed to write file".into());
                }
                if let Err(err) = file.flush().await {
                    tracing::error!(path = %path.display(), "failed to flush file: {:#?}", err);
                    return Err("failed to flush file".into());
                }
                if let Err(err) = file.sync_all().await {
                    tracing::error!(path = %path.display(), "failed to sync file: {:#?}", err);
                    return Err("failed to sync file".into());
                }

                server
                    .activity
                    .log_activity(Activity {
                        event: ActivityEvent::FileWrite,
                        user: None,
                        ip: None,
                        metadata: Some(serde_json::json!({
                            "file": file_path,
                        })),
                        timestamp: chrono::Utc::now(),
                    })
                    .await;

                if let Err(err) = server.filesystem.chown_path(&path).await {
                    tracing::error!(path = %path.display(), "failed to change ownership: {:#?}", err);

                    return Err("failed to change ownership".into());
                }
            }
            ScheduleAction::CopyFile {
                foreground,
                file,
                destination,
                ..
            } => {
                let location = match server.filesystem.async_canonicalize(file).await {
                    Ok(path) => path,
                    Err(_) => return Err("file not found".into()),
                };

                let metadata = match server.filesystem.async_metadata(&location).await {
                    Ok(metadata) => {
                        if !metadata.is_file()
                            || server
                                .filesystem
                                .is_ignored(&location, metadata.is_dir())
                                .await
                        {
                            return Err("file not found".into());
                        } else {
                            metadata
                        }
                    }
                    Err(_) => {
                        return Err("file not found".into());
                    }
                };

                let parent = match location.parent() {
                    Some(parent) => parent,
                    None => {
                        return Err("file has no parent".into());
                    }
                };

                if server.filesystem.is_ignored(parent, true).await {
                    return Err("parent directory not found".into());
                }

                let file_name = parent.join(destination);

                if !server
                    .filesystem
                    .async_allocate_in_path(parent, metadata.len() as i64, false)
                    .await
                {
                    return Err("failed to allocate space".into());
                }

                let thread = tokio::spawn({
                    let file_name = file_name.clone();
                    let server = server.clone();

                    async move {
                        server
                            .filesystem
                            .async_copy(&location, &server.filesystem, &file_name)
                            .await
                    }
                });

                server
                    .activity
                    .log_activity(Activity {
                        event: ActivityEvent::FileCopy,
                        user: None,
                        ip: None,
                        metadata: Some(serde_json::json!({
                            "file": file,
                            "name": destination,
                        })),
                        timestamp: chrono::Utc::now(),
                    })
                    .await;

                if *foreground && let Ok(Err(err)) = thread.await {
                    tracing::error!(path = %file_name.display(), "failed to copy file: {:#?}", err);

                    return Err("failed to copy file".into());
                }
            }
            ScheduleAction::DeleteFiles { root, files } => {
                let root = match server.filesystem.async_canonicalize(root).await {
                    Ok(path) => path,
                    Err(_) => {
                        return Err("root not found".into());
                    }
                };

                let metadata = server.filesystem.async_symlink_metadata(&root).await;
                if !metadata.map(|m| m.is_dir()).unwrap_or(false) {
                    return Err("root is not a directory".into());
                }

                for file in files {
                    let destination = root.join(file);
                    if destination == root {
                        continue;
                    }

                    if server
                        .filesystem
                        .is_ignored(
                            &destination,
                            server
                                .filesystem
                                .async_symlink_metadata(&destination)
                                .await
                                .is_ok_and(|m| m.is_dir()),
                        )
                        .await
                    {
                        continue;
                    }

                    server.filesystem.truncate_path(&destination).await.ok();
                }

                server
                    .activity
                    .log_activity(Activity {
                        event: ActivityEvent::FileDelete,
                        user: None,
                        ip: None,
                        metadata: Some(serde_json::json!({
                            "directory": root,
                            "files": files,
                        })),
                        timestamp: chrono::Utc::now(),
                    })
                    .await;
            }
            ScheduleAction::RenameFiles { root, files } => {
                let root = Path::new(root);

                let metadata = server.filesystem.async_metadata(&root).await;
                if !metadata.map(|m| m.is_dir()).unwrap_or(true) {
                    return Err("root is not a directory".into());
                }

                for file in files {
                    let from = root.join(&file.from);
                    if from == root {
                        continue;
                    }

                    let to = root.join(&file.to);
                    if to == root {
                        continue;
                    }

                    if from == to {
                        continue;
                    }

                    let from_metadata = match server.filesystem.async_metadata(&from).await {
                        Ok(metadata) => metadata,
                        Err(_) => continue,
                    };

                    if server.filesystem.async_metadata(&to).await.is_ok()
                        || server
                            .filesystem
                            .is_ignored(&from, from_metadata.is_dir())
                            .await
                        || server
                            .filesystem
                            .is_ignored(&to, from_metadata.is_dir())
                            .await
                    {
                        continue;
                    }

                    if let Err(err) = server.filesystem.rename_path(from, to).await {
                        tracing::debug!(
                            server = %server.uuid,
                            "failed to rename file: {:#?}",
                            err
                        );
                    }
                }

                server
                    .activity
                    .log_activity(Activity {
                        event: ActivityEvent::FileRename,
                        user: None,
                        ip: None,
                        metadata: Some(serde_json::json!({
                            "directory": root,
                            "files": files,
                        })),
                        timestamp: chrono::Utc::now(),
                    })
                    .await;
            }
            ScheduleAction::CompressFiles {
                foreground,
                root,
                files,
                format,
                name,
                ..
            } => {
                let root = match server.filesystem.async_canonicalize(root).await {
                    Ok(path) => path,
                    Err(_) => {
                        return Err("root not found".into());
                    }
                };

                let metadata = server.filesystem.async_symlink_metadata(&root).await;
                if !metadata.map(|m| m.is_dir()).unwrap_or(true) {
                    return Err("root is not a directory".into());
                }

                let file_name = root.join(name);

                if server.filesystem.is_ignored(&file_name, false).await {
                    return Err("file not found".into());
                }

                let thread = tokio::spawn({
                    let root = root.clone();
                    let files = files.clone();
                    let file_name = file_name.clone();
                    let server = server.clone();
                    let format = *format;

                    async move {
                        let ignored = server.filesystem.get_ignored().await;
                        let writer = tokio::task::spawn_blocking({
                            let server = server.clone();

                            move || {
                                crate::server::filesystem::writer::FileSystemWriter::new(
                                    server, &file_name, None, None,
                                )
                            }
                        })
                        .await??;

                        match format {
                            ArchiveFormat::Tar
                            | ArchiveFormat::TarGz
                            | ArchiveFormat::TarXz
                            | ArchiveFormat::TarBz2
                            | ArchiveFormat::TarLz4
                            | ArchiveFormat::TarZstd => {
                                crate::server::filesystem::archive::create::create_tar(
                                    server.filesystem.clone(),
                                    writer,
                                    &root,
                                    files.into_iter().map(PathBuf::from).collect(),
                                    None,
                                    vec![ignored],
                                    crate::server::filesystem::archive::create::CreateTarOptions {
                                        compression_type: match format {
                                            ArchiveFormat::Tar => CompressionType::None,
                                            ArchiveFormat::TarGz => CompressionType::Gz,
                                            ArchiveFormat::TarXz => CompressionType::Xz,
                                            ArchiveFormat::TarBz2 => CompressionType::Bz2,
                                            ArchiveFormat::TarLz4 => CompressionType::Lz4,
                                            ArchiveFormat::TarZstd => CompressionType::Zstd,
                                            _ => unreachable!(),
                                        },
                                        compression_level: server
                                            .app_state
                                            .config
                                            .system
                                            .backups
                                            .compression_level,
                                        threads: server
                                            .app_state
                                            .config
                                            .api
                                            .file_compression_threads,
                                    },
                                )
                                .await
                            }
                            ArchiveFormat::Zip => {
                                crate::server::filesystem::archive::create::create_zip(
                                    server.filesystem.clone(),
                                    writer,
                                    &root,
                                    files.into_iter().map(PathBuf::from).collect(),
                                    None,
                                    vec![ignored],
                                    crate::server::filesystem::archive::create::CreateZipOptions {
                                        compression_level: server
                                            .app_state
                                            .config
                                            .system
                                            .backups
                                            .compression_level,
                                    },
                                )
                                .await
                            }
                            ArchiveFormat::SevenZip => {
                                crate::server::filesystem::archive::create::create_7z(
                                    server.filesystem.clone(),
                                    writer,
                                    &root,
                                    files.into_iter().map(PathBuf::from).collect(),
                                    None,
                                    vec![ignored],
                                    crate::server::filesystem::archive::create::Create7zOptions {},
                                )
                                .await
                            }
                        }
                    }
                });

                server
                    .activity
                    .log_activity(Activity {
                        event: ActivityEvent::FileCompress,
                        user: None,
                        ip: None,
                        metadata: Some(serde_json::json!({
                            "directory": root.display().to_string(),
                            "name": name,
                            "files": files,
                        })),
                        timestamp: chrono::Utc::now(),
                    })
                    .await;

                if *foreground && let Ok(Err(err)) = thread.await {
                    tracing::error!(path = %file_name.display(), "failed to compress files: {:#?}", err);

                    return Err("failed to compress files".into());
                }
            }
            ScheduleAction::DecompressFile {
                foreground,
                root,
                file,
                ..
            } => {
                let root = match server.filesystem.async_canonicalize(root).await {
                    Ok(path) => path,
                    Err(_) => {
                        return Err("root not found".into());
                    }
                };

                let metadata = server.filesystem.async_metadata(&root).await;
                if !metadata.map(|m| m.is_dir()).unwrap_or(true) {
                    return Err("root is not a directory".into());
                }

                let source = root.join(file);

                if server
                    .filesystem
                    .is_ignored(
                        &source,
                        server
                            .filesystem
                            .async_metadata(&source)
                            .await
                            .is_ok_and(|m| m.is_dir()),
                    )
                    .await
                {
                    return Err("file not found".into());
                }

                let archive = match crate::server::filesystem::archive::Archive::open(
                    server.clone(),
                    source.clone(),
                )
                .await
                {
                    Some(archive) => archive,
                    None => {
                        return Err("failed to open archive".into());
                    }
                };

                let thread = tokio::spawn(archive.extract(root.clone(), None, None));

                server
                    .activity
                    .log_activity(Activity {
                        event: ActivityEvent::FileDecompress,
                        user: None,
                        ip: None,
                        metadata: Some(serde_json::json!({
                            "directory": root.display().to_string(),
                            "file": file,
                        })),
                        timestamp: chrono::Utc::now(),
                    })
                    .await;

                if *foreground && let Ok(Err(err)) = thread.await {
                    tracing::error!(path = %source.display(), "failed to decompress file: {:#?}", err);

                    return Err("failed to decompress file".into());
                }
            }
            ScheduleAction::UpdateStartupVariable {
                env_variable,
                value,
                ..
            } => {
                match state
                    .config
                    .client
                    .set_server_startup_variable(server.uuid, env_variable, value)
                    .await
                {
                    Ok(()) => {}
                    Err(err) => {
                        tracing::error!(
                            server = %server.uuid,
                            "failed to set server startup variable: {:#?}",
                            err
                        );

                        return Err("failed to set server startup variable".into());
                    }
                };
            }
            ScheduleAction::UpdateStartupCommand { command, .. } => {
                match state
                    .config
                    .client
                    .set_server_startup_command(server.uuid, command)
                    .await
                {
                    Ok(()) => {}
                    Err(err) => {
                        tracing::error!(
                            server = %server.uuid,
                            "failed to set server startup command: {:#?}",
                            err
                        );

                        return Err("failed to set server startup command".into());
                    }
                };
            }
            ScheduleAction::UpdateStartupDockerImage { image, .. } => {
                match state
                    .config
                    .client
                    .set_server_startup_docker_image(server.uuid, image)
                    .await
                {
                    Ok(()) => {}
                    Err(err) => {
                        tracing::error!(
                            server = %server.uuid,
                            "failed to set server startup docker image: {:#?}",
                            err
                        );

                        return Err("failed to set server startup docker image".into());
                    }
                };
            }
        }

        Ok(())
    }
}
