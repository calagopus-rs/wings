use crate::{
    models::DirectoryEntry,
    server::backup::{BackupAdapter, InternalBackup},
};
use std::path::Path;

mod btrfs;
mod ddup_bak;
mod restic;
mod wings;
mod zfs;

pub async fn list(
    backup: InternalBackup,
    server: &crate::server::Server,
    path: &Path,
    per_page: Option<usize>,
    page: usize,
) -> Result<(usize, Vec<DirectoryEntry>), anyhow::Error> {
    let path = super::Filesystem::resolve_path(path);

    match backup.adapter {
        BackupAdapter::Wings => wings::list(server, backup.uuid, path, per_page, page).await,
        BackupAdapter::DdupBak => ddup_bak::list(server, backup.uuid, path, per_page, page).await,
        BackupAdapter::Btrfs => btrfs::list(server, backup.uuid, path, per_page, page).await,
        BackupAdapter::Zfs => zfs::list(server, backup.uuid, path, per_page, page).await,
        BackupAdapter::Restic => restic::list(server, backup.uuid, path, per_page, page).await,
        _ => Err(anyhow::anyhow!(
            "This backup adapter does not support listing files"
        )),
    }
}

pub async fn reader(
    backup: InternalBackup,
    server: &crate::server::Server,
    path: &Path,
) -> Result<(Box<dyn tokio::io::AsyncRead + Send>, u64), anyhow::Error> {
    let path = super::Filesystem::resolve_path(path);

    match backup.adapter {
        BackupAdapter::Wings => wings::reader(server, backup.uuid, path).await,
        BackupAdapter::DdupBak => ddup_bak::reader(server, backup.uuid, path).await,
        BackupAdapter::Btrfs => btrfs::reader(server, backup.uuid, path).await,
        BackupAdapter::Zfs => zfs::reader(server, backup.uuid, path).await,
        BackupAdapter::Restic => restic::reader(server, backup.uuid, path).await,
        _ => Err(anyhow::anyhow!(
            "This backup adapter does not support reading files"
        )),
    }
}

pub async fn directory_reader(
    backup: InternalBackup,
    server: &crate::server::Server,
    path: &Path,
) -> Result<tokio::io::DuplexStream, anyhow::Error> {
    let path = super::Filesystem::resolve_path(path);

    match backup.adapter {
        BackupAdapter::Wings => wings::directory_reader(server, backup.uuid, path).await,
        BackupAdapter::DdupBak => ddup_bak::directory_reader(server, backup.uuid, path).await,
        BackupAdapter::Btrfs => btrfs::directory_reader(server, backup.uuid, path).await,
        BackupAdapter::Zfs => zfs::directory_reader(server, backup.uuid, path).await,
        BackupAdapter::Restic => restic::directory_reader(server, backup.uuid, path).await,
        _ => Err(anyhow::anyhow!(
            "This backup adapter does not support directory reading"
        )),
    }
}
