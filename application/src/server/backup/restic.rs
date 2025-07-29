use crate::{remote::backups::RawServerBackup, response::ApiResponse};
use axum::{body::Body, http::HeaderMap};
use human_bytes::human_bytes;
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        Arc, LazyLock,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt},
    process::Command,
    sync::{Mutex, RwLock},
};

#[macro_export]
macro_rules! restic_configuration {
    ($backuo_configurations:expr, $config:expr) => {
        match &$backuo_configurations.restic {
            Some(restic) => (
                &restic.repository,
                restic.retry_lock_seconds,
                &[] as &[&str],
                &restic.environment,
            ),
            None => (
                &$config.system.backups.restic.repository,
                $config.system.backups.restic.retry_lock_seconds,
                &[
                    "--password-file",
                    &$config.system.backups.restic.password_file,
                ] as &[&str],
                &$config.system.backups.restic.environment,
            ),
        }
    };
}

struct BackupCache {
    backups: HashMap<uuid::Uuid, PathBuf>,
    last_updated: Instant,
    refresh_in_progress: bool,
}

static GLOBAL_BACKUP_CACHE: LazyLock<RwLock<BackupCache>> = LazyLock::new(|| {
    RwLock::new(BackupCache {
        backups: HashMap::new(),
        last_updated: Instant::now() - Duration::from_secs(3600),
        refresh_in_progress: false,
    })
});

static REFRESH_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));
const BACKGROUND_REFRESH_THRESHOLD: Duration = Duration::from_secs(120);
const FOREGROUND_REFRESH_THRESHOLD: Duration = Duration::from_secs(3600);

async fn get_backup_list(config: &Arc<crate::config::Config>) -> Vec<uuid::Uuid> {
    let cache_age = {
        let cache = GLOBAL_BACKUP_CACHE.read().await;
        cache.last_updated.elapsed()
    };

    if cache_age < BACKGROUND_REFRESH_THRESHOLD {
        let cache = GLOBAL_BACKUP_CACHE.read().await;

        cache.backups.keys().copied().collect()
    } else if cache_age < FOREGROUND_REFRESH_THRESHOLD {
        let backups = {
            let cache = GLOBAL_BACKUP_CACHE.read().await;

            cache.backups.keys().copied().collect()
        };

        let refresh_needed = {
            let cache = GLOBAL_BACKUP_CACHE.read().await;
            !cache.refresh_in_progress
        };

        if refresh_needed {
            tracing::debug!("refreshing global restic backup cache");
            tokio::spawn(refresh_backup_cache(config.clone()));
        }

        backups
    } else {
        tracing::debug!("refreshing global restic backup cache (foreground)");
        refresh_backup_cache(config.clone()).await
    }
}

async fn refresh_backup_cache(config: Arc<crate::config::Config>) -> Vec<uuid::Uuid> {
    let _guard = REFRESH_LOCK.lock().await;

    {
        let mut cache = GLOBAL_BACKUP_CACHE.write().await;
        if cache.refresh_in_progress {
            return cache.backups.keys().copied().collect();
        }

        cache.refresh_in_progress = true;
    }

    let mut backups = HashMap::new();

    let configuration = config.backup_configurations.read().await;
    let (repository, _, args, envs) = restic_configuration!(&configuration, config);

    let output = match Command::new("restic")
        .envs(envs)
        .arg("--json")
        .arg("--no-lock")
        .arg("--repo")
        .arg(repository)
        .args(args)
        .arg("snapshots")
        .output()
        .await
    {
        Ok(output) => output,
        Err(err) => {
            tracing::error!("failed to list Restic backups: {}", err);

            let mut cache = GLOBAL_BACKUP_CACHE.write().await;
            cache.refresh_in_progress = false;

            return cache.backups.keys().copied().collect();
        }
    };
    drop(configuration);

    if !output.status.success() {
        tracing::error!(
            "failed to list Restic backups: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let snapshots = match serde_json::from_slice::<Vec<serde_json::Value>>(&output.stdout) {
        Ok(snapshots) => snapshots,
        Err(err) => {
            tracing::error!(
                "failed to parse Restic snapshots: {} <- {}",
                err,
                String::from_utf8_lossy(&output.stdout)
            );

            let mut cache = GLOBAL_BACKUP_CACHE.write().await;
            cache.refresh_in_progress = false;

            return cache.backups.keys().copied().collect();
        }
    };

    for snapshot in snapshots {
        if let Some(tags) = snapshot.get("tags")
            && let Some(tag) = tags.as_array().and_then(|arr| arr.first())
            && let Some(uuid_str) = tag.as_str()
            && let Ok(uuid) = uuid::Uuid::parse_str(uuid_str)
            && let Some(paths) = snapshot.get("paths")
            && let Some(path) = paths.as_array().and_then(|arr| arr.first())
            && let Some(path_str) = path.as_str()
        {
            backups.insert(uuid, PathBuf::from(path_str));
        }
    }

    let mut cache = GLOBAL_BACKUP_CACHE.write().await;
    cache.backups = backups;
    cache.last_updated = Instant::now();
    cache.refresh_in_progress = false;

    cache.backups.keys().copied().collect()
}

pub async fn get_backup_base_path(
    config: &Arc<crate::config::Config>,
    uuid: uuid::Uuid,
) -> Result<PathBuf, anyhow::Error> {
    let backups = get_backup_list(config).await;
    if !backups.contains(&uuid) {
        return Err(anyhow::anyhow!("Backup with UUID {} not found", uuid));
    }

    let cached_backups = GLOBAL_BACKUP_CACHE.read().await;
    if let Some(path) = cached_backups.backups.get(&uuid) {
        return Ok(path.clone());
    }

    Err(anyhow::anyhow!("Backup with UUID {} not found", uuid))
}

pub async fn create_backup(
    server: crate::server::Server,
    uuid: uuid::Uuid,
    progress: Arc<AtomicU64>,
    total: Arc<AtomicU64>,
    ignore_raw: String,
) -> Result<RawServerBackup, anyhow::Error> {
    let mut excluded_paths = Vec::new();
    for line in ignore_raw.lines() {
        excluded_paths.push("--exclude");
        excluded_paths.push(line);
    }

    let backups = get_backup_list(&server.config).await;

    let configuration = server.config.backup_configurations.read().await;
    let (repository, retry_lock_seconds, args, envs) =
        restic_configuration!(&configuration, server.config);

    let mut child = Command::new("restic")
        .envs(envs)
        .arg("--json")
        .arg("--repo")
        .arg(repository)
        .args(args)
        .arg("--retry-lock")
        .arg(format!("{retry_lock_seconds}s"))
        .arg("backup")
        .arg(&server.filesystem.base_path)
        .args(&excluded_paths)
        .arg("--tag")
        .arg(uuid.to_string())
        .arg("--group-by")
        .arg("tags")
        .arg("--limit-download")
        .arg((server.config.system.backups.read_limit * 1024).to_string())
        .arg("--limit-upload")
        .arg((server.config.system.backups.write_limit * 1024).to_string())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;
    drop(configuration);

    let mut line_reader = tokio::io::BufReader::new(child.stdout.take().unwrap()).lines();

    let mut snapshot_id = None;
    let mut total_bytes_processed = 0;

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
                let total_bytes = json
                    .get("total_bytes_processed")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                snapshot_id = json
                    .get("snapshot_id")
                    .and_then(|v| v.as_str())
                    .map(String::from);
                total_bytes_processed = total_bytes;
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

    if !backups.contains(&uuid) {
        let mut cache = GLOBAL_BACKUP_CACHE.write().await;
        cache
            .backups
            .insert(uuid, server.filesystem.base_path.clone());
    }

    Ok(RawServerBackup {
        checksum: snapshot_id.unwrap_or_else(|| "unknown".to_string()),
        checksum_type: "restic".to_string(),
        size: total_bytes_processed,
        successful: true,
        parts: vec![],
    })
}

pub async fn restore_backup(
    server: crate::server::Server,
    uuid: uuid::Uuid,
    progress: Arc<AtomicU64>,
    total: Arc<AtomicU64>,
) -> Result<(), anyhow::Error> {
    let base_path = get_backup_base_path(&server.config, uuid).await?;

    let configuration = server.config.backup_configurations.read().await;
    let (repository, _, args, envs) = restic_configuration!(&configuration, server.config);

    let child = Command::new("restic")
        .envs(envs)
        .arg("--json")
        .arg("--no-lock")
        .arg("--repo")
        .arg(repository)
        .args(args)
        .arg("restore")
        .arg(format!("latest:{}", base_path.display()))
        .arg("--tag")
        .arg(uuid.to_string())
        .arg("--target")
        .arg(&server.filesystem.base_path)
        .arg("--limit-download")
        .arg((server.config.system.backups.read_limit * 1024).to_string())
        .stdout(std::process::Stdio::piped())
        .spawn()?;
    drop(configuration);

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

pub async fn download_backup(
    config: &Arc<crate::config::Config>,
    uuid: uuid::Uuid,
) -> Result<ApiResponse, anyhow::Error> {
    let base_path = get_backup_base_path(config, uuid).await?;

    let configuration = config.backup_configurations.read().await;
    let (repository, _, args, envs) = restic_configuration!(&configuration, config);

    let child = Command::new("restic")
        .envs(envs)
        .arg("--json")
        .arg("--no-lock")
        .arg("--repo")
        .arg(repository)
        .args(args)
        .arg("dump")
        .arg(format!("latest:{}", base_path.display()))
        .arg("/")
        .arg("--tag")
        .arg(uuid.to_string())
        .stdout(std::process::Stdio::piped())
        .spawn()?;
    drop(configuration);

    let (reader, writer) = tokio::io::duplex(crate::BUFFER_SIZE);

    let compression_level = config.system.backups.compression_level;
    tokio::spawn(async move {
        let mut stdout = child.stdout.unwrap();
        let mut writer = async_compression::tokio::write::GzipEncoder::with_quality(
            writer,
            async_compression::Level::Precise(
                compression_level.flate2_compression_level().level() as i32
            ),
        );

        tokio::io::copy(&mut stdout, &mut writer).await.ok();
        writer.shutdown().await.ok();
    });

    let mut headers = HeaderMap::with_capacity(2);
    headers.insert(
        "Content-Disposition",
        format!("attachment; filename={uuid}.tar.gz")
            .parse()
            .unwrap(),
    );
    headers.insert("Content-Type", "application/gzip".parse().unwrap());

    Ok(ApiResponse::new(Body::from_stream(
        tokio_util::io::ReaderStream::with_capacity(reader, crate::BUFFER_SIZE),
    ))
    .with_headers(headers))
}

pub async fn delete_backup(
    config: &Arc<crate::config::Config>,
    uuid: uuid::Uuid,
) -> Result<(), anyhow::Error> {
    let configuration = config.backup_configurations.read().await;
    let (repository, _, args, envs) = restic_configuration!(&configuration, config);

    let output = Command::new("restic")
        .envs(envs)
        .arg("--repo")
        .arg(repository)
        .args(args)
        .arg("forget")
        .arg("latest")
        .arg("--tag")
        .arg(uuid.to_string())
        .arg("--group-by")
        .arg("tags")
        .arg("--prune")
        .output()
        .await?;
    drop(configuration);

    if !output.status.success() {
        return Err(anyhow::anyhow!(
            "failed to delete restic backup: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    let mut cache = GLOBAL_BACKUP_CACHE.write().await;
    cache.backups.remove(&uuid);

    Ok(())
}

pub async fn list_backups(
    config: &Arc<crate::config::Config>,
) -> Result<Vec<uuid::Uuid>, anyhow::Error> {
    if config.backup_configurations.read().await.restic.is_none()
        && tokio::fs::metadata(&config.system.backups.restic.password_file)
            .await
            .is_err()
    {
        return Ok(Vec::new());
    }

    Ok(get_backup_list(config).await)
}
