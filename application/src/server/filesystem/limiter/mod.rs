pub mod btrfs_subvolume;
pub mod none;
pub mod xfs_quota;
pub mod zfs_dataset;

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
        crate::config::SystemDiskLimiterMode::ZfsDataset => {
            match zfs_dataset::setup(filesystem).await {
                Err(err) => {
                    tracing::error!(
                        path = %filesystem.base_path.display(),
                        "failed to setup zfs dataset for server, falling back to interval scan: {:#?}",
                        err
                    );
                }
                Ok(_) => {
                    tracing::info!(
                        path = %filesystem.base_path.display(),
                        "successfully setup zfs dataset for server"
                    );
                    return Ok(());
                }
            }
        }
        crate::config::SystemDiskLimiterMode::XfsQuota => {
            match xfs_quota::setup(filesystem).await {
                Err(err) => {
                    tracing::error!(
                        path = %filesystem.base_path.display(),
                        "failed to setup xfs quota for server, falling back to interval scan: {:#?}",
                        err
                    );
                }
                Ok(_) => {
                    tracing::info!(
                        path = %filesystem.base_path.display(),
                        "successfully setup xfs quota for server"
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
                    tracing::warn!(
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
        crate::config::SystemDiskLimiterMode::ZfsDataset => {
            match zfs_dataset::attach(filesystem).await {
                Err(err) => {
                    tracing::warn!(
                        path = %filesystem.base_path.display(),
                        "failed to attach zfs dataset for server, falling back to interval scan: {:#?}",
                        err
                    );
                }
                Ok(_) => {
                    tracing::info!(
                        path = %filesystem.base_path.display(),
                        "successfully attached zfs dataset for server"
                    );
                    return Ok(());
                }
            }
        }
        crate::config::SystemDiskLimiterMode::XfsQuota => {
            match xfs_quota::attach(filesystem).await {
                Err(err) => {
                    tracing::warn!(
                        path = %filesystem.base_path.display(),
                        "failed to attach xfs quota for server, falling back to interval scan: {:#?}",
                        err
                    );
                }
                Ok(_) => {
                    tracing::info!(
                        path = %filesystem.base_path.display(),
                        "successfully attached xfs quota for server"
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
        crate::config::SystemDiskLimiterMode::ZfsDataset => {
            match zfs_dataset::disk_usage(filesystem).await {
                Err(err) => {
                    tracing::debug!(
                        path = %filesystem.base_path.display(),
                        "failed to get zfs disk usage for server, falling back to interval scan: {:#?}",
                        err
                    );
                }
                Ok(usage) => return Ok(usage),
            }
        }
        crate::config::SystemDiskLimiterMode::XfsQuota => {
            match xfs_quota::disk_usage(filesystem).await {
                Err(err) => {
                    tracing::debug!(
                        path = %filesystem.base_path.display(),
                        "failed to get xfs disk usage for server, falling back to interval scan: {:#?}",
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
        crate::config::SystemDiskLimiterMode::ZfsDataset => {
            zfs_dataset::update_disk_limit(filesystem, limit).await
        }
        crate::config::SystemDiskLimiterMode::XfsQuota => {
            xfs_quota::update_disk_limit(filesystem, limit).await
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
        crate::config::SystemDiskLimiterMode::ZfsDataset => zfs_dataset::destroy(filesystem).await,
        crate::config::SystemDiskLimiterMode::XfsQuota => xfs_quota::destroy(filesystem).await,
    }
}
