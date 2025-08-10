use crate::{
    models::DirectoryEntry, remote::backups::RawServerBackup, response::ApiResponse,
    server::filesystem::archive::StreamableArchiveFormat,
};
use std::{
    path::PathBuf,
    sync::{Arc, atomic::AtomicU64},
};
use tokio::io::AsyncRead;

pub mod adapters;
pub mod manager;

pub enum Backup {
    Wings(adapters::wings::WingsBackup),
    S3(adapters::s3::S3Backup),
    DdupBak(adapters::ddup_bak::DdupBakBackup),
    Btrfs(adapters::btrfs::BtrfsBackup),
    Zfs(adapters::zfs::ZfsBackup),
    Restic(adapters::restic::ResticBackup),
}

impl Backup {
    pub fn uuid(&self) -> uuid::Uuid {
        match self {
            Backup::Wings(backup) => backup.uuid(),
            Backup::S3(backup) => backup.uuid(),
            Backup::DdupBak(backup) => backup.uuid(),
            Backup::Btrfs(backup) => backup.uuid(),
            Backup::Zfs(backup) => backup.uuid(),
            Backup::Restic(backup) => backup.uuid(),
        }
    }

    #[inline]
    pub fn adapter(&self) -> adapters::BackupAdapter {
        match self {
            Backup::Wings(_) => adapters::BackupAdapter::Wings,
            Backup::S3(_) => adapters::BackupAdapter::S3,
            Backup::DdupBak(_) => adapters::BackupAdapter::DdupBak,
            Backup::Btrfs(_) => adapters::BackupAdapter::Btrfs,
            Backup::Zfs(_) => adapters::BackupAdapter::Zfs,
            Backup::Restic(_) => adapters::BackupAdapter::Restic,
        }
    }

    pub async fn download(
        &self,
        config: &Arc<crate::config::Config>,
        archive_format: StreamableArchiveFormat,
    ) -> Result<ApiResponse, anyhow::Error> {
        match self {
            Backup::Wings(backup) => backup.download(config, archive_format).await,
            Backup::S3(backup) => backup.download(config, archive_format).await,
            Backup::DdupBak(backup) => backup.download(config, archive_format).await,
            Backup::Btrfs(backup) => backup.download(config, archive_format).await,
            Backup::Zfs(backup) => backup.download(config, archive_format).await,
            Backup::Restic(backup) => backup.download(config, archive_format).await,
        }
    }

    pub async fn restore(
        &self,
        server: &crate::server::Server,
        progress: Arc<AtomicU64>,
        total: Arc<AtomicU64>,
        download_url: Option<String>,
    ) -> Result<(), anyhow::Error> {
        match self {
            Backup::Wings(backup) => backup.restore(server, progress, total, download_url).await,
            Backup::S3(backup) => backup.restore(server, progress, total, download_url).await,
            Backup::DdupBak(backup) => backup.restore(server, progress, total, download_url).await,
            Backup::Btrfs(backup) => backup.restore(server, progress, total, download_url).await,
            Backup::Zfs(backup) => backup.restore(server, progress, total, download_url).await,
            Backup::Restic(backup) => backup.restore(server, progress, total, download_url).await,
        }
    }

    pub async fn delete(&self, config: &Arc<crate::config::Config>) -> Result<(), anyhow::Error> {
        match self {
            Backup::Wings(backup) => backup.delete(config).await,
            Backup::S3(backup) => backup.delete(config).await,
            Backup::DdupBak(backup) => backup.delete(config).await,
            Backup::Btrfs(backup) => backup.delete(config).await,
            Backup::Zfs(backup) => backup.delete(config).await,
            Backup::Restic(backup) => backup.delete(config).await,
        }
    }

    async fn browse(&self, server: &crate::server::Server) -> Result<BrowseBackup, anyhow::Error> {
        match self {
            Backup::Wings(backup) => backup.browse(server).await,
            Backup::S3(backup) => backup.browse(server).await,
            Backup::DdupBak(backup) => backup.browse(server).await,
            Backup::Btrfs(backup) => backup.browse(server).await,
            Backup::Zfs(backup) => backup.browse(server).await,
            Backup::Restic(backup) => backup.browse(server).await,
        }
    }
}

pub enum BrowseBackup {
    Wings(adapters::wings::BrowseWingsBackup),
    DdupBak(adapters::ddup_bak::BrowseDdupBakBackup),
    Btrfs(adapters::btrfs::BrowseBtrfsBackup),
    Zfs(adapters::zfs::BrowseZfsBackup),
    Restic(adapters::restic::BrowseResticBackup),
}

impl BrowseBackup {
    pub async fn read_dir(
        &self,
        path: PathBuf,
        per_page: Option<usize>,
        page: usize,
        is_ignored: impl Fn(PathBuf, bool) -> bool + Send + Sync + 'static,
    ) -> Result<(usize, Vec<DirectoryEntry>), anyhow::Error> {
        match self {
            BrowseBackup::Wings(backup) => backup.read_dir(path, per_page, page, is_ignored).await,
            BrowseBackup::DdupBak(backup) => {
                backup.read_dir(path, per_page, page, is_ignored).await
            }
            BrowseBackup::Btrfs(backup) => backup.read_dir(path, per_page, page, is_ignored).await,
            BrowseBackup::Zfs(backup) => backup.read_dir(path, per_page, page, is_ignored).await,
            BrowseBackup::Restic(backup) => backup.read_dir(path, per_page, page, is_ignored).await,
        }
    }

    pub async fn read_file(
        &'_ self,
        path: PathBuf,
    ) -> Result<(u64, Box<dyn AsyncRead + Unpin + Send>), anyhow::Error> {
        match self {
            BrowseBackup::Wings(backup) => backup.read_file(path).await,
            BrowseBackup::DdupBak(backup) => backup.read_file(path).await,
            BrowseBackup::Btrfs(backup) => backup.read_file(path).await,
            BrowseBackup::Zfs(backup) => backup.read_file(path).await,
            BrowseBackup::Restic(backup) => backup.read_file(path).await,
        }
    }

    pub async fn read_directory_archive(
        &self,
        path: PathBuf,
        archive_format: StreamableArchiveFormat,
    ) -> Result<tokio::io::DuplexStream, anyhow::Error> {
        match self {
            BrowseBackup::Wings(backup) => {
                backup.read_directory_archive(path, archive_format).await
            }
            BrowseBackup::DdupBak(backup) => {
                backup.read_directory_archive(path, archive_format).await
            }
            BrowseBackup::Btrfs(backup) => {
                backup.read_directory_archive(path, archive_format).await
            }
            BrowseBackup::Zfs(backup) => backup.read_directory_archive(path, archive_format).await,
            BrowseBackup::Restic(backup) => {
                backup.read_directory_archive(path, archive_format).await
            }
        }
    }

    pub async fn read_files_archive(
        &self,
        path: PathBuf,
        file_paths: Vec<PathBuf>,
        archive_format: StreamableArchiveFormat,
    ) -> Result<tokio::io::DuplexStream, anyhow::Error> {
        match self {
            BrowseBackup::Wings(backup) => {
                backup
                    .read_files_archive(path, file_paths, archive_format)
                    .await
            }
            BrowseBackup::DdupBak(backup) => {
                backup
                    .read_files_archive(path, file_paths, archive_format)
                    .await
            }
            BrowseBackup::Btrfs(backup) => {
                backup
                    .read_files_archive(path, file_paths, archive_format)
                    .await
            }
            BrowseBackup::Zfs(backup) => {
                backup
                    .read_files_archive(path, file_paths, archive_format)
                    .await
            }
            BrowseBackup::Restic(backup) => {
                backup
                    .read_files_archive(path, file_paths, archive_format)
                    .await
            }
        }
    }
}

#[async_trait::async_trait]
pub trait BackupFindExt {
    async fn exists(
        config: &Arc<crate::config::Config>,
        uuid: uuid::Uuid,
    ) -> Result<bool, anyhow::Error>;
    async fn find(
        config: &Arc<crate::config::Config>,
        uuid: uuid::Uuid,
    ) -> Result<Option<Backup>, anyhow::Error>;
}

#[async_trait::async_trait]
pub trait BackupCreateExt {
    async fn create(
        server: &crate::server::Server,
        uuid: uuid::Uuid,
        progress: Arc<AtomicU64>,
        total: Arc<AtomicU64>,
        ignore: ignore::gitignore::Gitignore,
        ignore_raw: String,
    ) -> Result<RawServerBackup, anyhow::Error>;
}

#[async_trait::async_trait]
pub trait BackupExt {
    fn uuid(&self) -> uuid::Uuid;

    async fn download(
        &self,
        config: &Arc<crate::config::Config>,
        archive_format: StreamableArchiveFormat,
    ) -> Result<ApiResponse, anyhow::Error>;

    async fn restore(
        &self,
        server: &crate::server::Server,
        progress: Arc<AtomicU64>,
        total: Arc<AtomicU64>,
        download_url: Option<String>,
    ) -> Result<(), anyhow::Error>;
    async fn delete(&self, config: &Arc<crate::config::Config>) -> Result<(), anyhow::Error>;

    async fn browse(&self, server: &crate::server::Server) -> Result<BrowseBackup, anyhow::Error>;
}

#[async_trait::async_trait]
pub trait BackupCleanExt {
    async fn clean(server: &crate::server::Server, uuid: uuid::Uuid) -> Result<(), anyhow::Error>;
}

#[async_trait::async_trait]
pub trait BackupBrowseExt {
    async fn read_dir(
        &self,
        path: PathBuf,
        per_page: Option<usize>,
        page: usize,
        is_ignored: impl Fn(PathBuf, bool) -> bool + Send + Sync + 'static,
    ) -> Result<(usize, Vec<DirectoryEntry>), anyhow::Error>;
    async fn read_file(
        &self,
        path: PathBuf,
    ) -> Result<(u64, Box<dyn AsyncRead + Unpin + Send>), anyhow::Error>;

    async fn read_directory_archive(
        &self,
        path: PathBuf,
        archive_format: StreamableArchiveFormat,
    ) -> Result<tokio::io::DuplexStream, anyhow::Error>;
    async fn read_files_archive(
        &self,
        path: PathBuf,
        file_paths: Vec<PathBuf>,
        archive_format: StreamableArchiveFormat,
    ) -> Result<tokio::io::DuplexStream, anyhow::Error>;
}
