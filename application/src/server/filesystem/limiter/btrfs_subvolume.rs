use std::{
    collections::HashMap,
    path::PathBuf,
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
                let mut usage = String::new();

                for (server, (path, qgroup, server_usage)) in disk_usage.write().await.iter_mut() {
                    *server_usage = -1;

                    if let Some(line) = usage.lines().find(|line| line.ends_with(server)) {
                        let mut line = line.split_whitespace();

                        *qgroup = line.next().unwrap_or("").to_string();

                        if let Some(used_space) = line.next()
                            && let Ok(used_space) = used_space.parse::<i64>()
                        {
                            *server_usage = used_space;
                            continue;
                        }
                    }

                    let output = Command::new("btrfs")
                        .arg("qgroup")
                        .arg("show")
                        .arg("--raw")
                        .arg(path)
                        .output()
                        .await;
                    match output {
                        Ok(output) if output.status.success() => {
                            let output_str = String::from_utf8_lossy(&output.stdout);
                            for line in output_str.lines() {
                                if line.ends_with(server)
                                    && let Some(used_space) = line.split_whitespace().nth(1)
                                    && let Ok(used_space) = used_space.parse::<i64>()
                                {
                                    *server_usage = used_space;
                                    break;
                                }
                            }

                            usage.push_str(&output_str);
                        }
                        Ok(output) => {
                            tracing::error!(
                                server = server,
                                "failed to get Btrfs disk usage: {}",
                                String::from_utf8_lossy(&output.stderr)
                            );
                        }
                        Err(err) => {
                            tracing::error!("error executing btrfs command: {:#?}", err);
                        }
                    }
                }

                tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
            }
        }
    });

    disk_usage
});

pub async fn setup(
    filesystem: &crate::server::filesystem::Filesystem,
) -> Result<(), std::io::Error> {
    tracing::debug!(
        path = %filesystem.base_path.display(),
        "setting up btrfs disk limiter for volume"
    );

    if tokio::fs::metadata(&filesystem.base_path).await.is_err() {
        let output = Command::new("btrfs")
            .arg("subvolume")
            .arg("create")
            .arg(&filesystem.base_path)
            .output()
            .await?;

        if !output.status.success() {
            return Err(std::io::Error::other(format!(
                "Failed to create Btrfs subvolume for {}: {}",
                filesystem.base_path.display(),
                String::from_utf8_lossy(&output.stderr)
            )));
        }

        let output = Command::new("btrfs")
            .arg("quota")
            .arg("enable")
            .arg(&filesystem.base_path)
            .output()
            .await?;

        if !output.status.success() {
            return Err(std::io::Error::other(format!(
                "Failed to enable Btrfs quota for {}: {}",
                filesystem.base_path.display(),
                String::from_utf8_lossy(&output.stderr)
            )));
        }
    }

    DISK_USAGE.write().await.insert(
        filesystem.uuid.to_string(),
        (filesystem.base_path.clone(), "".to_string(), 0),
    );

    Ok(())
}

pub async fn attach(
    filesystem: &crate::server::filesystem::Filesystem,
) -> Result<(), std::io::Error> {
    tracing::debug!(
        path = %filesystem.base_path.display(),
        "attaching btrfs disk limiter for volume"
    );

    DISK_USAGE.write().await.insert(
        filesystem.uuid.to_string(),
        (filesystem.base_path.clone(), "".to_string(), 0),
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
        "Failed to load Btrfs disk usage for {}",
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
        "setting btrfs disk limit"
    );

    let output = Command::new("btrfs")
        .arg("qgroup")
        .arg("limit")
        .arg(if limit == 0 {
            "none".to_string()
        } else {
            format!("{}M", limit / 1024 / 1024)
        })
        .arg(&filesystem.base_path)
        .output()
        .await?;

    if !output.status.success() {
        return Err(std::io::Error::other(format!(
            "Failed to set Btrfs disk limit for {}: {}",
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
        "destroying btrfs subvolume for server"
    );

    if let Some(usage) = DISK_USAGE.read().await.get(&filesystem.uuid.to_string()) {
        tracing::debug!(
            path = %filesystem.base_path.display(),
            qgroup = %usage.1,
            "destroying btrfs qgroup for server"
        );

        let output = Command::new("btrfs")
            .arg("qgroup")
            .arg("destroy")
            .arg(&usage.1)
            .arg(&filesystem.base_path)
            .output()
            .await?;

        if !output.status.success() {
            tracing::error!(
                path = %filesystem.base_path.display(),
                qgroup = %usage.1,
                "failed to destroy btrfs qgroup: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }

    let output = Command::new("btrfs")
        .arg("subvolume")
        .arg("delete")
        .arg(&filesystem.base_path)
        .output()
        .await?;

    if !output.status.success() {
        tokio::fs::remove_dir_all(&filesystem.base_path).await?;
    }

    DISK_USAGE
        .write()
        .await
        .remove(&filesystem.uuid.to_string());

    Ok(())
}
