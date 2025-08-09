use crate::{
    remote::backups::RawServerBackup,
    server::backup::{Backup, BackupCleanExt, BackupCreateExt, BackupFindExt},
};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, atomic::AtomicU64};
use utoipa::ToSchema;

pub mod btrfs;
pub mod ddup_bak;
pub mod restic;
pub mod s3;
pub mod wings;
pub mod zfs;

#[derive(ToSchema, Clone, Copy, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "kebab-case")]
#[schema(rename_all = "kebab-case")]
pub enum BackupAdapter {
    Wings,
    S3,
    DdupBak,
    Btrfs,
    Zfs,
    Restic,
}

impl BackupAdapter {
    #[inline]
    pub fn variants() -> &'static [Self] {
        &[
            Self::Wings,
            Self::S3,
            Self::DdupBak,
            Self::Btrfs,
            Self::Zfs,
            Self::Restic,
        ]
    }
}

impl BackupAdapter {
    pub async fn exists_any(
        config: &Arc<crate::config::Config>,
        uuid: uuid::Uuid,
    ) -> Result<bool, anyhow::Error> {
        for adapter in Self::variants() {
            if match adapter {
                BackupAdapter::Wings => {
                    <wings::WingsBackup as BackupFindExt>::exists(config, uuid).await
                }
                BackupAdapter::S3 => <s3::S3Backup as BackupFindExt>::exists(config, uuid).await,
                BackupAdapter::DdupBak => {
                    <ddup_bak::DdupBakBackup as BackupFindExt>::exists(config, uuid).await
                }
                BackupAdapter::Btrfs => {
                    <btrfs::BtrfsBackup as BackupFindExt>::exists(config, uuid).await
                }
                BackupAdapter::Zfs => <zfs::ZfsBackup as BackupFindExt>::exists(config, uuid).await,
                BackupAdapter::Restic => {
                    <restic::ResticBackup as BackupFindExt>::exists(config, uuid).await
                }
            }? {
                return Ok(true);
            }
        }

        Ok(false)
    }

    pub async fn exists(
        self,
        config: &Arc<crate::config::Config>,
        uuid: uuid::Uuid,
    ) -> Result<bool, anyhow::Error> {
        match self {
            BackupAdapter::Wings => wings::WingsBackup::exists(config, uuid).await,
            BackupAdapter::S3 => s3::S3Backup::exists(config, uuid).await,
            BackupAdapter::DdupBak => ddup_bak::DdupBakBackup::exists(config, uuid).await,
            BackupAdapter::Btrfs => btrfs::BtrfsBackup::exists(config, uuid).await,
            BackupAdapter::Zfs => zfs::ZfsBackup::exists(config, uuid).await,
            BackupAdapter::Restic => restic::ResticBackup::exists(config, uuid).await,
        }
    }

    pub async fn find_all(
        config: &std::sync::Arc<crate::config::Config>,
        uuid: uuid::Uuid,
    ) -> Result<Option<(Self, Backup)>, anyhow::Error> {
        for adapter in Self::variants() {
            if let Some(backup) = match adapter {
                BackupAdapter::Wings => {
                    <wings::WingsBackup as BackupFindExt>::find(config, uuid).await
                }
                BackupAdapter::S3 => Ok(None),
                BackupAdapter::DdupBak => {
                    <ddup_bak::DdupBakBackup as BackupFindExt>::find(config, uuid).await
                }
                BackupAdapter::Btrfs => {
                    <btrfs::BtrfsBackup as BackupFindExt>::find(config, uuid).await
                }
                BackupAdapter::Zfs => <zfs::ZfsBackup as BackupFindExt>::find(config, uuid).await,
                BackupAdapter::Restic => {
                    <restic::ResticBackup as BackupFindExt>::find(config, uuid).await
                }
            }? {
                return Ok(Some((*adapter, backup)));
            }
        }

        Ok(None)
    }

    pub async fn find(
        self,
        config: &Arc<crate::config::Config>,
        uuid: uuid::Uuid,
    ) -> Result<Option<Backup>, anyhow::Error> {
        match self {
            BackupAdapter::Wings => wings::WingsBackup::find(config, uuid).await,
            BackupAdapter::S3 => s3::S3Backup::find(config, uuid).await,
            BackupAdapter::DdupBak => ddup_bak::DdupBakBackup::find(config, uuid).await,
            BackupAdapter::Btrfs => btrfs::BtrfsBackup::find(config, uuid).await,
            BackupAdapter::Zfs => zfs::ZfsBackup::find(config, uuid).await,
            BackupAdapter::Restic => restic::ResticBackup::find(config, uuid).await,
        }
    }

    pub async fn create(
        self,
        server: &crate::server::Server,
        uuid: uuid::Uuid,
        progress: Arc<AtomicU64>,
        total: Arc<AtomicU64>,
        ignore: ignore::gitignore::Gitignore,
        ignore_raw: String,
    ) -> Result<RawServerBackup, anyhow::Error> {
        match self {
            BackupAdapter::Wings => {
                wings::WingsBackup::create(server, uuid, progress, total, ignore, ignore_raw).await
            }
            BackupAdapter::S3 => {
                s3::S3Backup::create(server, uuid, progress, total, ignore, ignore_raw).await
            }
            BackupAdapter::DdupBak => {
                ddup_bak::DdupBakBackup::create(server, uuid, progress, total, ignore, ignore_raw)
                    .await
            }
            BackupAdapter::Btrfs => {
                btrfs::BtrfsBackup::create(server, uuid, progress, total, ignore, ignore_raw).await
            }
            BackupAdapter::Zfs => {
                zfs::ZfsBackup::create(server, uuid, progress, total, ignore, ignore_raw).await
            }
            BackupAdapter::Restic => {
                restic::ResticBackup::create(server, uuid, progress, total, ignore, ignore_raw)
                    .await
            }
        }
    }

    pub async fn clean(
        self,
        server: &crate::server::Server,
        uuid: uuid::Uuid,
    ) -> Result<(), anyhow::Error> {
        match self {
            BackupAdapter::Wings => wings::WingsBackup::clean(server, uuid).await,
            BackupAdapter::S3 => s3::S3Backup::clean(server, uuid).await,
            BackupAdapter::DdupBak => ddup_bak::DdupBakBackup::clean(server, uuid).await,
            BackupAdapter::Btrfs => btrfs::BtrfsBackup::clean(server, uuid).await,
            BackupAdapter::Zfs => zfs::ZfsBackup::clean(server, uuid).await,
            BackupAdapter::Restic => restic::ResticBackup::clean(server, uuid).await,
        }
    }
}
