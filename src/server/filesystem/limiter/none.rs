pub async fn setup(
    filesystem: &crate::server::filesystem::Filesystem,
) -> Result<(), std::io::Error> {
    tracing::debug!(
        path = %filesystem.base_path.display(),
        "setting up no disk limiter for volume"
    );

    tokio::fs::create_dir_all(&filesystem.base_path).await?;

    Ok(())
}

pub async fn attach(
    filesystem: &crate::server::filesystem::Filesystem,
) -> Result<(), std::io::Error> {
    tracing::debug!(
        path = %filesystem.base_path.display(),
        "attaching no disk limiter for volume"
    );

    Ok(())
}

pub async fn disk_usage(
    filesystem: &crate::server::filesystem::Filesystem,
) -> Result<u64, std::io::Error> {
    Ok(filesystem
        .disk_usage_cached
        .load(std::sync::atomic::Ordering::Relaxed))
}

pub async fn update_disk_limit(
    _filesystem: &crate::server::filesystem::Filesystem,
    _limit: u64,
) -> Result<(), std::io::Error> {
    Ok(())
}

pub async fn destroy(
    filesystem: &crate::server::filesystem::Filesystem,
) -> Result<(), std::io::Error> {
    tokio::fs::remove_dir_all(&filesystem.base_path).await
}
