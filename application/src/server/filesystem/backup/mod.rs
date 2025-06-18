use crate::{
    models::DirectoryEntry,
    server::backup::{BackupAdapter, InternalBackup},
};
use std::path::Path;

mod btrfs;
mod ddup_bak;
mod zfs;

pub async fn list(
    backup: InternalBackup,
    server: &crate::server::Server,
    path: &Path,
) -> std::io::Result<Vec<DirectoryEntry>> {
    match backup.adapter {
        BackupAdapter::DdupBak => ddup_bak::list(server, backup.uuid, path).await,
        BackupAdapter::Btrfs => btrfs::list(server, backup.uuid, path).await,
        BackupAdapter::Zfs => zfs::list(server, backup.uuid, path).await,
        _ => Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "This backup adapter does not support listing files",
        )),
    }
}

pub async fn reader(
    backup: InternalBackup,
    server: &crate::server::Server,
    path: &Path,
) -> std::io::Result<(Box<dyn tokio::io::AsyncRead + Send>, u64)> {
    match backup.adapter {
        BackupAdapter::DdupBak => ddup_bak::reader(server, backup.uuid, path).await,
        BackupAdapter::Btrfs => btrfs::reader(server, backup.uuid, path).await,
        BackupAdapter::Zfs => zfs::reader(server, backup.uuid, path).await,
        _ => Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "This backup adapter does not support reading files",
        )),
    }
}

pub async fn directory_reader(
    backup: InternalBackup,
    server: &crate::server::Server,
    path: &Path,
) -> std::io::Result<tokio::io::DuplexStream> {
    match backup.adapter {
        BackupAdapter::DdupBak => ddup_bak::directory_reader(server, backup.uuid, path).await,
        BackupAdapter::Btrfs => btrfs::directory_reader(server, backup.uuid, path).await,
        BackupAdapter::Zfs => zfs::directory_reader(server, backup.uuid, path).await,
        _ => Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "This backup adapter does not support directory reading",
        )),
    }
}
