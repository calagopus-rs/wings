#![allow(clippy::single_match)]

pub mod btrfs_subvolume;
pub mod none;

pub async fn setup(
    filesystem: &crate::server::filesystem::Filesystem,
) -> Result<(), std::io::Error> {
    match filesystem.config.system.disk_limiter_mode {
        crate::config::SystemDiskLimiterMode::BtrfsSubvolume => {
            match btrfs_subvolume::setup(filesystem).await {
                Err(err) => {
                    tracing::error!(
                        path = %filesystem.base_path.display(),
                        "failed to setup btrfs subvolume for server, falling back to interval scan: {:#?}",
                        err
                    );
                }
                Ok(_) => {
                    tracing::info!(
                        path = %filesystem.base_path.display(),
                        "successfully setup btrfs subvolume for server"
                    );
                    return Ok(());
                }
            }
        }
        _ => {}
    }

    none::setup(filesystem).await
}

pub async fn attach(
    filesystem: &crate::server::filesystem::Filesystem,
) -> Result<(), std::io::Error> {
    match filesystem.config.system.disk_limiter_mode {
        crate::config::SystemDiskLimiterMode::BtrfsSubvolume => {
            match btrfs_subvolume::attach(filesystem).await {
                Err(err) => {
                    tracing::error!(
                        path = %filesystem.base_path.display(),
                        "failed to attach btrfs subvolume for server, falling back to interval scan: {:#?}",
                        err
                    );
                }
                Ok(_) => {
                    tracing::info!(
                        path = %filesystem.base_path.display(),
                        "successfully attached btrfs subvolume for server"
                    );
                    return Ok(());
                }
            }
        }
        _ => {}
    }

    none::attach(filesystem).await
}

pub async fn disk_usage(
    filesystem: &crate::server::filesystem::Filesystem,
) -> Result<u64, std::io::Error> {
    match filesystem.config.system.disk_limiter_mode {
        crate::config::SystemDiskLimiterMode::BtrfsSubvolume => {
            match btrfs_subvolume::disk_usage(filesystem).await {
                Err(err) => {
                    tracing::debug!(
                        path = %filesystem.base_path.display(),
                        "failed to get btrfs disk usage for server, falling back to interval scan: {:#?}",
                        err
                    );
                }
                Ok(usage) => return Ok(usage),
            }
        }
        _ => {}
    }

    none::disk_usage(filesystem).await
}

pub async fn update_disk_limit(
    filesystem: &crate::server::filesystem::Filesystem,
    limit: u64,
) -> Result<(), std::io::Error> {
    match filesystem.config.system.disk_limiter_mode {
        crate::config::SystemDiskLimiterMode::None => {
            none::update_disk_limit(filesystem, limit).await
        }
        crate::config::SystemDiskLimiterMode::BtrfsSubvolume => {
            btrfs_subvolume::update_disk_limit(filesystem, limit).await
        }
    }
}

pub async fn destroy(
    filesystem: &crate::server::filesystem::Filesystem,
) -> Result<(), std::io::Error> {
    match filesystem.config.system.disk_limiter_mode {
        crate::config::SystemDiskLimiterMode::None => none::destroy(filesystem).await,
        crate::config::SystemDiskLimiterMode::BtrfsSubvolume => {
            btrfs_subvolume::destroy(filesystem).await
        }
    }
}
