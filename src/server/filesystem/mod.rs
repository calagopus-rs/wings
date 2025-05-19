use std::{
    collections::HashMap,
    fs::Metadata,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::{
        Arc, RwLock, RwLockReadGuard,
        atomic::{AtomicBool, AtomicI64, AtomicU64},
    },
};
use tokio::io::AsyncReadExt;

pub mod archive;
pub mod pull;
mod usage;
pub mod writer;

pub struct Filesystem {
    checker_abort: Arc<AtomicBool>,

    pub base_path: PathBuf,

    pub disk_limit: AtomicI64,
    pub disk_usage_cached: Arc<AtomicU64>,
    pub disk_usage: Arc<RwLock<usage::DiskUsage>>,
    pub disk_ignored: Arc<RwLock<ignore::overrides::Override>>,

    pub owner_uid: u32,
    pub owner_gid: u32,

    pub pulls: RwLock<HashMap<uuid::Uuid, Arc<RwLock<pull::Download>>>>,
}

impl Filesystem {
    pub fn new(
        base_path: PathBuf,
        disk_limit: u64,
        check_interval: u64,
        config: &crate::config::Config,
        deny_list: &[String],
    ) -> Self {
        let disk_usage = Arc::new(RwLock::new(usage::DiskUsage::new()));
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
                    if checker_abort.load(std::sync::atomic::Ordering::Relaxed) {
                        break;
                    }

                    tracing::debug!(
                        path = %base_path.display(),
                        "checking disk usage"
                    );

                    let mut tmp_disk_usage = usage::DiskUsage::new();

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

                                    if metadata.is_dir() {
                                        let size = recursive_size(&path, &new_path, disk_usage);
                                        disk_usage.update_size(&new_path, size as i64);
                                    } else {
                                        total_size += metadata.len();
                                    }
                                }
                            }
                        } else {
                            total_size += metadata.len();
                        }

                        total_size
                    }

                    let total_size = recursive_size(&base_path, &[], &mut tmp_disk_usage);
                    let total_entry_size =
                        tmp_disk_usage.entries.values().map(|e| e.size).sum::<u64>();

                    *disk_usage.write().unwrap() = tmp_disk_usage;
                    disk_usage_cached.store(
                        total_size + total_entry_size,
                        std::sync::atomic::Ordering::Relaxed,
                    );

                    tracing::debug!(
                        path = %base_path.display(),
                        "{} bytes disk usage",
                        disk_usage_cached.load(std::sync::atomic::Ordering::Relaxed)
                    );

                    std::thread::sleep(std::time::Duration::from_secs(check_interval));
                }
            }
        });

        Self {
            checker_abort,

            base_path,

            disk_limit: AtomicI64::new(disk_limit as i64),
            disk_usage_cached,
            disk_usage,
            disk_ignored: Arc::new(RwLock::new(disk_ignored.build().unwrap())),

            owner_uid: config.system.user.uid,
            owner_gid: config.system.user.gid,

            pulls: RwLock::new(HashMap::new()),
        }
    }

    pub fn update_ignored(&self, deny_list: &[String]) {
        let mut disk_ignored = ignore::overrides::OverrideBuilder::new(&self.base_path);
        for entry in deny_list {
            disk_ignored.add(entry).ok();
        }

        *self.disk_ignored.write().unwrap() = disk_ignored.build().unwrap();
    }

    pub fn is_ignored(&self, path: &Path, is_dir: bool) -> bool {
        self.disk_ignored
            .read()
            .unwrap()
            .matched(path, is_dir)
            .invert()
            .is_ignore()
    }

    pub fn pulls(&self) -> RwLockReadGuard<'_, HashMap<uuid::Uuid, Arc<RwLock<pull::Download>>>> {
        if let Ok(mut pulls) = self.pulls.try_write() {
            for key in pulls.keys().cloned().collect::<Vec<_>>() {
                if let Some(download) = pulls.get(&key) {
                    if download
                        .read()
                        .unwrap()
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

        self.pulls.read().unwrap()
    }

    #[inline]
    pub fn cached_usage(&self) -> u64 {
        self.disk_usage_cached
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    #[inline]
    pub fn disk_limit(&self) -> i64 {
        self.disk_limit.load(std::sync::atomic::Ordering::Relaxed)
    }

    #[inline]
    pub fn is_full(&self) -> bool {
        self.disk_limit() != 0 && self.cached_usage() >= self.disk_limit() as u64
    }

    #[inline]
    pub fn base(&self) -> String {
        self.base_path.to_string_lossy().to_string()
    }

    #[inline]
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

        result
    }

    #[inline]
    pub fn relative_path(&self, path: &Path) -> Option<PathBuf> {
        let parent = path.parent()?.canonicalize().ok()?;
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
    pub async fn safe_path(&self, path: &str) -> Option<PathBuf> {
        let path = self.base_path.join(path.trim_start_matches('/'));

        if let Ok(safe_path) = tokio::fs::canonicalize(&path).await {
            if safe_path.starts_with(&self.base_path) {
                Some(safe_path)
            } else {
                None
            }
        } else {
            let safe_path = Self::resolve_path(&path);
            if safe_path.starts_with(&self.base_path) {
                Some(safe_path)
            } else {
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

    pub async fn truncate_path(&self, path: &PathBuf) -> tokio::io::Result<()> {
        let metadata = path.symlink_metadata()?;

        let components = self.path_to_components(path);
        let size = if metadata.is_dir() {
            let disk_usage = self.disk_usage.read().unwrap();
            disk_usage.get_size(&components).unwrap_or(0)
        } else {
            metadata.len()
        };

        self.allocate_in_path(path, -(size as i64));

        if metadata.is_dir() && size > 0 {
            let mut disk_usage = self.disk_usage.write().unwrap();
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

        let metadata = old_path.symlink_metadata()?;
        let is_dir = metadata.is_dir();

        let old_parent = old_path.parent().unwrap().canonicalize()?;
        let new_parent = new_path.parent().unwrap().canonicalize()?;

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
            let mut disk_usage = self.disk_usage.write().unwrap();

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

            self.allocate_in_path(&old_parent, -size);
            self.allocate_in_path(&new_parent, size);
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
    pub fn allocate_in_path_raw(&self, path: &[String], delta: i64) -> bool {
        if delta == 0 {
            return true;
        }

        if delta > 0 {
            let current_usage = self
                .disk_usage_cached
                .load(std::sync::atomic::Ordering::Relaxed) as i64;

            if self.disk_limit() != 0 && current_usage + delta > self.disk_limit() {
                return false;
            }
        }

        if delta > 0 {
            self.disk_usage_cached
                .fetch_add(delta as u64, std::sync::atomic::Ordering::Relaxed);
        } else {
            let abs_size = delta.unsigned_abs();
            let current = self
                .disk_usage_cached
                .load(std::sync::atomic::Ordering::Relaxed);

            if current >= abs_size {
                self.disk_usage_cached
                    .fetch_sub(abs_size, std::sync::atomic::Ordering::Relaxed);
            } else {
                self.disk_usage_cached
                    .store(0, std::sync::atomic::Ordering::Relaxed);
            }
        }

        self.disk_usage.write().unwrap().update_size(path, delta);

        true
    }

    #[inline]
    pub fn allocate_in_path(&self, path: &Path, delta: i64) -> bool {
        let components = self.path_to_components(path);

        self.allocate_in_path_raw(&components, delta)
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
        self.disk_usage.write().unwrap().clear();
        self.disk_usage_cached
            .store(0, std::sync::atomic::Ordering::Relaxed);

        let mut directory = tokio::fs::read_dir(&self.base_path).await.unwrap();
        while let Ok(Some(entry)) = directory.next_entry().await {
            let path = entry.path();

            if let Ok(metadata) = path.symlink_metadata() {
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
            let owner_uid = self.owner_uid;
            let owner_gid = self.owner_gid;

            move || {
                recursive_chown(&path, owner_uid, owner_gid);
            }
        })
        .await
        .unwrap()
    }

    pub async fn get_pteroignore(&self) -> Option<String> {
        let path = self.base_path.join(".pteroignore");
        if path.symlink_metadata().ok()?.is_file() {
            tokio::fs::read_to_string(&path).await.ok()
        } else {
            None
        }
    }

    pub async fn setup(&self) {
        let base_path = self.base_path.clone();
        let owner_uid = self.owner_uid;
        let owner_gid = self.owner_gid;

        tokio::fs::create_dir_all(&base_path).await.unwrap_or(());
        tokio::task::spawn_blocking(move || {
            std::os::unix::fs::chown(&base_path, Some(owner_uid), Some(owner_gid)).unwrap();
        })
        .await
        .unwrap();
    }

    pub async fn destroy(&self) {
        self.checker_abort
            .store(true, std::sync::atomic::Ordering::Relaxed);
        if let Err(err) = tokio::fs::remove_dir_all(&self.base_path).await {
            tracing::error!(
                path = %self.base_path.display(),
                "failed to delete server base directory for: {}",
                err
            );
        }
    }

    #[inline]
    pub fn to_api_entry_buffer(
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
            let disk_usage = self.disk_usage.read().unwrap();
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

        #[inline]
        fn format_mode(mode: u32) -> String {
            let mut mode_str = String::new();

            let type_chars = "dalTLDpSugct?";

            let file_type = (mode >> 28) & 0xF;
            if file_type < type_chars.len() as u32 {
                mode_str.push(type_chars.chars().nth(file_type as usize).unwrap());
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

            mode_str
        }

        crate::models::DirectoryEntry {
            name: path.file_name().unwrap().to_string_lossy().to_string(),
            created: chrono::DateTime::from_timestamp(
                metadata
                    .created()
                    .unwrap()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_secs() as i64,
                0,
            )
            .unwrap(),
            modified: chrono::DateTime::from_timestamp(
                metadata
                    .modified()
                    .unwrap()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_secs() as i64,
                0,
            )
            .unwrap(),
            mode: format_mode(metadata.permissions().mode()),
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

                    if let Ok(joined) = joined.canonicalize() {
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
    }
}

impl Drop for Filesystem {
    fn drop(&mut self) {
        self.checker_abort
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }
}
