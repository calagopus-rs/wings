use crate::server::backup::BrowseBackup;
use cap_std::fs::{Metadata, PermissionsExt};
use std::{
    collections::HashMap,
    ops::Deref,
    path::{Path, PathBuf},
    pin::Pin,
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
pub mod pull;
mod usage;
pub mod writer;

pub struct Filesystem {
    uuid: uuid::Uuid,
    disk_checker: Mutex<Option<tokio::task::JoinHandle<()>>>,
    config: Arc<crate::config::Config>,

    pub base_path: PathBuf,
    cap_filesystem: cap::CapFilesystem,

    disk_limit: AtomicI64,
    disk_usage_cached: Arc<AtomicU64>,
    disk_usage: Arc<RwLock<usage::DiskUsage>>,
    disk_ignored: Arc<RwLock<ignore::gitignore::Gitignore>>,

    pub pulls: RwLock<HashMap<uuid::Uuid, Arc<RwLock<pull::Download>>>>,
}

impl Filesystem {
    pub fn new(
        uuid: uuid::Uuid,
        disk_limit: u64,
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

        Self {
            uuid,
            disk_checker: Mutex::new(None),
            config: Arc::clone(&config),

            base_path: base_path.clone(),
            cap_filesystem: cap::CapFilesystem::new_uninitialized(base_path),

            disk_limit: AtomicI64::new(disk_limit as i64),
            disk_usage_cached,
            disk_usage,
            disk_ignored: Arc::new(RwLock::new(disk_ignored.build().unwrap())),

            pulls: RwLock::new(HashMap::new()),
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
            for key in pulls.keys().cloned().collect::<Vec<_>>() {
                if let Some(download) = pulls.get(&key)
                    && download
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

        let components = self.path_to_components(&path);
        let size = if metadata.is_dir() {
            let disk_usage = self.disk_usage.read().await;
            disk_usage.get_size(&components).unwrap_or(0)
        } else {
            metadata.len()
        };

        self.allocate_in_path(&path, -(size as i64)).await;

        if metadata.is_dir() {
            let mut disk_usage = self.disk_usage.write().await;
            disk_usage.remove_path(&components);
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

            let path = disk_usage.remove_path(&self.path_to_components(&old_path));
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

            self.allocate_in_path(&old_parent, -size).await;
            self.allocate_in_path(&new_parent, size).await;
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
    pub async fn allocate_in_path_raw(&self, path: &[String], delta: i64, ignorant: bool) -> bool {
        if delta == 0 {
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
    pub fn allocate_in_path_raw_sync(&self, path: &[String], delta: i64, ignorant: bool) -> bool {
        if delta == 0 {
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

    #[inline]
    pub async fn allocate_in_path(&self, path: &Path, delta: i64) -> bool {
        let components = self.path_to_components(path);

        self.allocate_in_path_raw(&components, delta, false).await
    }

    pub async fn truncate_root(&self) -> Result<(), anyhow::Error> {
        self.disk_usage.write().await.clear();
        self.disk_usage_cached.store(0, Ordering::Relaxed);

        let mut directory = tokio::fs::read_dir(&self.base_path).await?;
        while let Ok(Some(entry)) = directory.next_entry().await {
            let path = entry.path();

            if let Ok(metadata) = tokio::fs::symlink_metadata(&path).await {
                if metadata.is_dir() {
                    tokio::fs::remove_dir_all(&path).await?;
                } else {
                    tokio::fs::remove_file(&path).await?;
                }
            }
        }

        Ok(())
    }

    pub async fn chown_path(&self, path: impl Into<PathBuf>) -> Result<(), anyhow::Error> {
        fn recursive_chown(
            path: &Path,
            owner_uid: u32,
            owner_gid: u32,
        ) -> Result<(), std::io::Error> {
            let metadata = path.symlink_metadata()?;
            if metadata.is_dir() {
                if let Ok(entries) = path.read_dir() {
                    for entry in entries.flatten() {
                        let path = entry.path();
                        recursive_chown(&path, owner_uid, owner_gid)?;
                    }
                }

                std::os::unix::fs::lchown(path, Some(owner_uid), Some(owner_gid))
            } else {
                std::os::unix::fs::lchown(path, Some(owner_uid), Some(owner_gid))
            }
        }

        tokio::task::spawn_blocking({
            let path = self.base_path.join(self.relative_path(&path.into()));
            let owner_uid = self.config.system.user.uid;
            let owner_gid = self.config.system.user.gid;

            move || Ok(recursive_chown(&path, owner_uid, owner_gid)?)
        })
        .await?
    }

    pub async fn setup_disk_checker(&self, server: &crate::server::Server) {
        self.disk_checker.lock().await.replace(tokio::task::spawn({
            let check_interval = self.config.system.disk_check_interval;
            let disable_directory_size = self.config.api.disable_directory_size;
            let server = server.clone();

            async move {
                loop {
                    let run_inner = async || -> Result<(), anyhow::Error> {
                        tracing::debug!(
                            path = %server.filesystem.base_path.display(),
                            "checking disk usage"
                        );

                        let mut tmp_disk_usage = usage::DiskUsage::default();

                        fn recursive_size<'a>(
                            server: &'a crate::server::Server,
                            path: &'a Path,
                            relative_path: &'a [String],
                            disk_usage: &'a mut usage::DiskUsage,
                            disable_directory_size: bool,
                        ) -> Pin<Box<dyn Future<Output = u64> + Send + 'a>>
                        {
                            Box::pin(async move {
                                let mut total_size = 0;
                                let metadata =
                                    match server.filesystem.async_symlink_metadata(path).await {
                                        Ok(metadata) => metadata,
                                        Err(_) => return 0,
                                    };

                                total_size += metadata.len();

                                if metadata.is_dir()
                                    && let Ok(mut entries) =
                                        server.filesystem.async_read_dir(path).await
                                {
                                    while let Some(Ok((is_dir, file_name))) =
                                        entries.next_entry().await
                                    {
                                        let sub_path = path.join(&file_name);
                                        let metadata = match server
                                            .filesystem
                                            .async_symlink_metadata(&sub_path)
                                            .await
                                        {
                                            Ok(metadata) => metadata,
                                            Err(_) => continue,
                                        };

                                        let mut new_path = relative_path.to_vec();
                                        new_path.push(file_name);

                                        total_size += metadata.len();

                                        if is_dir {
                                            let size = recursive_size(
                                                server,
                                                &sub_path,
                                                &new_path,
                                                disk_usage,
                                                disable_directory_size,
                                            )
                                            .await;

                                            if !disable_directory_size {
                                                disk_usage.update_size(&new_path, size as i64);
                                            }
                                        }
                                    }
                                }

                                total_size
                            })
                        }

                        let total_size = recursive_size(
                            &server,
                            &server.filesystem.base_path,
                            &[],
                            &mut tmp_disk_usage,
                            disable_directory_size,
                        )
                        .await;

                        let total_entry_size =
                            tmp_disk_usage.entries.values().map(|e| e.size).sum::<u64>();

                        *server.filesystem.disk_usage.write().await = tmp_disk_usage;
                        server
                            .filesystem
                            .disk_usage_cached
                            .store(total_size + total_entry_size, Ordering::Relaxed);

                        tracing::debug!(
                            path = %server.filesystem.base_path.display(),
                            "{} bytes disk usage",
                            server.filesystem.disk_usage_cached.load(Ordering::Relaxed)
                        );

                        Ok(())
                    };

                    match run_inner().await {
                        Ok(_) => {
                            tracing::debug!(
                                path = %server.filesystem.base_path.display(),
                                "disk usage check completed successfully"
                            );
                        }
                        Err(err) => {
                            tracing::error!(
                                path = %server.filesystem.base_path.display(),
                                "disk usage check failed: {}",
                                err
                            );
                        }
                    }

                    tokio::time::sleep(std::time::Duration::from_secs(check_interval)).await;
                }
            }
        }));
    }

    pub async fn setup(&self, server: &crate::server::Server) {
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

        match tokio::task::spawn_blocking({
            let base_path = base_path.clone();

            move || std::os::unix::fs::chown(base_path, Some(owner_uid), Some(owner_gid))
        })
        .await
        {
            Ok(Ok(())) => {
                tracing::debug!(
                    path = %base_path.display(),
                    "set ownership for server base directory"
                );
            }
            Ok(Err(err)) => {
                tracing::error!(
                    path = %base_path.display(),
                    "failed to set ownership for server base directory: {}",
                    err
                );
            }
            Err(err) => {
                tracing::error!(
                    path = %base_path.display(),
                    "failed to set ownership for server base directory: {}",
                    err
                );
            }
        }

        if self.cap_filesystem.is_uninitialized().await {
            match cap_std::fs::Dir::open_ambient_dir(&self.base_path, cap_std::ambient_authority())
            {
                Ok(dir) => {
                    *self.cap_filesystem.inner.write().await = Some(Arc::new(dir));
                    self.setup_disk_checker(server).await;
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

    pub async fn attach(&self, server: &crate::server::Server) {
        if let Err(err) = limiter::attach(self).await {
            tracing::error!(
                path = %self.base_path.display(),
                "failed to attach server base directory: {}",
                err
            );
        }

        if self.cap_filesystem.is_uninitialized().await {
            match cap_std::fs::Dir::open_ambient_dir(&self.base_path, cap_std::ambient_authority())
            {
                Ok(dir) => {
                    *self.cap_filesystem.inner.write().await = Some(Arc::new(dir));
                    self.setup_disk_checker(server).await;
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
        if let Some(disk_checker) = self.disk_checker.lock().await.take() {
            disk_checker.abort();
        }

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
                let components = self.path_to_components(real_path);

                self.disk_usage
                    .read()
                    .await
                    .get_size(&components)
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
            .unwrap(),
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
