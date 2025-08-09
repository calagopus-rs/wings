use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, LazyLock},
};
use tokio::{process::Command, sync::RwLock};

type DiskUsageMap = HashMap<String, (PathBuf, String, i64)>;
static DISK_USAGE: LazyLock<Arc<RwLock<DiskUsageMap>>> = LazyLock::new(|| {
    let disk_usage: Arc<RwLock<DiskUsageMap>> = Arc::new(RwLock::new(HashMap::new()));

    tokio::spawn({
        let disk_usage = Arc::clone(&disk_usage);

        async move {
            loop {
                for (server, (path, dataset_name, server_usage)) in
                    disk_usage.write().await.iter_mut()
                {
                    *server_usage = -1;

                    let pool_name = match get_pool_from_path(path).await {
                        Ok(pool) => pool,
                        Err(e) => {
                            tracing::error!(server = server, "failed to get ZFS pool name: {}", e);
                            continue;
                        }
                    };

                    if dataset_name.is_empty() {
                        *dataset_name = format!("{pool_name}/server-{server}");
                    }

                    let output = Command::new("zfs")
                        .arg("list")
                        .arg("-p")
                        .arg("-o")
                        .arg("used,referenced")
                        .arg(dataset_name)
                        .output()
                        .await;

                    match output {
                        Ok(output) if output.status.success() => {
                            let output_str = String::from_utf8_lossy(&output.stdout);

                            if let Some(line) = output_str.lines().nth(1)
                                && let Some(used_space) = line.split_whitespace().nth(1)
                                && let Ok(used_space) = used_space.parse::<i64>()
                            {
                                *server_usage = used_space;
                            }
                        }
                        Ok(output) => {
                            tracing::error!(
                                server = server,
                                "failed to get ZFS disk usage: {}",
                                String::from_utf8_lossy(&output.stderr)
                            );
                        }
                        Err(err) => {
                            tracing::error!("error executing zfs command: {:#?}", err);
                        }
                    }
                }

                tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
            }
        }
    });

    disk_usage
});

async fn get_pool_from_path(path: &Path) -> Result<String, std::io::Error> {
    let output = Command::new("zfs")
        .arg("list")
        .arg("-o")
        .arg("name,mountpoint")
        .output()
        .await?;

    if !output.status.success() {
        return Err(std::io::Error::other(format!(
            "Failed to list ZFS datasets: {}",
            String::from_utf8_lossy(&output.stderr)
        )));
    }

    let output_str = String::from_utf8_lossy(&output.stdout);
    let path_str = path.to_string_lossy();

    let mut best_match = None;
    let mut best_match_len = 0;

    for line in output_str.lines().skip(1) {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() >= 2 {
            let dataset = parts[0];
            let mountpoint = parts[1];

            if path_str.starts_with(mountpoint) && mountpoint.len() > best_match_len {
                best_match = Some(dataset.to_string());
                best_match_len = mountpoint.len();
            }
        }
    }

    if let Some(dataset) = best_match {
        if let Some(pool_end) = dataset.find('/') {
            return Ok(dataset[0..pool_end].to_string());
        }

        return Ok(dataset);
    }

    Err(std::io::Error::other(format!(
        "No ZFS pool found for path: {path_str}"
    )))
}

pub async fn setup(
    filesystem: &crate::server::filesystem::Filesystem,
) -> Result<(), std::io::Error> {
    tracing::debug!(
        path = %filesystem.base_path.display(),
        "setting up zfs dataset for volume"
    );

    let pool_name = get_pool_from_path(&filesystem.base_path).await?;
    let dataset_name = format!("{}/server-{}", pool_name, filesystem.uuid);

    if tokio::fs::metadata(&filesystem.base_path).await.is_err() {
        let output = Command::new("zfs")
            .arg("create")
            .arg("-o")
            .arg(format!("mountpoint={}", filesystem.base_path.display()))
            .arg(&dataset_name)
            .output()
            .await?;

        if !output.status.success() {
            return Err(std::io::Error::other(format!(
                "Failed to create ZFS dataset for {}: {}",
                filesystem.base_path.display(),
                String::from_utf8_lossy(&output.stderr)
            )));
        }
    }

    DISK_USAGE.write().await.insert(
        filesystem.uuid.to_string(),
        (filesystem.base_path.clone(), dataset_name, 0),
    );

    Ok(())
}

pub async fn attach(
    filesystem: &crate::server::filesystem::Filesystem,
) -> Result<(), std::io::Error> {
    tracing::debug!(
        path = %filesystem.base_path.display(),
        "attaching zfs disk limiter for volume"
    );

    let pool_name = get_pool_from_path(&filesystem.base_path).await?;
    let dataset_name = format!("{}/server-{}", pool_name, filesystem.uuid);

    DISK_USAGE.write().await.insert(
        filesystem.uuid.to_string(),
        (filesystem.base_path.clone(), dataset_name, 0),
    );

    Ok(())
}

pub async fn disk_usage(
    filesystem: &crate::server::filesystem::Filesystem,
) -> Result<u64, std::io::Error> {
    if let Some(usage) = DISK_USAGE.read().await.get(&filesystem.uuid.to_string())
        && usage.2 >= 0
    {
        return Ok(usage.2 as u64);
    }

    Err(std::io::Error::other(format!(
        "Failed to load ZFS disk usage for {}",
        filesystem.base_path.display()
    )))
}

pub async fn update_disk_limit(
    filesystem: &crate::server::filesystem::Filesystem,
    limit: u64,
) -> Result<(), std::io::Error> {
    tracing::debug!(
        path = %filesystem.base_path.display(),
        limit = limit,
        "setting zfs disk limit"
    );

    let dataset_name = match DISK_USAGE.read().await.get(&filesystem.uuid.to_string()) {
        Some(usage) => usage.1.clone(),
        None => {
            let pool_name = get_pool_from_path(&filesystem.base_path).await?;
            format!("{}/server-{}", pool_name, filesystem.uuid)
        }
    };

    let output = Command::new("zfs")
        .arg("set")
        .arg(format!(
            "refquota={}",
            if limit == 0 {
                "none".to_string()
            } else {
                format!("{}M", limit / 1024 / 1024)
            }
        ))
        .arg(&dataset_name)
        .output()
        .await?;

    if !output.status.success() {
        return Err(std::io::Error::other(format!(
            "Failed to set ZFS quota for {}: {}",
            filesystem.base_path.display(),
            String::from_utf8_lossy(&output.stderr)
        )));
    }

    Ok(())
}

pub async fn destroy(
    filesystem: &crate::server::filesystem::Filesystem,
) -> Result<(), std::io::Error> {
    tracing::debug!(
        path = %filesystem.base_path.display(),
        "destroying zfs dataset for server"
    );

    let dataset_name = match DISK_USAGE.read().await.get(&filesystem.uuid.to_string()) {
        Some(usage) => usage.1.clone(),
        None => {
            let pool_name = get_pool_from_path(&filesystem.base_path).await?;
            format!("{}/server-{}", pool_name, filesystem.uuid)
        }
    };

    let output = Command::new("zfs")
        .arg("destroy")
        .arg("-r")
        .arg(&dataset_name)
        .output()
        .await?;

    if !output.status.success() {
        tokio::fs::remove_dir_all(&filesystem.base_path).await.ok();
    }

    DISK_USAGE
        .write()
        .await
        .remove(&filesystem.uuid.to_string());

    Ok(())
}
