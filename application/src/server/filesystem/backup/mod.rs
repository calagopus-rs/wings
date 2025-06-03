use crate::{models::DirectoryEntry, server::backup::BackupAdapter};
use std::path::Path;

mod btrfs;
mod ddup_bak;
mod zfs;

pub async fn list(
    adapter: BackupAdapter,
    server: &crate::server::Server,
    uuid: uuid::Uuid,
    path: &Path,
) -> std::io::Result<Vec<DirectoryEntry>> {
    match adapter {
        BackupAdapter::DdupBak => ddup_bak::list(server, uuid, path).await,
        BackupAdapter::Btrfs => btrfs::list(server, uuid, path).await,
        BackupAdapter::Zfs => zfs::list(server, uuid, path).await,
        _ => Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "This backup adapter does not support listing files",
        )),
    }
}

pub async fn reader(
    adapter: BackupAdapter,
    server: &crate::server::Server,
    uuid: uuid::Uuid,
    path: &Path,
) -> std::io::Result<(Box<dyn std::io::Read + Send>, u64)> {
    match adapter {
        BackupAdapter::DdupBak => ddup_bak::reader(server, uuid, path).await,
        BackupAdapter::Btrfs => btrfs::reader(server, uuid, path).await,
        BackupAdapter::Zfs => zfs::reader(server, uuid, path).await,
        _ => Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "This backup adapter does not support reading files",
        )),
    }
}

pub async fn directory_reader(
    adapter: BackupAdapter,
    server: &crate::server::Server,
    uuid: uuid::Uuid,
    path: &Path,
) -> std::io::Result<tokio::io::DuplexStream> {
    match adapter {
        BackupAdapter::DdupBak => ddup_bak::directory_reader(server, uuid, path).await,
        BackupAdapter::Btrfs => btrfs::directory_reader(server, uuid, path).await,
        BackupAdapter::Zfs => zfs::directory_reader(server, uuid, path).await,
        _ => Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "This backup adapter does not support directory reading",
        )),
    }
}
