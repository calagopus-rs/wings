use crate::remote::backups::RawServerBackup;
use axum::{
    body::Body,
    http::{HeaderMap, StatusCode},
};
use human_bytes::human_bytes;
use std::{collections::HashMap, path::PathBuf};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt},
    process::Command,
    sync::RwLock,
};

static CACHED_BACKUPS: RwLock<Option<HashMap<uuid::Uuid, PathBuf>>> = RwLock::const_new(None);

async fn get_backup_list(server: &crate::server::Server) -> Vec<uuid::Uuid> {
    let cached_backups = CACHED_BACKUPS.read().await;
    if let Some(backups) = cached_backups.as_ref() {
        return backups.keys().copied().collect();
    }

    drop(cached_backups);

    let (backups_sender, backups_reciever) = tokio::sync::oneshot::channel();
    tokio::spawn({
        let server = server.clone();
        let mut backups_sender = Some(backups_sender);

        async move {
            loop {
                let mut backups = HashMap::new();

                let output = match Command::new("restic")
                    .envs(&server.config.system.backups.restic.environment)
                    .arg("--json")
                    .arg("--no-lock")
                    .arg("--repo")
                    .arg(&server.config.system.backups.restic.repository)
                    .arg("--password-file")
                    .arg(&server.config.system.backups.restic.password_file)
                    .arg("snapshots")
                    .output()
                    .await
                {
                    Ok(output) => output,
                    Err(err) => {
                        tracing::error!("failed to list Restic backups: {}", err);
                        tokio::time::sleep(std::time::Duration::from_secs(60)).await;
                        continue;
                    }
                };

                if !output.status.success() {
                    tracing::error!(
                        "failed to list Restic backups: {}",
                        String::from_utf8_lossy(&output.stderr)
                    );
                }

                let snapshots =
                    match serde_json::from_slice::<Vec<serde_json::Value>>(&output.stdout) {
                        Ok(snapshots) => snapshots,
                        Err(err) => {
                            tracing::error!("failed to parse Restic snapshots: {}", err);
                            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
                            continue;
                        }
                    };

                for snapshot in snapshots {
                    if let Some(tags) = snapshot.get("tags") {
                        if let Some(tag) = tags.as_array().and_then(|arr| arr.first()) {
                            if let Some(uuid_str) = tag.as_str() {
                                if let Ok(uuid) = uuid::Uuid::parse_str(uuid_str) {
                                    if let Some(paths) = snapshot.get("paths") {
                                        if let Some(path) =
                                            paths.as_array().and_then(|arr| arr.first())
                                        {
                                            if let Some(path_str) = path.as_str() {
                                                backups.insert(uuid, PathBuf::from(path_str));
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }

                if let Some(sender) = backups_sender.take() {
                    sender
                        .send(backups.iter().map(|(uuid, _)| *uuid).collect())
                        .unwrap_or(());
                }
                CACHED_BACKUPS.write().await.replace(backups);

                tokio::time::sleep(std::time::Duration::from_secs(60)).await;
            }
        }
    });

    backups_reciever.into_future().await.unwrap()
}

pub async fn get_backup_base_path(
    server: &crate::server::Server,
    uuid: uuid::Uuid,
) -> Option<PathBuf> {
    let backups = get_backup_list(server).await;
    if !backups.contains(&uuid) {
        return None;
    }

    let cached_backups = CACHED_BACKUPS.read().await;
    if let Some(cached_backups) = cached_backups.as_ref() {
        if let Some(path) = cached_backups.get(&uuid) {
            return Some(path.clone());
        }
    }

    None
}

pub async fn create_backup(
    server: crate::server::Server,
    uuid: uuid::Uuid,
    ignore_raw: String,
) -> Result<RawServerBackup, anyhow::Error> {
    let mut excluded_paths = Vec::new();
    for line in ignore_raw.lines() {
        excluded_paths.push("--exclude");
        excluded_paths.push(line);
    }

    let backups = get_backup_list(&server).await;

    let output = Command::new("restic")
        .envs(&server.config.system.backups.restic.environment)
        .arg("--json")
        .arg("--repo")
        .arg(&server.config.system.backups.restic.repository)
        .arg("--password-file")
        .arg(&server.config.system.backups.restic.password_file)
        .arg("--retry-lock")
        .arg(format!(
            "{}s",
            server.config.system.backups.restic.retry_lock_seconds
        ))
        .arg("backup")
        .arg(&server.filesystem.base_path)
        .args(&excluded_paths)
        .arg("--tag")
        .arg(uuid.to_string())
        .arg("--group-by")
        .arg("tags")
        .output()
        .await?;

    if !output.status.success() {
        return Err(anyhow::anyhow!(
            "Failed to create Restic backup for {}: {}",
            server.filesystem.base_path.display(),
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    let mut snapshot_id = None;
    let mut total_bytes_processed = 0;
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(line) {
            if let Some("summary") = json.get("message_type").and_then(|v| v.as_str()) {
                snapshot_id = json
                    .get("snapshot_id")
                    .and_then(|v| v.as_str())
                    .map(String::from);
                total_bytes_processed = json
                    .get("total_bytes_processed")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);

                break;
            }
        }
    }

    if !backups.contains(&uuid) {
        CACHED_BACKUPS
            .write()
            .await
            .as_mut()
            .unwrap()
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
) -> Result<(), anyhow::Error> {
    let base_path = get_backup_base_path(&server, uuid).await.ok_or_else(|| {
        anyhow::anyhow!(
            "Backup with UUID {} not found for server {}",
            uuid,
            server.uuid
        )
    })?;

    let child = Command::new("restic")
        .envs(&server.config.system.backups.restic.environment)
        .arg("--json")
        .arg("--no-lock")
        .arg("--repo")
        .arg(&server.config.system.backups.restic.repository)
        .arg("--password-file")
        .arg(&server.config.system.backups.restic.password_file)
        .arg("restore")
        .arg(format!("latest:{}", base_path.display()))
        .arg("--tag")
        .arg(uuid.to_string())
        .arg("--target")
        .arg(&server.filesystem.base_path)
        .stdout(std::process::Stdio::piped())
        .spawn()?;

    let mut line_reader = tokio::io::BufReader::new(child.stdout.unwrap()).lines();

    while let Ok(Some(line)) = line_reader.next_line().await {
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(&line) {
            if let Some("status") = json.get("message_type").and_then(|v| v.as_str()) {
                let total_bytes = json
                    .get("total_bytes")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.0);
                let bytes_restored = json
                    .get("bytes_restored")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.0);
                let percent_done = json
                    .get("percent_done")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.0);
                let percent_done = (percent_done * 10000.0).round() / 100.0;

                server
                    .log_daemon(format!(
                        "(restoring): {} of {} ({}%)",
                        human_bytes(bytes_restored),
                        human_bytes(total_bytes),
                        percent_done
                    ))
                    .await;
            }
        }
    }

    Ok(())
}

pub async fn download_backup(
    server: &crate::server::Server,
    uuid: uuid::Uuid,
) -> Result<(StatusCode, HeaderMap, Body), anyhow::Error> {
    let base_path = get_backup_base_path(server, uuid).await.ok_or_else(|| {
        anyhow::anyhow!(
            "Backup with UUID {} not found for server {}",
            uuid,
            server.uuid
        )
    })?;

    let (reader, writer) = tokio::io::duplex(65536);

    let child = Command::new("restic")
        .envs(&server.config.system.backups.restic.environment)
        .arg("--json")
        .arg("--no-lock")
        .arg("--repo")
        .arg(&server.config.system.backups.restic.repository)
        .arg("--password-file")
        .arg(&server.config.system.backups.restic.password_file)
        .arg("dump")
        .arg(format!("latest:{}", base_path.display()))
        .arg("/")
        .arg("--tag")
        .arg(uuid.to_string())
        .stdout(std::process::Stdio::piped())
        .spawn()?;

    let compression_level = server.config.system.backups.compression_level;
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

    Ok((
        StatusCode::OK,
        headers,
        Body::from_stream(tokio_util::io::ReaderStream::new(
            tokio::io::BufReader::new(reader),
        )),
    ))
}

pub async fn delete_backup(
    server: &crate::server::Server,
    uuid: uuid::Uuid,
) -> Result<(), anyhow::Error> {
    let output = Command::new("restic")
        .envs(&server.config.system.backups.restic.environment)
        .arg("--repo")
        .arg(&server.config.system.backups.restic.repository)
        .arg("--password-file")
        .arg(&server.config.system.backups.restic.password_file)
        .arg("forget")
        .arg("latest")
        .arg("--tag")
        .arg(uuid.to_string())
        .arg("--group-by")
        .arg("tags")
        .arg("--prune")
        .output()
        .await?;

    if !output.status.success() {
        return Err(anyhow::anyhow!(
            "Failed to delete Restic backup for {}: {}",
            server.filesystem.base_path.display(),
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    Ok(())
}

pub async fn list_backups(
    server: &crate::server::Server,
) -> Result<Vec<uuid::Uuid>, anyhow::Error> {
    Ok(get_backup_list(server).await)
}
