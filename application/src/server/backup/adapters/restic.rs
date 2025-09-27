use crate::{
    io::compression::writer::CompressionWriter,
    models::DirectoryEntry,
    remote::backups::{RawServerBackup, ResticBackupConfiguration},
    response::ApiResponse,
    server::{
        backup::{
            Backup, BackupBrowseExt, BackupCleanExt, BackupCreateExt, BackupExt, BackupFindExt,
            BrowseBackup,
        },
        filesystem::archive::StreamableArchiveFormat,
    },
};
use axum::{body::Body, http::HeaderMap};
use chrono::{Datelike, Timelike};
use human_bytes::human_bytes;
use serde::Deserialize;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        Arc, LazyLock,
        atomic::{AtomicU64, Ordering},
    },
};
use tokio::{io::AsyncBufReadExt, process::Command, sync::RwLock};

type ResticBackupCache =
    RwLock<HashMap<uuid::Uuid, (ResticSnapshot, Arc<ResticBackupConfiguration>)>>;
static RESTIC_BACKUP_CACHE: LazyLock<ResticBackupCache> =
    LazyLock::new(|| RwLock::new(HashMap::new()));

#[derive(Debug, Deserialize)]
struct ResticSnapshot {
    short_id: String,
    tags: Vec<String>,
    paths: Vec<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum ResticEntryType {
    File,
    Dir,
    Symlink,
}

#[derive(Deserialize)]
struct ResticDirectoryEntry {
    r#type: ResticEntryType,
    path: PathBuf,
    mode: u32,
    size: Option<u64>,
    mtime: chrono::DateTime<chrono::Utc>,
}

pub struct ResticBackup {
    uuid: uuid::Uuid,
    short_id: String,

    config: Arc<crate::config::Config>,
    server_path: PathBuf,
    configuration: Arc<ResticBackupConfiguration>,
}

#[async_trait::async_trait]
impl BackupFindExt for ResticBackup {
    async fn exists(
        config: &Arc<crate::config::Config>,
        uuid: uuid::Uuid,
    ) -> Result<bool, anyhow::Error> {
        if RESTIC_BACKUP_CACHE.read().await.contains_key(&uuid) {
            return Ok(true);
        }

        if tokio::fs::metadata(&config.system.backups.restic.password_file)
            .await
            .is_ok()
        {
            let output = match Command::new("restic")
                .envs(&config.system.backups.restic.environment)
                .arg("--json")
                .arg("--no-lock")
                .arg("--repo")
                .arg(&config.system.backups.restic.repository)
                .arg("--password-file")
                .arg(&config.system.backups.restic.password_file)
                .arg("snapshots")
                .output()
                .await
            {
                Ok(output) => output,
                Err(err) => {
                    return Err(anyhow::anyhow!(
                        "failed to check restic backup existence: {:#?}",
                        err
                    ));
                }
            };

            if output.status.success() {
                let snapshots: Vec<ResticSnapshot> =
                    serde_json::from_slice(&output.stdout).unwrap_or_default();
                let configuration = Arc::new(ResticBackupConfiguration {
                    repository: config.system.backups.restic.repository.clone(),
                    password_file: Some(config.system.backups.restic.password_file.clone()),
                    retry_lock_seconds: config.system.backups.restic.retry_lock_seconds,
                    environment: config.system.backups.restic.environment.clone(),
                });

                let mut found = false;
                let mut cache = RESTIC_BACKUP_CACHE.write().await;
                for snapshot in snapshots {
                    let snapshot_uuid = match snapshot.tags.first() {
                        Some(tag) => match uuid::Uuid::parse_str(tag) {
                            Ok(uuid) => uuid,
                            Err(_) => continue,
                        },
                        _ => continue,
                    };

                    if snapshot_uuid == uuid {
                        found = true;
                    }

                    cache.insert(snapshot_uuid, (snapshot, Arc::clone(&configuration)));
                }
                drop(cache);

                if found {
                    return Ok(true);
                }
            }
        }

        if let Ok(configuration) = config.client.backup_restic_configuration(uuid).await {
            let output = match Command::new("restic")
                .envs(&configuration.environment)
                .arg("--json")
                .arg("--no-lock")
                .arg("--repo")
                .arg(&configuration.repository)
                .arg("snapshots")
                .output()
                .await
            {
                Ok(output) => output,
                Err(err) => {
                    return Err(anyhow::anyhow!(
                        "failed to check restic backup existence: {:#?}",
                        err
                    ));
                }
            };

            if output.status.success() {
                let snapshots: Vec<ResticSnapshot> =
                    serde_json::from_slice(&output.stdout).unwrap_or_default();
                let configuration = Arc::new(configuration.clone());

                let mut found = false;
                let mut cache = RESTIC_BACKUP_CACHE.write().await;
                for snapshot in snapshots {
                    let snapshot_uuid = match snapshot.tags.first() {
                        Some(tag) => match uuid::Uuid::parse_str(tag) {
                            Ok(uuid) => uuid,
                            Err(_) => continue,
                        },
                        _ => continue,
                    };

                    if snapshot_uuid == uuid {
                        found = true;
                    }

                    cache.insert(snapshot_uuid, (snapshot, Arc::clone(&configuration)));
                }
                drop(cache);

                if found {
                    return Ok(true);
                }
            }
        }

        Ok(false)
    }

    async fn find(
        config: &Arc<crate::config::Config>,
        uuid: uuid::Uuid,
    ) -> Result<Option<Backup>, anyhow::Error> {
        if let Some((snapshot, configuration)) = RESTIC_BACKUP_CACHE.read().await.get(&uuid) {
            return Ok(Some(Backup::Restic(ResticBackup {
                uuid,
                short_id: snapshot.short_id.clone(),
                config: Arc::clone(config),
                server_path: match snapshot.paths.first() {
                    Some(path) => PathBuf::from(path),
                    None => {
                        return Err(anyhow::anyhow!(
                            "no paths found in restic snapshot for uuid: {}",
                            uuid
                        ));
                    }
                },
                configuration: Arc::clone(configuration),
            })));
        }

        if tokio::fs::metadata(&config.system.backups.restic.password_file)
            .await
            .is_ok()
        {
            let output = match Command::new("restic")
                .envs(&config.system.backups.restic.environment)
                .arg("--json")
                .arg("--no-lock")
                .arg("--repo")
                .arg(&config.system.backups.restic.repository)
                .arg("--password-file")
                .arg(&config.system.backups.restic.password_file)
                .arg("snapshots")
                .output()
                .await
            {
                Ok(output) => output,
                Err(err) => {
                    return Err(anyhow::anyhow!("failed to find restic backup: {:#?}", err));
                }
            };

            if output.status.success() {
                let snapshots: Vec<ResticSnapshot> =
                    serde_json::from_slice(&output.stdout).unwrap_or_default();
                let configuration = Arc::new(ResticBackupConfiguration {
                    repository: config.system.backups.restic.repository.clone(),
                    password_file: Some(config.system.backups.restic.password_file.clone()),
                    retry_lock_seconds: config.system.backups.restic.retry_lock_seconds,
                    environment: config.system.backups.restic.environment.clone(),
                });

                let mut backup = None;
                let mut cache = RESTIC_BACKUP_CACHE.write().await;
                for snapshot in snapshots {
                    let snapshot_uuid = match snapshot.tags.first() {
                        Some(tag) => match uuid::Uuid::parse_str(tag) {
                            Ok(uuid) => uuid,
                            Err(_) => continue,
                        },
                        _ => continue,
                    };

                    if snapshot_uuid == uuid {
                        backup = Some(ResticBackup {
                            uuid,
                            short_id: snapshot.short_id.clone(),
                            config: Arc::clone(config),
                            server_path: match snapshot.paths.first() {
                                Some(path) => PathBuf::from(path),
                                None => {
                                    return Err(anyhow::anyhow!(
                                        "no paths found in restic snapshot for uuid: {}",
                                        uuid
                                    ));
                                }
                            },
                            configuration: Arc::clone(&configuration),
                        });
                    }

                    cache.insert(snapshot_uuid, (snapshot, Arc::clone(&configuration)));
                }
                drop(cache);

                if let Some(backup) = backup {
                    return Ok(Some(Backup::Restic(backup)));
                }
            }
        }

        if let Ok(configuration) = config.client.backup_restic_configuration(uuid).await {
            let output = match Command::new("restic")
                .envs(&configuration.environment)
                .arg("--json")
                .arg("--no-lock")
                .arg("--repo")
                .arg(&configuration.repository)
                .arg("snapshots")
                .output()
                .await
            {
                Ok(output) => output,
                Err(err) => {
                    return Err(anyhow::anyhow!("failed to find restic backup: {:#?}", err));
                }
            };

            if output.status.success() {
                let snapshots: Vec<ResticSnapshot> =
                    serde_json::from_slice(&output.stdout).unwrap_or_default();
                let configuration = Arc::new(configuration.clone());

                let mut backup = None;
                let mut cache = RESTIC_BACKUP_CACHE.write().await;
                for snapshot in snapshots {
                    let snapshot_uuid = match snapshot.tags.first() {
                        Some(tag) => match uuid::Uuid::parse_str(tag) {
                            Ok(uuid) => uuid,
                            Err(_) => continue,
                        },
                        _ => continue,
                    };

                    if snapshot_uuid == uuid {
                        backup = Some(ResticBackup {
                            uuid,
                            short_id: snapshot.short_id.clone(),
                            config: Arc::clone(config),
                            server_path: match snapshot.paths.first() {
                                Some(path) => PathBuf::from(path),
                                None => {
                                    return Err(anyhow::anyhow!(
                                        "no paths found in restic snapshot for uuid: {}",
                                        uuid
                                    ));
                                }
                            },
                            configuration: Arc::clone(&configuration),
                        });
                    }

                    cache.insert(snapshot_uuid, (snapshot, Arc::clone(&configuration)));
                }
                drop(cache);

                if let Some(backup) = backup {
                    return Ok(Some(Backup::Restic(backup)));
                }
            }
        }

        Ok(None)
    }
}

#[async_trait::async_trait]
impl BackupCreateExt for ResticBackup {
    async fn create(
        server: &crate::server::Server,
        uuid: uuid::Uuid,
        progress: Arc<AtomicU64>,
        total: Arc<AtomicU64>,
        _ignore: ignore::gitignore::Gitignore,
        ignore_raw: String,
    ) -> Result<RawServerBackup, anyhow::Error> {
        let mut excluded_paths = Vec::new();
        for line in ignore_raw.lines() {
            excluded_paths.push("--exclude");
            excluded_paths.push(line);
        }

        let (mut child, configuration) =
            if tokio::fs::metadata(&server.app_state.config.system.backups.restic.password_file)
                .await
                .is_ok()
            {
                (
                    Command::new("restic")
                        .envs(&server.app_state.config.system.backups.restic.environment)
                        .arg("--json")
                        .arg("--repo")
                        .arg(&server.app_state.config.system.backups.restic.repository)
                        .arg("--password-file")
                        .arg(&server.app_state.config.system.backups.restic.password_file)
                        .arg("--retry-lock")
                        .arg(format!(
                            "{}s",
                            server
                                .app_state
                                .config
                                .system
                                .backups
                                .restic
                                .retry_lock_seconds
                        ))
                        .arg("backup")
                        .arg(&server.filesystem.base_path)
                        .args(&excluded_paths)
                        .arg("--tag")
                        .arg(uuid.to_string())
                        .arg("--group-by")
                        .arg("tags")
                        .arg("--limit-download")
                        .arg((server.app_state.config.system.backups.read_limit * 1024).to_string())
                        .arg("--limit-upload")
                        .arg(
                            (server.app_state.config.system.backups.write_limit * 1024).to_string(),
                        )
                        .stdout(std::process::Stdio::piped())
                        .stderr(std::process::Stdio::piped())
                        .spawn()?,
                    ResticBackupConfiguration {
                        repository: server
                            .app_state
                            .config
                            .system
                            .backups
                            .restic
                            .repository
                            .clone(),
                        password_file: Some(
                            server
                                .app_state
                                .config
                                .system
                                .backups
                                .restic
                                .password_file
                                .clone(),
                        ),
                        retry_lock_seconds: server
                            .app_state
                            .config
                            .system
                            .backups
                            .restic
                            .retry_lock_seconds,
                        environment: server
                            .app_state
                            .config
                            .system
                            .backups
                            .restic
                            .environment
                            .clone(),
                    },
                )
            } else {
                let configuration = server
                    .app_state
                    .config
                    .client
                    .backup_restic_configuration(uuid)
                    .await?;

                (
                    Command::new("restic")
                        .envs(&configuration.environment)
                        .arg("--json")
                        .arg("--repo")
                        .arg(&configuration.repository)
                        .arg("--retry-lock")
                        .arg(format!("{}s", configuration.retry_lock_seconds))
                        .arg("backup")
                        .arg(&server.filesystem.base_path)
                        .args(&excluded_paths)
                        .arg("--tag")
                        .arg(uuid.to_string())
                        .arg("--group-by")
                        .arg("tags")
                        .arg("--limit-download")
                        .arg((server.app_state.config.system.backups.read_limit * 1024).to_string())
                        .arg("--limit-upload")
                        .arg(
                            (server.app_state.config.system.backups.write_limit * 1024).to_string(),
                        )
                        .stdout(std::process::Stdio::piped())
                        .stderr(std::process::Stdio::piped())
                        .spawn()?,
                    configuration,
                )
            };

        let mut line_reader = tokio::io::BufReader::new(child.stdout.take().unwrap()).lines();

        let mut snapshot_id = None;
        let mut total_bytes_processed = 0;
        let mut total_files_processed = 0;

        while let Ok(Some(line)) = line_reader.next_line().await {
            if let Ok(json) = serde_json::from_str::<serde_json::Value>(&line) {
                if json.get("message_type").and_then(|v| v.as_str()) == Some("status") {
                    let bytes_done = json.get("bytes_done").and_then(|v| v.as_u64()).unwrap_or(0);
                    let total_bytes = json
                        .get("total_bytes")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);

                    progress.store(bytes_done, Ordering::SeqCst);
                    total.store(total_bytes, Ordering::SeqCst);
                } else if json.get("message_type").and_then(|v| v.as_str()) == Some("summary") {
                    total_bytes_processed = json
                        .get("total_bytes_processed")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);
                    total_files_processed = json
                        .get("total_files_processed")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);
                    snapshot_id = json
                        .get("snapshot_id")
                        .and_then(|v| v.as_str())
                        .map(String::from);
                }
            }
        }

        let output = child.wait_with_output().await?;

        if !output.status.success() {
            return Err(anyhow::anyhow!(
                "Failed to create Restic backup for {}: {}",
                server.filesystem.base_path.display(),
                String::from_utf8_lossy(&output.stderr)
            ));
        }

        if let Some(snapshot_id) = &snapshot_id {
            let mut cache = RESTIC_BACKUP_CACHE.write().await;
            cache.insert(
                uuid,
                (
                    ResticSnapshot {
                        short_id: snapshot_id.clone(),
                        tags: vec![uuid.to_string()],
                        paths: vec![server.filesystem.base_path.to_string_lossy().to_string()],
                    },
                    Arc::new(configuration),
                ),
            );
        }

        Ok(RawServerBackup {
            checksum: snapshot_id.unwrap_or_else(|| "unknown".to_string()),
            checksum_type: "restic".to_string(),
            size: total_bytes_processed,
            files: total_files_processed,
            successful: true,
            parts: vec![],
        })
    }
}

#[async_trait::async_trait]
impl BackupExt for ResticBackup {
    #[inline]
    fn uuid(&self) -> uuid::Uuid {
        self.uuid
    }

    async fn download(
        &self,
        config: &Arc<crate::config::Config>,
        archive_format: StreamableArchiveFormat,
    ) -> Result<crate::response::ApiResponse, anyhow::Error> {
        let compression_level = config.system.backups.compression_level;
        let (reader, writer) = tokio::io::duplex(crate::BUFFER_SIZE);

        match archive_format {
            StreamableArchiveFormat::Zip => {
                let child = std::process::Command::new("restic")
                    .envs(&self.configuration.environment)
                    .arg("--json")
                    .arg("--no-lock")
                    .arg("--repo")
                    .arg(&self.configuration.repository)
                    .args(self.configuration.password())
                    .arg("dump")
                    .arg(format!("{}:{}", self.short_id, self.server_path.display()))
                    .arg("/")
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::null())
                    .spawn()?;

                tokio::task::spawn_blocking(move || -> Result<(), anyhow::Error> {
                    let writer = tokio_util::io::SyncIoBridge::new(writer);
                    let mut archive = zip::ZipWriter::new_stream(writer);

                    let mut subtar = tar::Archive::new(child.stdout.unwrap());
                    let mut entries = subtar.entries()?;

                    let mut read_buffer = vec![0; crate::BUFFER_SIZE];
                    while let Some(Ok(mut entry)) = entries.next() {
                        let header = entry.header().clone();
                        let relative = entry.path()?;

                        let mut options: zip::write::FileOptions<'_, ()> =
                            zip::write::FileOptions::default()
                                .compression_level(
                                    Some(compression_level.to_deflate_level() as i64),
                                )
                                .unix_permissions(header.mode()?)
                                .large_file(header.size()? >= u32::MAX as u64);
                        if let Ok(mtime) = header.mtime()
                            && let Some(mtime) = chrono::DateTime::from_timestamp(mtime as i64, 0)
                        {
                            options =
                                options.last_modified_time(zip::DateTime::from_date_and_time(
                                    mtime.year() as u16,
                                    mtime.month() as u8,
                                    mtime.day() as u8,
                                    mtime.hour() as u8,
                                    mtime.minute() as u8,
                                    mtime.second() as u8,
                                )?);
                        }

                        match header.entry_type() {
                            tar::EntryType::Directory => {
                                archive.add_directory(relative.to_string_lossy(), options)?;
                            }
                            tar::EntryType::Regular => {
                                archive.start_file(relative.to_string_lossy(), options)?;
                                crate::io::copy_shared(&mut read_buffer, &mut entry, &mut archive)?;
                            }
                            _ => continue,
                        }
                    }

                    Ok(())
                });
            }
            _ => {
                let child = std::process::Command::new("restic")
                    .envs(&self.configuration.environment)
                    .arg("--json")
                    .arg("--no-lock")
                    .arg("--repo")
                    .arg(&self.configuration.repository)
                    .args(self.configuration.password())
                    .arg("dump")
                    .arg(format!("{}:{}", self.short_id, self.server_path.display()))
                    .arg("/")
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::null())
                    .spawn()?;

                let file_compression_threads = self.config.api.file_compression_threads;
                tokio::task::spawn_blocking(move || {
                    let mut writer = CompressionWriter::new(
                        tokio_util::io::SyncIoBridge::new(writer),
                        archive_format.compression_format(),
                        compression_level,
                        file_compression_threads,
                    );

                    if let Err(err) = crate::io::copy(&mut child.stdout.unwrap(), &mut writer) {
                        tracing::error!(
                            "failed to compress tar archive for restic backup: {}",
                            err
                        );
                    }

                    writer.finish().ok();
                });
            }
        }

        let mut headers = HeaderMap::with_capacity(2);
        headers.insert(
            "Content-Disposition",
            format!(
                "attachment; filename={}.{}",
                self.uuid,
                archive_format.extension()
            )
            .parse()?,
        );
        headers.insert("Content-Type", archive_format.mime_type().parse()?);

        Ok(ApiResponse::new(Body::from_stream(
            tokio_util::io::ReaderStream::with_capacity(reader, crate::BUFFER_SIZE),
        ))
        .with_headers(headers))
    }

    async fn restore(
        &self,
        server: &crate::server::Server,
        progress: Arc<AtomicU64>,
        total: Arc<AtomicU64>,
        _download_url: Option<String>,
    ) -> Result<(), anyhow::Error> {
        let child = Command::new("restic")
            .envs(&self.configuration.environment)
            .arg("--json")
            .arg("--no-lock")
            .arg("--repo")
            .arg(&self.configuration.repository)
            .args(self.configuration.password())
            .arg("restore")
            .arg(format!("{}:{}", self.short_id, self.server_path.display()))
            .arg("--target")
            .arg(&server.filesystem.base_path)
            .arg("--limit-download")
            .arg((server.app_state.config.system.backups.read_limit * 1024).to_string())
            .stdout(std::process::Stdio::piped())
            .spawn()?;

        let mut line_reader = tokio::io::BufReader::new(child.stdout.unwrap()).lines();

        while let Ok(Some(line)) = line_reader.next_line().await {
            if let Ok(json) = serde_json::from_str::<serde_json::Value>(&line)
                && json.get("message_type").and_then(|v| v.as_str()) == Some("status")
            {
                let total_bytes = json
                    .get("total_bytes")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                let bytes_restored = json
                    .get("bytes_restored")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                let percent_done = json
                    .get("percent_done")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.0);
                let percent_done = (percent_done * 10000.0).round() / 100.0;

                progress.store(bytes_restored, Ordering::SeqCst);
                total.store(total_bytes, Ordering::SeqCst);

                server
                    .log_daemon(format!(
                        "(restoring): {} of {} ({}%)",
                        human_bytes(bytes_restored as f64),
                        human_bytes(total_bytes as f64),
                        percent_done
                    ))
                    .await;
            }
        }

        Ok(())
    }

    async fn delete(&self, _config: &Arc<crate::config::Config>) -> Result<(), anyhow::Error> {
        let output = Command::new("restic")
            .envs(&self.configuration.environment)
            .arg("--repo")
            .arg(&self.configuration.repository)
            .args(self.configuration.password())
            .arg("--retry-lock")
            .arg(format!("{}s", self.configuration.retry_lock_seconds))
            .arg("forget")
            .arg(&self.short_id)
            .arg("--group-by")
            .arg("tags")
            .arg("--prune")
            .output()
            .await?;

        if !output.status.success() {
            return Err(anyhow::anyhow!(
                "failed to delete restic backup: {}",
                String::from_utf8_lossy(&output.stderr)
            ));
        }

        let mut cache = RESTIC_BACKUP_CACHE.write().await;
        cache.remove(&self.uuid);

        Ok(())
    }

    async fn browse(&self, server: &crate::server::Server) -> Result<BrowseBackup, anyhow::Error> {
        let child = Command::new("restic")
            .envs(&self.configuration.environment)
            .arg("--json")
            .arg("--repo")
            .arg(&self.configuration.repository)
            .args(self.configuration.password())
            .arg("--retry-lock")
            .arg(format!("{}s", self.configuration.retry_lock_seconds))
            .arg("ls")
            .arg(format!("{}:{}", self.short_id, self.server_path.display()))
            .arg("/")
            .arg("--recursive")
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()?;

        let mut line_reader = tokio::io::BufReader::new(child.stdout.unwrap()).lines();
        let mut entries = Vec::new();

        while let Ok(Some(line)) = line_reader.next_line().await {
            if line.is_empty() {
                continue;
            }

            if let Ok(mut entry) = serde_json::from_str::<ResticDirectoryEntry>(&line) {
                entry.path = entry
                    .path
                    .strip_prefix(Path::new("/"))
                    .unwrap_or(&entry.path)
                    .to_owned();

                entries.push(entry);
            }
        }

        Ok(BrowseBackup::Restic(BrowseResticBackup {
            server: server.clone(),
            short_id: self.short_id.clone(),
            server_path: self.server_path.clone(),
            configuration: Arc::clone(&self.configuration),
            entries: Arc::new(entries),
        }))
    }
}

#[async_trait::async_trait]
impl BackupCleanExt for ResticBackup {
    async fn clean(
        _server: &crate::server::Server,
        _uuid: uuid::Uuid,
    ) -> Result<(), anyhow::Error> {
        Ok(())
    }
}

pub struct BrowseResticBackup {
    server: crate::server::Server,
    short_id: String,

    server_path: PathBuf,
    configuration: Arc<ResticBackupConfiguration>,
    entries: Arc<Vec<ResticDirectoryEntry>>,
}

impl BrowseResticBackup {
    fn restic_entry_to_directory_entry(
        &self,
        path: &Path,
        entry: &ResticDirectoryEntry,
    ) -> DirectoryEntry {
        let size = match entry.r#type {
            ResticEntryType::File => entry.size.unwrap_or(0),
            ResticEntryType::Dir => self
                .entries
                .iter()
                .filter(|e| e.path.starts_with(&entry.path))
                .map(|e| e.size.unwrap_or(0))
                .sum(),
            _ => 0,
        };

        let mime = match entry.r#type {
            ResticEntryType::Dir => "inode/directory",
            ResticEntryType::Symlink => "inode/symlink",
            _ => new_mime_guess::from_path(&entry.path)
                .iter_raw()
                .next()
                .unwrap_or("application/octet-stream"),
        };

        let mut mode_str = String::new();

        mode_str.reserve_exact(10);
        mode_str.push(match rustix::fs::FileType::from_raw_mode(entry.mode) {
            rustix::fs::FileType::RegularFile => '-',
            rustix::fs::FileType::Directory => 'd',
            rustix::fs::FileType::Symlink => 'l',
            rustix::fs::FileType::BlockDevice => 'b',
            rustix::fs::FileType::CharacterDevice => 'c',
            rustix::fs::FileType::Socket => 's',
            rustix::fs::FileType::Fifo => 'p',
            rustix::fs::FileType::Unknown => '?',
        });

        const RWX: &str = "rwxrwxrwx";
        for i in 0..9 {
            if entry.mode & (1 << (8 - i)) != 0 {
                mode_str.push(RWX.chars().nth(i).unwrap());
            } else {
                mode_str.push('-');
            }
        }

        DirectoryEntry {
            name: path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string(),
            created: chrono::DateTime::from_timestamp(0, 0).unwrap_or_default(),
            modified: entry.mtime,
            mode: mode_str,
            mode_bits: format!("{:o}", entry.mode & 0o777),
            size,
            directory: matches!(entry.r#type, ResticEntryType::Dir),
            file: matches!(entry.r#type, ResticEntryType::File),
            symlink: matches!(entry.r#type, ResticEntryType::Symlink),
            mime,
        }
    }
}

#[async_trait::async_trait]
impl BackupBrowseExt for BrowseResticBackup {
    async fn read_dir(
        &self,
        path: PathBuf,
        per_page: Option<usize>,
        page: usize,
        is_ignored: impl Fn(PathBuf, bool) -> bool + Send + Sync + 'static,
    ) -> Result<(usize, Vec<crate::models::DirectoryEntry>), anyhow::Error> {
        let mut directory_entries = Vec::new();
        let mut other_entries = Vec::new();

        let path_len = path.components().count();
        for entry in self.entries.iter() {
            let name = &entry.path;

            let name_len = name.components().count();
            if name_len < path_len
                || !name.starts_with(&path)
                || name == &path
                || name_len > path_len + 1
            {
                continue;
            }

            if is_ignored(name.clone(), matches!(entry.r#type, ResticEntryType::Dir)) {
                continue;
            }

            if matches!(entry.r#type, ResticEntryType::Dir) {
                directory_entries.push(entry);
            } else {
                other_entries.push(entry);
            }
        }

        directory_entries.sort_unstable_by(|a, b| a.path.cmp(&b.path));
        other_entries.sort_unstable_by(|a, b| a.path.cmp(&b.path));

        let total_entries = directory_entries.len() + other_entries.len();
        let mut entries = Vec::new();

        if let Some(per_page) = per_page {
            let start = (page - 1) * per_page;

            for entry in directory_entries
                .iter()
                .chain(other_entries.iter())
                .skip(start)
                .take(per_page)
            {
                entries.push(self.restic_entry_to_directory_entry(&entry.path, entry));
            }
        } else {
            for entry in directory_entries.iter().chain(other_entries.iter()) {
                entries.push(self.restic_entry_to_directory_entry(&entry.path, entry));
            }
        }

        Ok((total_entries, entries))
    }

    async fn read_file(
        &self,
        path: PathBuf,
    ) -> Result<(u64, Box<dyn tokio::io::AsyncRead + Unpin + Send>), anyhow::Error> {
        let entry = self
            .entries
            .iter()
            .find(|e| e.path == path)
            .ok_or_else(|| anyhow::anyhow!(std::io::Error::from(rustix::io::Errno::NOENT)))?;
        if !matches!(entry.r#type, ResticEntryType::File) {
            return Err(anyhow::anyhow!(std::io::Error::from(
                rustix::io::Errno::NOENT
            )));
        }

        let full_path = PathBuf::from(&self.server_path).join(&entry.path);

        let child = Command::new("restic")
            .envs(&self.configuration.environment)
            .arg("--json")
            .arg("--no-lock")
            .arg("--repo")
            .arg(&self.configuration.repository)
            .args(self.configuration.password())
            .arg("dump")
            .arg(&self.short_id)
            .arg(full_path)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()?;

        Ok((entry.size.unwrap_or(0), Box::new(child.stdout.unwrap())))
    }

    async fn read_directory_archive(
        &self,
        path: PathBuf,
        archive_format: StreamableArchiveFormat,
    ) -> Result<tokio::io::DuplexStream, anyhow::Error> {
        let entry = self
            .entries
            .iter()
            .find(|e| e.path == path)
            .ok_or_else(|| anyhow::anyhow!(std::io::Error::from(rustix::io::Errno::NOENT)))?;
        if !matches!(entry.r#type, ResticEntryType::Dir) {
            return Err(anyhow::anyhow!(std::io::Error::from(
                rustix::io::Errno::NOENT
            )));
        }

        let full_path = PathBuf::from(&self.server_path).join(&entry.path);
        let compression_level = self
            .server
            .app_state
            .config
            .system
            .backups
            .compression_level;
        let (reader, writer) = tokio::io::duplex(crate::BUFFER_SIZE);

        match archive_format {
            StreamableArchiveFormat::Zip => {
                let child = std::process::Command::new("restic")
                    .envs(&self.configuration.environment)
                    .arg("--json")
                    .arg("--no-lock")
                    .arg("--repo")
                    .arg(&self.configuration.repository)
                    .args(self.configuration.password())
                    .arg("dump")
                    .arg(format!("{}:{}", self.short_id, full_path.display()))
                    .arg("/")
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::null())
                    .spawn()?;

                tokio::task::spawn_blocking(move || -> Result<(), anyhow::Error> {
                    let writer = tokio_util::io::SyncIoBridge::new(writer);
                    let mut archive = zip::ZipWriter::new_stream(writer);

                    let mut subtar = tar::Archive::new(child.stdout.unwrap());
                    let mut entries = subtar.entries()?;

                    let mut read_buffer = vec![0; crate::BUFFER_SIZE];
                    while let Some(Ok(mut entry)) = entries.next() {
                        let header = entry.header().clone();
                        let relative = entry.path()?;

                        let mut options: zip::write::FileOptions<'_, ()> =
                            zip::write::FileOptions::default()
                                .compression_level(
                                    Some(compression_level.to_deflate_level() as i64),
                                )
                                .unix_permissions(header.mode()?)
                                .large_file(header.size()? >= u32::MAX as u64);
                        if let Ok(mtime) = header.mtime()
                            && let Some(mtime) = chrono::DateTime::from_timestamp(mtime as i64, 0)
                        {
                            options =
                                options.last_modified_time(zip::DateTime::from_date_and_time(
                                    mtime.year() as u16,
                                    mtime.month() as u8,
                                    mtime.day() as u8,
                                    mtime.hour() as u8,
                                    mtime.minute() as u8,
                                    mtime.second() as u8,
                                )?);
                        }

                        match header.entry_type() {
                            tar::EntryType::Directory => {
                                archive.add_directory(relative.to_string_lossy(), options)?;
                            }
                            tar::EntryType::Regular => {
                                archive.start_file(relative.to_string_lossy(), options)?;
                                crate::io::copy_shared(&mut read_buffer, &mut entry, &mut archive)?;
                            }
                            _ => continue,
                        }
                    }

                    Ok(())
                });
            }
            _ => {
                let child = std::process::Command::new("restic")
                    .envs(&self.configuration.environment)
                    .arg("--json")
                    .arg("--no-lock")
                    .arg("--repo")
                    .arg(&self.configuration.repository)
                    .args(self.configuration.password())
                    .arg("dump")
                    .arg(format!("{}:{}", self.short_id, full_path.display()))
                    .arg("/")
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::null())
                    .spawn()?;

                let file_compression_threads =
                    self.server.app_state.config.api.file_compression_threads;
                tokio::task::spawn_blocking(move || {
                    let mut writer = CompressionWriter::new(
                        tokio_util::io::SyncIoBridge::new(writer),
                        archive_format.compression_format(),
                        compression_level,
                        file_compression_threads,
                    );

                    if let Err(err) = crate::io::copy(&mut child.stdout.unwrap(), &mut writer) {
                        tracing::error!(
                            "failed to compress tar archive for restic backup: {}",
                            err
                        );
                    }

                    writer.finish().ok();
                });
            }
        }

        Ok(reader)
    }

    async fn read_files_archive(
        &self,
        path: PathBuf,
        file_paths: Vec<PathBuf>,
        archive_format: StreamableArchiveFormat,
    ) -> Result<tokio::io::DuplexStream, anyhow::Error> {
        let path = if path.components().count() > 0 {
            let entry = self
                .entries
                .iter()
                .find(|e| e.path == path)
                .ok_or_else(|| anyhow::anyhow!(std::io::Error::from(rustix::io::Errno::NOENT)))?;
            if !matches!(entry.r#type, ResticEntryType::Dir) {
                return Err(anyhow::anyhow!(std::io::Error::from(
                    rustix::io::Errno::NOENT
                )));
            }

            &entry.path
        } else {
            &PathBuf::from("")
        };

        let full_path = PathBuf::from(&self.server_path).join(path);
        let compression_level = self
            .server
            .app_state
            .config
            .system
            .backups
            .compression_level;
        let (reader, writer) = tokio::io::duplex(crate::BUFFER_SIZE);

        match archive_format {
            StreamableArchiveFormat::Zip => {
                tokio::task::spawn_blocking({
                    let short_id = self.short_id.clone();
                    let configuration = Arc::clone(&self.configuration);
                    let entries = Arc::clone(&self.entries);

                    move || -> Result<(), anyhow::Error> {
                        let writer = tokio_util::io::SyncIoBridge::new(writer);
                        let mut archive = zip::ZipWriter::new_stream(writer);

                        for file_path in file_paths {
                            let path = full_path.join(&file_path);
                            let entry = match entries.iter().find(|e| e.path == file_path) {
                                Some(entry) => entry,
                                None => continue,
                            };

                            let relative = match path.strip_prefix(&full_path) {
                                Ok(path) => path,
                                Err(_) => continue,
                            };

                            let options: zip::write::FileOptions<'_, ()> =
                                zip::write::FileOptions::default()
                                    .compression_level(Some(
                                        compression_level.to_deflate_level() as i64
                                    ))
                                    .unix_permissions(entry.mode)
                                    .large_file(
                                        entry.size.is_some_and(|size| size >= u32::MAX as u64),
                                    )
                                    .last_modified_time(zip::DateTime::from_date_and_time(
                                        entry.mtime.year() as u16,
                                        entry.mtime.month() as u8,
                                        entry.mtime.day() as u8,
                                        entry.mtime.hour() as u8,
                                        entry.mtime.minute() as u8,
                                        entry.mtime.second() as u8,
                                    )?);

                            match entry.r#type {
                                ResticEntryType::Dir => {
                                    archive.add_directory(relative.to_string_lossy(), options)?;

                                    let child = match std::process::Command::new("restic")
                                        .envs(&configuration.environment)
                                        .arg("--json")
                                        .arg("--no-lock")
                                        .arg("--repo")
                                        .arg(&configuration.repository)
                                        .args(configuration.password())
                                        .arg("dump")
                                        .arg(format!("{}:{}", short_id, path.display()))
                                        .arg("/")
                                        .stdout(std::process::Stdio::piped())
                                        .stderr(std::process::Stdio::null())
                                        .spawn()
                                    {
                                        Ok(child) => child,
                                        Err(_) => continue,
                                    };

                                    let mut subtar = tar::Archive::new(child.stdout.unwrap());
                                    let mut entries = match subtar.entries() {
                                        Ok(entries) => entries,
                                        Err(_) => continue,
                                    };

                                    let mut read_buffer = vec![0; crate::BUFFER_SIZE];
                                    while let Some(Ok(mut entry)) = entries.next() {
                                        let header = entry.header().clone();
                                        let relative = relative.join(entry.path()?);

                                        let mut options: zip::write::FileOptions<'_, ()> =
                                            zip::write::FileOptions::default()
                                                .compression_level(Some(
                                                    compression_level.to_deflate_level() as i64,
                                                ))
                                                .unix_permissions(header.mode()?)
                                                .large_file(header.size()? >= u32::MAX as u64);
                                        if let Ok(mtime) = header.mtime()
                                            && let Some(mtime) =
                                                chrono::DateTime::from_timestamp(mtime as i64, 0)
                                        {
                                            options = options.last_modified_time(
                                                zip::DateTime::from_date_and_time(
                                                    mtime.year() as u16,
                                                    mtime.month() as u8,
                                                    mtime.day() as u8,
                                                    mtime.hour() as u8,
                                                    mtime.minute() as u8,
                                                    mtime.second() as u8,
                                                )?,
                                            );
                                        }

                                        match header.entry_type() {
                                            tar::EntryType::Directory => {
                                                archive.add_directory(
                                                    relative.to_string_lossy(),
                                                    options,
                                                )?;
                                            }
                                            tar::EntryType::Regular => {
                                                archive.start_file(
                                                    relative.to_string_lossy(),
                                                    options,
                                                )?;
                                                crate::io::copy_shared(
                                                    &mut read_buffer,
                                                    &mut entry,
                                                    &mut archive,
                                                )?;
                                            }
                                            _ => continue,
                                        }
                                    }
                                }
                                ResticEntryType::File => {
                                    let child = match std::process::Command::new("restic")
                                        .envs(&configuration.environment)
                                        .arg("--json")
                                        .arg("--no-lock")
                                        .arg("--repo")
                                        .arg(&configuration.repository)
                                        .args(configuration.password())
                                        .arg("dump")
                                        .arg(&short_id)
                                        .arg(&path)
                                        .stdout(std::process::Stdio::piped())
                                        .stderr(std::process::Stdio::null())
                                        .spawn()
                                    {
                                        Ok(child) => child,
                                        Err(_) => continue,
                                    };

                                    archive.start_file(relative.to_string_lossy(), options)?;
                                    crate::io::copy(&mut child.stdout.unwrap(), &mut archive)?;
                                }
                                _ => continue,
                            }
                        }

                        archive.finish()?;

                        Ok(())
                    }
                });
            }
            _ => {
                tokio::task::spawn_blocking({
                    let file_compression_threads =
                        self.server.app_state.config.api.file_compression_threads;
                    let short_id = self.short_id.clone();
                    let configuration = Arc::clone(&self.configuration);
                    let entries = Arc::clone(&self.entries);

                    move || {
                        let writer = CompressionWriter::new(
                            tokio_util::io::SyncIoBridge::new(writer),
                            archive_format.compression_format(),
                            compression_level,
                            file_compression_threads,
                        );
                        let mut archive = tar::Builder::new(writer);

                        for file_path in file_paths {
                            let path = full_path.join(&file_path);
                            let entry = match entries.iter().find(|e| e.path == file_path) {
                                Some(entry) => entry,
                                None => continue,
                            };

                            let relative = match path.strip_prefix(&full_path) {
                                Ok(path) => path,
                                Err(_) => continue,
                            };

                            let mut header = tar::Header::new_gnu();
                            header.set_size(0);
                            header.set_mode(entry.mode);
                            header.set_mtime(entry.mtime.timestamp() as u64);

                            match entry.r#type {
                                ResticEntryType::Dir => {
                                    header.set_entry_type(tar::EntryType::Directory);

                                    if archive
                                        .append_data(&mut header, relative, std::io::empty())
                                        .is_ok()
                                    {
                                        let child = match std::process::Command::new("restic")
                                            .envs(&configuration.environment)
                                            .arg("--json")
                                            .arg("--no-lock")
                                            .arg("--repo")
                                            .arg(&configuration.repository)
                                            .args(configuration.password())
                                            .arg("dump")
                                            .arg(format!("{}:{}", short_id, path.display()))
                                            .arg("/")
                                            .stdout(std::process::Stdio::piped())
                                            .stderr(std::process::Stdio::null())
                                            .spawn()
                                        {
                                            Ok(child) => child,
                                            Err(_) => continue,
                                        };

                                        let mut subtar = tar::Archive::new(child.stdout.unwrap());
                                        let mut entries = match subtar.entries() {
                                            Ok(entries) => entries,
                                            Err(_) => continue,
                                        };

                                        while let Some(Ok(entry)) = entries.next() {
                                            let mut header = entry.header().clone();

                                            match archive.append_data(
                                                &mut header,
                                                relative.join(entry.path().unwrap()),
                                                entry,
                                            ) {
                                                Ok(_) => {}
                                                Err(_) => break,
                                            }
                                        }
                                    }
                                }
                                ResticEntryType::File => {
                                    let child = match std::process::Command::new("restic")
                                        .envs(&configuration.environment)
                                        .arg("--json")
                                        .arg("--no-lock")
                                        .arg("--repo")
                                        .arg(&configuration.repository)
                                        .args(configuration.password())
                                        .arg("dump")
                                        .arg(&short_id)
                                        .arg(&path)
                                        .stdout(std::process::Stdio::piped())
                                        .stderr(std::process::Stdio::null())
                                        .spawn()
                                    {
                                        Ok(child) => child,
                                        Err(_) => continue,
                                    };

                                    header.set_size(entry.size.unwrap_or(0));
                                    header.set_entry_type(tar::EntryType::Regular);

                                    archive
                                        .append_data(&mut header, relative, child.stdout.unwrap())
                                        .ok();
                                }
                                _ => continue,
                            }
                        }

                        if let Ok(inner) = archive.into_inner() {
                            inner.finish().ok();
                        }
                    }
                });
            }
        }

        Ok(reader)
    }
}
