use crate::server::backup::InternalBackup;
use std::{
    collections::HashMap,
    fs::Metadata,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering},
    },
};
use tokio::{
    io::AsyncReadExt,
    sync::{RwLock, RwLockReadGuard},
};

pub mod archive;
pub mod backup;
pub mod limiter;
pub mod pull;
mod usage;
pub mod writer;

pub struct Filesystem {
    uuid: uuid::Uuid,
    checker_abort: Arc<AtomicBool>,
    config: Arc<crate::config::Config>,

    pub base_path: PathBuf,

    disk_limit: AtomicI64,
    disk_usage_cached: Arc<AtomicU64>,
    disk_usage: Arc<RwLock<usage::DiskUsage>>,
    disk_ignored: Arc<RwLock<ignore::overrides::Override>>,

    pub pulls: RwLock<HashMap<uuid::Uuid, Arc<RwLock<pull::Download>>>>,
}

impl Filesystem {
    pub fn new(
        uuid: uuid::Uuid,
        disk_limit: u64,
        check_interval: u64,
        config: Arc<crate::config::Config>,
        deny_list: &[String],
    ) -> Self {
        let base_path = Path::new(&config.system.data_directory).join(uuid.to_string());
        let disk_usage = Arc::new(RwLock::new(usage::DiskUsage::default()));
        let disk_usage_cached = Arc::new(AtomicU64::new(0));
        let mut disk_ignored = ignore::overrides::OverrideBuilder::new(&base_path);

        for entry in deny_list {
            disk_ignored.add(entry).ok();
        }

        let checker_abort = Arc::new(AtomicBool::new(false));

        std::thread::spawn({
            let disk_usage = Arc::clone(&disk_usage);
            let disk_usage_cached = Arc::clone(&disk_usage_cached);
            let checker_abort = Arc::clone(&checker_abort);
            let base_path = base_path.clone();

            move || {
                loop {
                    if checker_abort.load(Ordering::Relaxed) {
                        break;
                    }

                    tracing::debug!(
                        path = %base_path.display(),
                        "checking disk usage"
                    );

                    let mut tmp_disk_usage = usage::DiskUsage::default();

                    fn recursive_size(
                        path: &Path,
                        relative_path: &[String],
                        disk_usage: &mut usage::DiskUsage,
                    ) -> u64 {
                        let mut total_size = 0;
                        let metadata = match path.symlink_metadata() {
                            Ok(metadata) => metadata,
                            Err(_) => return 0,
                        };

                        total_size += metadata.len();

                        if metadata.is_dir() {
                            if let Ok(entries) = path.read_dir() {
                                for entry in entries.flatten() {
                                    let path = entry.path();
                                    let metadata = match path.symlink_metadata() {
                                        Ok(metadata) => metadata,
                                        Err(_) => continue,
                                    };

                                    let file_name = entry.file_name().to_string_lossy().to_string();
                                    let mut new_path = relative_path.to_vec();
                                    new_path.push(file_name);

                                    total_size += metadata.len();

                                    if metadata.is_dir() {
                                        let size = recursive_size(&path, &new_path, disk_usage);
                                        disk_usage.update_size(&new_path, size as i64);
                                    }
                                }
                            }
                        }

                        total_size
                    }

                    let total_size = recursive_size(&base_path, &[], &mut tmp_disk_usage);
                    let total_entry_size =
                        tmp_disk_usage.entries.values().map(|e| e.size).sum::<u64>();

                    *disk_usage.blocking_write() = tmp_disk_usage;
                    disk_usage_cached.store(total_size + total_entry_size, Ordering::Relaxed);

                    tracing::debug!(
                        path = %base_path.display(),
                        "{} bytes disk usage",
                        disk_usage_cached.load(Ordering::Relaxed)
                    );

                    std::thread::sleep(std::time::Duration::from_secs(check_interval));
                }
            }
        });

        Self {
            uuid,
            checker_abort,
            config: Arc::clone(&config),

            base_path,

            disk_limit: AtomicI64::new(disk_limit as i64),
            disk_usage_cached,
            disk_usage,
            disk_ignored: Arc::new(RwLock::new(disk_ignored.build().unwrap())),

            pulls: RwLock::new(HashMap::new()),
        }
    }

    pub async fn update_ignored(&self, deny_list: &[String]) {
        let mut disk_ignored = ignore::overrides::OverrideBuilder::new(&self.base_path);
        for entry in deny_list {
            disk_ignored.add(entry).ok();
        }

        *self.disk_ignored.write().await = disk_ignored.build().unwrap();
    }

    pub async fn is_ignored(&self, path: &Path, is_dir: bool) -> bool {
        self.disk_ignored
            .read()
            .await
            .matched(path, is_dir)
            .invert()
            .is_ignore()
    }

    pub fn is_ignored_sync(&self, path: &Path, is_dir: bool) -> bool {
        self.disk_ignored
            .blocking_read()
            .matched(path, is_dir)
            .invert()
            .is_ignore()
    }

    pub async fn pulls(
        &self,
    ) -> RwLockReadGuard<'_, HashMap<uuid::Uuid, Arc<RwLock<pull::Download>>>> {
        if let Ok(mut pulls) = self.pulls.try_write() {
            for key in pulls.keys().cloned().collect::<Vec<_>>() {
                if let Some(download) = pulls.get(&key) {
                    if download
                        .read()
                        .await
                        .task
                        .as_ref()
                        .map(|t| t.is_finished())
                        .unwrap_or(true)
                    {
                        pulls.remove(&key);
                    }
                }
            }
        }

        self.pulls.read().await
    }

    #[inline]
    pub async fn limiter_usage(&self) -> u64 {
        limiter::disk_usage(self)
            .await
            .unwrap_or_else(|_| self.disk_usage_cached.load(Ordering::Relaxed))
    }

    #[inline]
    pub async fn update_disk_limit(&self, limit: u64) {
        self.disk_limit.store(limit as i64, Ordering::Relaxed);
        limiter::update_disk_limit(self, limit)
            .await
            .unwrap_or_else(|_| tracing::warn!("failed to update disk limit"));
    }

    #[inline]
    pub fn disk_limit(&self) -> i64 {
        self.disk_limit.load(Ordering::Relaxed)
    }

    #[inline]
    pub async fn is_full(&self) -> bool {
        self.disk_limit() != 0 && self.limiter_usage().await >= self.disk_limit() as u64
    }

    #[inline]
    pub fn base(&self) -> String {
        self.base_path.to_string_lossy().to_string()
    }

    #[inline]
    #[tracing::instrument]
    pub fn resolve_path(path: &Path) -> PathBuf {
        let mut result = PathBuf::new();

        for component in path.components() {
            match component {
                std::path::Component::ParentDir => {
                    if !result.as_os_str().is_empty()
                        && result.components().next_back() != Some(std::path::Component::RootDir)
                    {
                        result.pop();
                    }
                }
                _ => {
                    result.push(component);
                }
            }
        }

        tracing::debug!(
            safe_path = %result.display(),
            "resolved filesystem path"
        );

        result
    }

    #[inline]
    pub fn relative_path(&self, path: &Path) -> Option<PathBuf> {
        let parent = Self::resolve_path(path.parent()?);
        if !parent.starts_with(&self.base_path) {
            return None;
        }

        let file_name = path.file_name()?;
        parent
            .strip_prefix(&self.base_path)
            .ok()
            .map(|p| p.join(file_name))
    }

    #[inline]
    pub fn path_to_components(&self, path: &Path) -> Vec<String> {
        if let Some(rel_path) = self.relative_path(path) {
            rel_path
                .components()
                .map(|c| c.as_os_str().to_string_lossy().to_string())
                .collect()
        } else {
            Vec::new()
        }
    }

    #[inline]
    #[tracing::instrument(skip(self))]
    pub async fn safe_path(&self, path: &str) -> Option<PathBuf> {
        let path = self.base_path.join(path.trim_start_matches('/'));

        if let Ok(safe_path) = tokio::fs::canonicalize(&path).await {
            if safe_path.starts_with(&self.base_path) {
                tracing::debug!(
                    safe_path = %safe_path.display(),
                    "resolved filesystem path"
                );

                Some(safe_path)
            } else {
                tracing::debug!(
                    safe_path = %safe_path.display(),
                    "resolved filesystem path, but it is out of bounds"
                );

                None
            }
        } else {
            let safe_path = Self::resolve_path(&path);
            if safe_path.starts_with(&self.base_path) {
                Some(safe_path)
            } else {
                tracing::debug!(
                    safe_path = %safe_path.display(),
                    "resolved filesystem path, but it is out of bounds"
                );

                None
            }
        }
    }

    #[inline]
    pub async fn safe_symlink_path(&self, path: &str) -> Option<PathBuf> {
        let safe_path = Self::resolve_path(&self.base_path.join(path.trim_start_matches('/')));
        if safe_path.starts_with(&self.base_path) {
            Some(safe_path)
        } else {
            None
        }
    }

    pub async fn backup_fs(
        &self,
        server: &crate::server::Server,
        path: &Path,
    ) -> Option<(InternalBackup, PathBuf)> {
        if !self.config.system.backups.mounting.enabled {
            return None;
        }

        if !path.starts_with(
            self.base_path
                .join(&self.config.system.backups.mounting.path),
        ) {
            return None;
        }

        let backup_path = path
            .strip_prefix(
                self.base_path
                    .join(&self.config.system.backups.mounting.path),
            )
            .ok()?;
        let uuid: uuid::Uuid = backup_path
            .components()
            .next()?
            .as_os_str()
            .to_string_lossy()
            .parse()
            .ok()?;

        if !server.configuration.read().await.backups.contains(&uuid) {
            return None;
        }

        match crate::server::backup::InternalBackup::find(server, uuid).await {
            Some(backup) => Some((
                backup,
                backup_path
                    .strip_prefix(uuid.to_string())
                    .ok()?
                    .to_path_buf(),
            )),
            None => None,
        }
    }

    pub async fn truncate_path(&self, path: &PathBuf) -> tokio::io::Result<()> {
        let metadata = tokio::fs::symlink_metadata(path).await?;

        let components = self.path_to_components(path);
        let size = if metadata.is_dir() {
            let disk_usage = self.disk_usage.read().await;
            disk_usage.get_size(&components).unwrap_or(0)
        } else {
            metadata.len()
        };

        self.allocate_in_path(path, -(size as i64)).await;

        if metadata.is_dir() && size > 0 {
            let mut disk_usage = self.disk_usage.write().await;
            disk_usage.remove_path(&components);
        }

        if metadata.is_dir() {
            tokio::fs::remove_dir_all(path).await
        } else {
            tokio::fs::remove_file(path).await
        }
    }

    pub async fn rename_path(
        &self,
        old_path: &PathBuf,
        new_path: &PathBuf,
    ) -> tokio::io::Result<()> {
        if let Some(parent) = new_path.parent() {
            if !parent.exists() {
                tokio::fs::create_dir_all(parent).await?;
            }
        }

        let metadata = tokio::fs::symlink_metadata(old_path).await?;
        let is_dir = metadata.is_dir();

        let old_parent = tokio::fs::canonicalize(old_path.parent().unwrap()).await?;
        let new_parent = tokio::fs::canonicalize(new_path.parent().unwrap()).await?;

        if !self.is_safe_path(&old_parent).await || !self.is_safe_path(&new_parent).await {
            return Err(tokio::io::Error::new(
                tokio::io::ErrorKind::PermissionDenied,
                "Unsafe path",
            ));
        }

        let abs_new_path = new_parent.join(new_path.file_name().unwrap());

        if !self.is_safe_path(&abs_new_path).await {
            return Err(tokio::io::Error::new(
                tokio::io::ErrorKind::PermissionDenied,
                "Unsafe path",
            ));
        }

        if is_dir {
            let mut disk_usage = self.disk_usage.write().await;

            let path = disk_usage.remove_path(&self.path_to_components(old_path));
            if let Some(path) = path {
                disk_usage.add_directory(
                    &abs_new_path
                        .strip_prefix(&self.base_path)
                        .unwrap()
                        .components()
                        .map(|c| c.as_os_str().to_string_lossy().to_string())
                        .collect::<Vec<_>>(),
                    path,
                );
            }
        } else {
            let size = metadata.len() as i64;

            self.allocate_in_path(&old_parent, -size).await;
            self.allocate_in_path(&new_parent, size).await;
        }

        tokio::fs::rename(old_path, new_path).await?;

        Ok(())
    }

    /// Allocates (or deallocates) space for a path in the filesystem.
    /// Updates both the disk_usage map for directories and the cached total.
    ///
    /// - `path`: The path to allocate space for
    /// - `size`: The amount of space to allocate (positive) or deallocate (negative)
    ///
    /// Returns `true` if allocation was successful, `false` if it would exceed disk limit
    pub async fn allocate_in_path_raw(&self, path: &[String], delta: i64) -> bool {
        if delta == 0 {
            return true;
        }

        if delta > 0 {
            let current_usage = self.disk_usage_cached.load(Ordering::Relaxed) as i64;

            if self.disk_limit() != 0 && current_usage + delta > self.disk_limit() {
                return false;
            }
        }

        if delta > 0 {
            self.disk_usage_cached
                .fetch_add(delta as u64, Ordering::Relaxed);
        } else {
            let abs_size = delta.unsigned_abs();
            let current = self.disk_usage_cached.load(Ordering::Relaxed);

            if current >= abs_size {
                self.disk_usage_cached
                    .fetch_sub(abs_size, Ordering::Relaxed);
            } else {
                self.disk_usage_cached.store(0, Ordering::Relaxed);
            }
        }

        self.disk_usage.write().await.update_size(path, delta);

        true
    }

    #[inline]
    pub async fn allocate_in_path(&self, path: &Path, delta: i64) -> bool {
        let components = self.path_to_components(path);

        self.allocate_in_path_raw(&components, delta).await
    }

    #[inline]
    pub async fn is_safe_path(&self, path: &Path) -> bool {
        if let Ok(path) = tokio::fs::canonicalize(path).await {
            path.starts_with(&self.base_path)
        } else {
            Self::resolve_path(path).starts_with(&self.base_path)
        }
    }

    #[inline]
    pub fn is_safe_path_sync(&self, path: &Path) -> bool {
        if let Ok(path) = path.canonicalize() {
            path.starts_with(&self.base_path)
        } else {
            Self::resolve_path(path).starts_with(&self.base_path)
        }
    }

    pub async fn truncate_root(&self) {
        self.disk_usage.write().await.clear();
        self.disk_usage_cached.store(0, Ordering::Relaxed);

        let mut directory = tokio::fs::read_dir(&self.base_path).await.unwrap();
        while let Ok(Some(entry)) = directory.next_entry().await {
            let path = entry.path();

            if let Ok(metadata) = tokio::fs::symlink_metadata(&path).await {
                if metadata.is_dir() {
                    tokio::fs::remove_dir_all(&path).await.ok();
                } else {
                    tokio::fs::remove_file(&path).await.ok();
                }
            }
        }
    }

    pub async fn chown_path(&self, path: &Path) {
        fn recursive_chown(path: &Path, owner_uid: u32, owner_gid: u32) {
            let metadata = path.symlink_metadata().unwrap();
            if metadata.is_dir() {
                if let Ok(entries) = path.read_dir() {
                    for entry in entries.flatten() {
                        let path = entry.path();
                        recursive_chown(&path, owner_uid, owner_gid);
                    }
                }

                std::os::unix::fs::chown(path, Some(owner_uid), Some(owner_gid)).ok();
            } else {
                std::os::unix::fs::chown(path, Some(owner_uid), Some(owner_gid)).ok();
            }
        }

        tokio::task::spawn_blocking({
            let path = path.to_path_buf();
            let owner_uid = self.config.system.user.uid;
            let owner_gid = self.config.system.user.gid;

            move || {
                recursive_chown(&path, owner_uid, owner_gid);
            }
        })
        .await
        .unwrap()
    }

    pub async fn setup(&self) {
        if let Err(err) = limiter::setup(self).await {
            tracing::error!(
                path = %self.base_path.display(),
                "failed to create server base directory: {}",
                err
            );

            return;
        }

        if let Err(err) =
            limiter::update_disk_limit(self, self.disk_limit.load(Ordering::Relaxed) as u64).await
        {
            tracing::error!(
                path = %self.base_path.display(),
                "failed to update disk limit for server: {}",
                err
            );
        }

        let base_path = self.base_path.clone();
        let owner_uid = self.config.system.user.uid;
        let owner_gid = self.config.system.user.gid;

        tokio::task::spawn_blocking(move || {
            std::os::unix::fs::chown(&base_path, Some(owner_uid), Some(owner_gid)).unwrap();
        })
        .await
        .unwrap();
    }

    pub async fn attach(&self) {
        if let Err(err) = limiter::attach(self).await {
            tracing::error!(
                path = %self.base_path.display(),
                "failed to attach server base directory: {}",
                err
            );
        }
    }

    pub async fn destroy(&self) {
        self.checker_abort.store(true, Ordering::Relaxed);

        if let Err(err) = limiter::destroy(self).await {
            tracing::error!(
                path = %self.base_path.display(),
                "failed to delete server base directory for: {}",
                err
            );
        }
    }

    #[inline]
    pub async fn to_api_entry_buffer(
        &self,
        path: PathBuf,
        metadata: &Metadata,
        buffer: Option<&[u8]>,
        symlink_destination: Option<PathBuf>,
        symlink_destination_metadata: Option<Metadata>,
    ) -> crate::models::DirectoryEntry {
        let real_metadata = symlink_destination_metadata.as_ref().unwrap_or(metadata);
        let real_path = symlink_destination.as_ref().unwrap_or(&path);

        let size = if real_metadata.is_dir() {
            let disk_usage = self.disk_usage.read().await;
            let components = self.path_to_components(real_path);

            disk_usage.get_size(&components).unwrap_or(0)
        } else {
            real_metadata.len()
        };

        let mime = if real_metadata.is_dir() {
            "inode/directory"
        } else if real_metadata.is_symlink() {
            "inode/symlink"
        } else if let Some(buffer) = buffer {
            if let Some(mime) = infer::get(buffer) {
                mime.mime_type()
            } else if std::str::from_utf8(buffer).is_ok() {
                "text/plain"
            } else {
                "application/octet-stream"
            }
        } else {
            "application/octet-stream"
        };

        let mut mode_str = String::new();
        let mode = metadata.permissions().mode();
        const TYPE_CHARS: &str = "dalTLDpSugct?";

        let file_type = (mode >> 28) & 0xF;
        if file_type < TYPE_CHARS.len() as u32 {
            mode_str.push(TYPE_CHARS.chars().nth(file_type as usize).unwrap());
        } else {
            mode_str.push('?');
        }

        const RWX: &str = "rwxrwxrwx";
        for i in 0..9 {
            if mode & (1 << (8 - i)) != 0 {
                mode_str.push(RWX.chars().nth(i).unwrap());
            } else {
                mode_str.push('-');
            }
        }

        crate::models::DirectoryEntry {
            name: path.file_name().unwrap().to_string_lossy().to_string(),
            created: chrono::DateTime::from_timestamp(
                metadata
                    .created()
                    .map(|t| t.duration_since(std::time::UNIX_EPOCH).unwrap_or_default())
                    .unwrap_or_default()
                    .as_secs() as i64,
                0,
            )
            .unwrap(),
            modified: chrono::DateTime::from_timestamp(
                metadata
                    .modified()
                    .map(|t| t.duration_since(std::time::UNIX_EPOCH).unwrap_or_default())
                    .unwrap_or_default()
                    .as_secs() as i64,
                0,
            )
            .unwrap(),
            mode: mode_str,
            mode_bits: format!("{:o}", metadata.permissions().mode() & 0o777),
            size,
            directory: real_metadata.is_dir(),
            file: real_metadata.is_file(),
            symlink: metadata.is_symlink(),
            mime,
        }
    }

    pub async fn to_api_entry(
        &self,
        path: PathBuf,
        metadata: Metadata,
    ) -> crate::models::DirectoryEntry {
        let symlink_destination = if metadata.is_symlink() {
            match tokio::fs::read_link(&path).await {
                Ok(link) => {
                    let joined = self.base_path.join(link);

                    if let Ok(joined) = tokio::fs::canonicalize(&joined).await {
                        if joined.starts_with(&self.base_path) {
                            Some(joined)
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                }
                Err(_) => None,
            }
        } else {
            None
        };

        let symlink_destination_metadata = if let Some(symlink_destination) = &symlink_destination {
            tokio::fs::symlink_metadata(symlink_destination).await.ok()
        } else {
            None
        };

        let mut buffer = [0; 128];
        let buffer = if metadata.is_file()
            || (symlink_destination.is_some()
                && symlink_destination_metadata
                    .as_ref()
                    .is_some_and(|m| m.is_file()))
        {
            let mut file = tokio::fs::File::open(symlink_destination.as_ref().unwrap_or(&path))
                .await
                .unwrap();
            let bytes_read = file.read(&mut buffer).await.unwrap_or(0);

            Some(&buffer[..bytes_read])
        } else {
            None
        };

        self.to_api_entry_buffer(
            path,
            &metadata,
            buffer,
            symlink_destination,
            symlink_destination_metadata,
        )
        .await
    }
}

impl Drop for Filesystem {
    fn drop(&mut self) {
        self.checker_abort.store(true, Ordering::Relaxed);
    }
}
