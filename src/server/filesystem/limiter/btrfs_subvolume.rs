use tokio::process::Command;

pub async fn setup(
    filesystem: &crate::server::filesystem::Filesystem,
) -> Result<(), std::io::Error> {
    tracing::debug!(
        path = %filesystem.base_path.display(),
        "setting up btrfs disk limiter for volume"
    );

    if !filesystem.base_path.exists() {
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
    }

    Ok(())
}

pub async fn disk_usage(
    filesystem: &crate::server::filesystem::Filesystem,
) -> Result<u64, std::io::Error> {
    let output = Command::new("btrfs")
        .arg("qgroup")
        .arg("show")
        .arg(&filesystem.base_path)
        .arg("--raw")
        .output()
        .await?;

    if !output.status.success() {
        return Err(std::io::Error::other(format!(
            "Failed to get Btrfs disk usage for {}: {}",
            filesystem.base_path.display(),
            String::from_utf8_lossy(&output.stderr)
        )));
    }

    let output_str = String::from_utf8_lossy(&output.stdout);
    let server_uuid_str = filesystem
        .base_path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    for line in output_str.lines() {
        if !line.ends_with(&server_uuid_str) {
            continue;
        }

        if let Some(used_space) = line.split_whitespace().nth(1) {
            if let Ok(used_space) = used_space.parse::<u64>() {
                return Ok(used_space);
            }
        }
    }

    Err(std::io::Error::other(format!(
        "Failed to parse Btrfs disk usage for {}",
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

    let output = Command::new("btrfs")
        .arg("subvolume")
        .arg("delete")
        .arg(&filesystem.base_path)
        .output()
        .await?;

    if !output.status.success() {
        return Err(std::io::Error::other(format!(
            "Failed to destroy Btrfs subvolume for {}: {}",
            filesystem.base_path.display(),
            String::from_utf8_lossy(&output.stderr)
        )));
    }

    Ok(())
}
