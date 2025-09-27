use crate::server::backup::BrowseBackup;
use cap_std::fs::{Metadata, PermissionsExt};
use std::{
    collections::HashMap,
    ops::Deref,
    os::fd::AsFd,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicI64, AtomicU64, Ordering},
    },
};
use tokio::{
    io::AsyncReadExt,
    sync::{Mutex, RwLock, RwLockReadGuard},
};

pub mod archive;
pub mod cap;
pub mod limiter;
pub mod operations;
pub mod pull;
pub mod usage;
pub mod writer;

pub struct Filesystem {
    uuid: uuid::Uuid,
    disk_checker: tokio::task::JoinHandle<()>,
    config: Arc<crate::config::Config>,

    pub base_path: PathBuf,
    cap_filesystem: cap::CapFilesystem,

    disk_limit: AtomicI64,
    disk_usage_cached: Arc<AtomicU64>,
    pub disk_usage: Arc<RwLock<usage::DiskUsage>>,
    disk_ignored: Arc<RwLock<ignore::gitignore::Gitignore>>,

    pub pulls: RwLock<HashMap<uuid::Uuid, Arc<RwLock<pull::Download>>>>,
    pub operations: operations::OperationManager,
}

impl Filesystem {
    pub fn new(
        uuid: uuid::Uuid,
        disk_limit: u64,
        sender: tokio::sync::broadcast::Sender<crate::server::websocket::WebsocketMessage>,
        config: Arc<crate::config::Config>,
        deny_list: &[String],
    ) -> Self {
        let base_path = Path::new(&config.system.data_directory).join(uuid.to_string());
        let disk_usage = Arc::new(RwLock::new(usage::DiskUsage::default()));
        let disk_usage_cached = Arc::new(AtomicU64::new(0));
        let mut disk_ignored = ignore::gitignore::GitignoreBuilder::new("/");

        for entry in deny_list {
            disk_ignored.add_line(None, entry).ok();
        }

        let cap_filesystem = cap::CapFilesystem::new_uninitialized(base_path.clone());

        Self {
            uuid,
            disk_checker: tokio::task::spawn({
                let config = Arc::clone(&config);
                let disk_usage = Arc::clone(&disk_usage);
                let disk_usage_cached = Arc::clone(&disk_usage_cached);
                let cap_filesystem = cap_filesystem.clone();

                async move {
                    loop {
                        let run_inner = async || -> Result<(), anyhow::Error> {
                            tracing::debug!(
                                path = %cap_filesystem.base_path.display(),
                                "checking disk usage"
                            );

                            let tmp_disk_usage =
                                Arc::new(Mutex::new(Some(usage::DiskUsage::default())));
                            let total_size = Arc::new(AtomicU64::new(0));

                            cap_filesystem
                                .async_walk_dir(Path::new(""))
                                .await?
                                .run_multithreaded(
                                    config.system.disk_check_threads,
                                    Arc::new({
                                        let total_size = Arc::clone(&total_size);
                                        let disk_usage = Arc::clone(&tmp_disk_usage);
                                        let cap_filesystem = cap_filesystem.clone();

                                        move |_, path: PathBuf| {
                                            let total_size = Arc::clone(&total_size);
                                            let disk_usage = Arc::clone(&disk_usage);
                                            let cap_filesystem = cap_filesystem.clone();

                                            async move {
                                                let metadata = match cap_filesystem
                                                    .async_symlink_metadata(&path)
                                                    .await
                                                {
                                                    Ok(metadata) => metadata,
                                                    Err(_) => return Ok(()),
                                                };
                                                let size = metadata.len();

                                                if metadata.is_dir()
                                                    && let Some(disk_usage) =
                                                        &mut *disk_usage.lock().await
                                                {
                                                    disk_usage.update_size(&path, size as i64);
                                                } else if let Some(parent) = path.parent()
                                                    && let Some(disk_usage) =
                                                        &mut *disk_usage.lock().await
                                                {
                                                    disk_usage.update_size(parent, size as i64);
                                                }

                                                total_size.fetch_add(size, Ordering::Relaxed);
                                                Ok(())
                                            }
                                        }
                                    }),
                                )
                                .await?;

                            let tmp_disk_usage = match tmp_disk_usage.lock().await.take() {
                                Some(usage) => usage,
                                None => {
                                    return Err(anyhow::anyhow!(
                                        "disk usage is already taken (???????)"
                                    ));
                                }
                            };

                            *disk_usage.write().await = tmp_disk_usage;
                            disk_usage_cached
                                .store(total_size.load(Ordering::Relaxed), Ordering::Relaxed);

                            tracing::debug!(
                                path = %cap_filesystem.base_path.display(),
                                "{} bytes disk usage",
                                disk_usage_cached.load(Ordering::Relaxed)
                            );

                            Ok(())
                        };

                        match run_inner().await {
                            Ok(_) => {
                                tracing::debug!(
                                    path = %cap_filesystem.base_path.display(),
                                    "disk usage check completed successfully"
                                );
                            }
                            Err(err) => {
                                tracing::error!(
                                    path = %cap_filesystem.base_path.display(),
                                    "disk usage check failed: {}",
                                    err
                                );
                            }
                        }

                        tokio::time::sleep(std::time::Duration::from_secs(
                            config.system.disk_check_interval,
                        ))
                        .await;
                    }
                }
            }),
            config: Arc::clone(&config),

            base_path,
            cap_filesystem,

            disk_limit: AtomicI64::new(disk_limit as i64),
            disk_usage_cached,
            disk_usage,
            disk_ignored: Arc::new(RwLock::new(disk_ignored.build().unwrap())),

            pulls: RwLock::new(HashMap::new()),
            operations: operations::OperationManager::new(sender),
        }
    }

    pub async fn update_ignored(&self, deny_list: &[String]) {
        let mut disk_ignored = ignore::gitignore::GitignoreBuilder::new("");
        for entry in deny_list {
            disk_ignored.add_line(None, entry).ok();
        }

        *self.disk_ignored.write().await = disk_ignored.build().unwrap();
    }

    pub async fn is_ignored(&self, path: &Path, is_dir: bool) -> bool {
        self.disk_ignored
            .read()
            .await
            .matched(path, is_dir)
            .is_ignore()
    }

    pub async fn get_ignored(&self) -> ignore::gitignore::Gitignore {
        self.disk_ignored.read().await.clone()
    }

    pub fn is_ignored_sync(&self, path: &Path, is_dir: bool) -> bool {
        self.disk_ignored
            .blocking_read()
            .matched(path, is_dir)
            .is_ignore()
    }

    pub async fn pulls(
        &self,
    ) -> RwLockReadGuard<'_, HashMap<uuid::Uuid, Arc<RwLock<pull::Download>>>> {
        if let Ok(mut pulls) = self.pulls.try_write() {
            let operations = self.operations.operations().await;

            for key in pulls.keys().copied().collect::<Vec<_>>() {
                if !operations.contains_key(&key) {
                    pulls.remove(&key);
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
    pub fn path_to_components(&self, path: &Path) -> Vec<String> {
        self.relative_path(path)
            .components()
            .map(|c| c.as_os_str().to_string_lossy().to_string())
            .collect()
    }

    pub async fn backup_fs(
        &self,
        server: &crate::server::Server,
        backup_manager: &crate::server::backup::manager::BackupManager,
        path: &Path,
    ) -> Option<(Arc<BrowseBackup>, PathBuf)> {
        if !self.config.system.backups.mounting.enabled {
            return None;
        }

        let path = self.relative_path(path);
        if !path.starts_with(&self.config.system.backups.mounting.path) {
            return None;
        }

        let backup_path = path
            .strip_prefix(&self.config.system.backups.mounting.path)
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

        match backup_manager.browse(server, uuid).await {
            Ok(Some(backup)) => Some((
                backup,
                backup_path
                    .strip_prefix(uuid.to_string())
                    .ok()?
                    .to_path_buf(),
            )),
            Ok(None) => None,
            Err(err) => {
                tracing::error!(server = %server.uuid, backup = %uuid, "failed to find backup: {}", err);
                None
            }
        }
    }

    pub async fn truncate_path(&self, path: impl AsRef<Path>) -> Result<(), anyhow::Error> {
        let path = self.relative_path(path.as_ref());

        let metadata = self.async_symlink_metadata(&path).await?;

        let size = if metadata.is_dir() {
            let disk_usage = self.disk_usage.read().await;
            disk_usage.get_size(&path).unwrap_or(0)
        } else {
            metadata.len()
        };

        self.async_allocate_in_path(&path, -(size as i64), false)
            .await;

        if metadata.is_dir() {
            let mut disk_usage = self.disk_usage.write().await;
            disk_usage.remove_path(&path);
        }

        if metadata.is_dir() {
            self.async_remove_dir_all(path).await?;
        } else {
            self.async_remove_file(path).await?;
        }

        Ok(())
    }

    pub async fn rename_path(
        &self,
        old_path: impl AsRef<Path>,
        new_path: impl AsRef<Path>,
    ) -> Result<(), anyhow::Error> {
        let old_path = self.relative_path(old_path.as_ref());
        let new_path = self.relative_path(new_path.as_ref());

        if let Some(parent) = new_path.parent() {
            self.async_create_dir_all(parent).await?;
        }

        let metadata = self.async_metadata(&old_path).await?;
        let is_dir = metadata.is_dir();

        let old_parent = self
            .async_canonicalize(match old_path.parent() {
                Some(parent) => parent,
                None => return Err(anyhow::anyhow!("failed to get old path parent")),
            })
            .await
            .unwrap_or_default();
        let new_parent = self
            .async_canonicalize(match new_path.parent() {
                Some(parent) => parent,
                None => return Err(anyhow::anyhow!("failed to get new path parent")),
            })
            .await
            .unwrap_or_default();

        let abs_new_path = new_parent.join(match new_path.file_name() {
            Some(name) => name,
            None => return Err(anyhow::anyhow!("failed to get new path file name")),
        });

        if is_dir {
            let mut disk_usage = self.disk_usage.write().await;

            let path = disk_usage.remove_path(&old_path);
            if let Some(path) = path {
                disk_usage.add_directory(
                    &abs_new_path
                        .components()
                        .map(|c| c.as_os_str().to_string_lossy().to_string())
                        .collect::<Vec<_>>(),
                    path,
                );
            }
        } else {
            let size = metadata.len() as i64;

            self.async_allocate_in_path(&old_parent, -size, true).await;
            self.async_allocate_in_path(&new_parent, size, true).await;
        }

        self.async_rename(old_path, &self.cap_filesystem, new_path)
            .await?;

        Ok(())
    }

    /// Allocates (or deallocates) space for a path in the filesystem.
    /// Updates both the disk_usage map for directories and the cached total.
    ///
    /// - `path`: The path to allocate space for
    /// - `size`: The amount of space to allocate (positive) or deallocate (negative)
    /// - `ignorant`: If `true`, ignores disk limit checks
    ///
    /// Returns `true` if allocation was successful, `false` if it would exceed disk limit
    pub async fn async_allocate_in_path(&self, path: &Path, delta: i64, ignorant: bool) -> bool {
        if crate::unlikely(delta == 0) {
            return true;
        }

        if delta > 0 && !ignorant {
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

    /// Allocates (or deallocates) space for a path in the filesystem.
    /// Updates both the disk_usage map for directories and the cached total.
    ///
    /// - `path`: The path to allocate space for
    /// - `size`: The amount of space to allocate (positive) or deallocate (negative)
    /// - `ignorant`: If `true`, ignores disk limit checks
    ///
    /// Returns `true` if allocation was successful, `false` if it would exceed disk limit
    pub async fn async_allocate_in_path_slice(
        &self,
        path: &[String],
        delta: i64,
        ignorant: bool,
    ) -> bool {
        if crate::unlikely(delta == 0) {
            return true;
        }

        if delta > 0 && !ignorant {
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

        self.disk_usage.write().await.update_size_slice(path, delta);

        true
    }

    /// Allocates (or deallocates) space for a path in the filesystem.
    /// Updates both the disk_usage map for directories and the cached total.
    ///
    /// - `path`: The path to allocate space for
    /// - `size`: The amount of space to allocate (positive) or deallocate (negative)
    /// - `ignorant`: If `true`, ignores disk limit checks
    ///
    /// Returns `true` if allocation was successful, `false` if it would exceed disk limit
    pub fn allocate_in_path(&self, path: &Path, delta: i64, ignorant: bool) -> bool {
        if crate::unlikely(delta == 0) {
            return true;
        }

        if delta > 0 && !ignorant {
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

        self.disk_usage.blocking_write().update_size(path, delta);

        true
    }

    /// Allocates (or deallocates) space for a path in the filesystem.
    /// Updates both the disk_usage map for directories and the cached total.
    ///
    /// - `path`: The path to allocate space for
    /// - `size`: The amount of space to allocate (positive) or deallocate (negative)
    /// - `ignorant`: If `true`, ignores disk limit checks
    ///
    /// Returns `true` if allocation was successful, `false` if it would exceed disk limit
    pub fn allocate_in_path_slice(&self, path: &[String], delta: i64, ignorant: bool) -> bool {
        if crate::unlikely(delta == 0) {
            return true;
        }

        if delta > 0 && !ignorant {
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

        self.disk_usage
            .blocking_write()
            .update_size_slice(path, delta);

        true
    }

    pub async fn truncate_root(&self) -> Result<(), anyhow::Error> {
        self.disk_usage.write().await.clear();
        self.disk_usage_cached.store(0, Ordering::Relaxed);

        let mut directory = self.async_walk_dir(Path::new("")).await?;
        while let Some(Ok((is_dir, path))) = directory.next_entry().await {
            if is_dir {
                self.async_remove_dir_all(&path).await?;
            } else {
                self.async_remove_file(&path).await?;
            }
        }

        Ok(())
    }

    pub async fn chown_path(&self, path: impl AsRef<Path>) -> Result<(), anyhow::Error> {
        let metadata = self.async_metadata(path.as_ref()).await?;

        let owner_uid = rustix::fs::Uid::from_raw_unchecked(self.config.system.user.uid);
        let owner_gid = rustix::fs::Gid::from_raw_unchecked(self.config.system.user.gid);

        tokio::task::spawn_blocking({
            let cap_filesystem = self.cap_filesystem.clone();
            let path = self.relative_path(path.as_ref());
            let base_path = self.base_path.clone();

            move || {
                if crate::unlikely(path == PathBuf::from("") || path == PathBuf::from("/")) {
                    Ok::<_, anyhow::Error>(std::os::unix::fs::chown(
                        base_path,
                        Some(owner_uid.as_raw()),
                        Some(owner_gid.as_raw()),
                    )?)
                } else {
                    Ok(rustix::fs::chownat(
                        cap_filesystem.get_inner()?.as_fd(),
                        path,
                        Some(owner_uid),
                        Some(owner_gid),
                        rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
                    )?)
                }
            }
        })
        .await??;

        if metadata.is_dir() {
            let cap_filesystem = self.cap_filesystem.clone();

            self.async_walk_dir(path)
                .await?
                .run_multithreaded(
                    self.config.system.check_permissions_on_boot_threads,
                    Arc::new(move |_, path: PathBuf| {
                        let cap_filesystem = cap_filesystem.clone();

                        async move {
                            tokio::task::spawn_blocking(move || {
                                rustix::fs::chownat(
                                    cap_filesystem.get_inner()?.as_fd(),
                                    path,
                                    Some(owner_uid),
                                    Some(owner_gid),
                                    rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
                                )
                                .ok();

                                Ok(())
                            })
                            .await?
                        }
                    }),
                )
                .await
        } else {
            Ok(())
        }
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

        if self.cap_filesystem.is_uninitialized().await {
            let base_path = self.base_path.clone();
            match tokio::task::spawn_blocking(move || {
                cap_std::fs::Dir::open_ambient_dir(&base_path, cap_std::ambient_authority())
            })
            .await
            {
                Ok(Ok(dir)) => {
                    *self.cap_filesystem.inner.write().await = Some(Arc::new(dir));
                }
                Ok(Err(err)) => {
                    tracing::error!(
                        path = %self.base_path.display(),
                        "failed to open server base directory: {}",
                        err
                    );
                }
                Err(err) => {
                    tracing::error!(
                        path = %self.base_path.display(),
                        "failed to open server base directory: {}",
                        err
                    );
                }
            }
        }
    }

    pub async fn attach(&self) {
        if let Err(err) = limiter::attach(self).await {
            tracing::error!(
                path = %self.base_path.display(),
                "failed to attach server base directory: {}",
                err
            );
        }

        if self.cap_filesystem.is_uninitialized().await {
            let base_path = self.base_path.clone();
            match tokio::task::spawn_blocking(move || {
                cap_std::fs::Dir::open_ambient_dir(&base_path, cap_std::ambient_authority())
            })
            .await
            {
                Ok(Ok(dir)) => {
                    *self.cap_filesystem.inner.write().await = Some(Arc::new(dir));
                }
                Ok(Err(err)) => {
                    tracing::error!(
                        path = %self.base_path.display(),
                        "failed to open server base directory: {}",
                        err
                    );
                }
                Err(err) => {
                    tracing::error!(
                        path = %self.base_path.display(),
                        "failed to open server base directory: {}",
                        err
                    );
                }
            }
        }
    }

    pub async fn destroy(&self) {
        self.disk_checker.abort();

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
        no_directory_size: bool,
        buffer: Option<&[u8]>,
        symlink_destination: Option<PathBuf>,
        symlink_destination_metadata: Option<Metadata>,
    ) -> crate::models::DirectoryEntry {
        let real_metadata = symlink_destination_metadata.as_ref().unwrap_or(metadata);
        let real_path = symlink_destination.as_ref().unwrap_or(&path);

        let size = if real_metadata.is_dir() {
            if !no_directory_size && !self.config.api.disable_directory_size {
                self.disk_usage
                    .read()
                    .await
                    .get_size(real_path)
                    .unwrap_or(0)
            } else {
                0
            }
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
            } else if let Some(mime) = new_mime_guess::from_path(real_path).iter_raw().next() {
                mime
            } else if crate::is_valid_utf8_slice(buffer) || buffer.is_empty() {
                "text/plain"
            } else {
                "application/octet-stream"
            }
        } else {
            "application/octet-stream"
        };

        let mut mode_str = String::new();
        let mode = metadata.permissions().mode();

        mode_str.reserve_exact(10);
        mode_str.push(match rustix::fs::FileType::from_raw_mode(mode) {
            rustix::fs::FileType::RegularFile => '-',
            rustix::fs::FileType::Directory => 'd',
            rustix::fs::FileType::Symlink => 'l',
            rustix::fs::FileType::BlockDevice => 'b',
            rustix::fs::FileType::CharacterDevice => 'c',
            rustix::fs::FileType::Socket => 's',
            rustix::fs::FileType::Fifo => 'p',
            rustix::fs::FileType::Unknown => '?',
        });

        const RWX: &str = "rwxrwxrwx";
        for i in 0..9 {
            if mode & (1 << (8 - i)) != 0 {
                mode_str.push(RWX.chars().nth(i).unwrap());
            } else {
                mode_str.push('-');
            }
        }

        crate::models::DirectoryEntry {
            name: path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string(),
            created: chrono::DateTime::from_timestamp(
                metadata
                    .created()
                    .map(|t| {
                        t.into_std()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                    })
                    .unwrap_or_default()
                    .as_secs() as i64,
                0,
            )
            .unwrap_or_default(),
            modified: chrono::DateTime::from_timestamp(
                metadata
                    .modified()
                    .map(|t| {
                        t.into_std()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                    })
                    .unwrap_or_default()
                    .as_secs() as i64,
                0,
            )
            .unwrap_or_default(),
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
            match self.async_read_link(&path).await {
                Ok(link) => self.async_canonicalize(link).await.ok(),
                Err(_) => None,
            }
        } else {
            None
        };

        let symlink_destination_metadata =
            if let Some(symlink_destination) = symlink_destination.clone() {
                self.async_symlink_metadata(&symlink_destination).await.ok()
            } else {
                None
            };

        let mut buffer = [0; 64];
        let buffer = if metadata.is_file()
            || (symlink_destination.is_some()
                && symlink_destination_metadata
                    .as_ref()
                    .is_some_and(|m| m.is_file()))
        {
            match self
                .async_open(symlink_destination.as_ref().unwrap_or(&path))
                .await
            {
                Ok(mut file) => {
                    let bytes_read = file.read(&mut buffer).await.unwrap_or(0);

                    Some(&buffer[..bytes_read])
                }
                Err(_) => None,
            }
        } else {
            None
        };

        self.to_api_entry_buffer(
            path,
            &metadata,
            false,
            buffer,
            symlink_destination,
            symlink_destination_metadata,
        )
        .await
    }

    pub async fn to_api_entry_cap(
        &self,
        filesystem: &cap::CapFilesystem,
        path: PathBuf,
        metadata: Metadata,
    ) -> crate::models::DirectoryEntry {
        let symlink_destination = if metadata.is_symlink() {
            match filesystem.async_read_link(&path).await {
                Ok(link) => filesystem.async_canonicalize(link).await.ok(),
                Err(_) => None,
            }
        } else {
            None
        };

        let symlink_destination_metadata =
            if let Some(symlink_destination) = symlink_destination.clone() {
                filesystem
                    .async_symlink_metadata(&symlink_destination)
                    .await
                    .ok()
            } else {
                None
            };

        let mut buffer = [0; 64];
        let buffer = if metadata.is_file()
            || (symlink_destination.is_some()
                && symlink_destination_metadata
                    .as_ref()
                    .is_some_and(|m| m.is_file()))
        {
            match filesystem
                .async_open(symlink_destination.as_ref().unwrap_or(&path))
                .await
            {
                Ok(mut file) => {
                    let bytes_read = file.read(&mut buffer).await.unwrap_or(0);

                    Some(&buffer[..bytes_read])
                }
                Err(_) => None,
            }
        } else {
            None
        };

        self.to_api_entry_buffer(
            path,
            &metadata,
            true,
            buffer,
            symlink_destination,
            symlink_destination_metadata,
        )
        .await
    }
}

impl Deref for Filesystem {
    type Target = cap::CapFilesystem;

    fn deref(&self) -> &Self::Target {
        &self.cap_filesystem
    }
}
