use std::{
    collections::HashMap,
    fs::Metadata,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::{
        Arc, RwLock, RwLockReadGuard,
        atomic::{AtomicI64, AtomicU64},
    },
};
use tokio::io::AsyncReadExt;

pub mod archive;
pub mod pull;
mod usage;
pub mod writer;

pub struct Filesystem {
    checker: tokio::task::JoinHandle<()>,

    pub base_path: PathBuf,

    pub disk_limit: AtomicI64,
    pub disk_usage_cached: Arc<AtomicU64>,
    pub disk_usage: Arc<RwLock<usage::DiskUsage>>,

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
    ) -> Self {
        let disk_usage = Arc::new(RwLock::new(usage::DiskUsage::new()));
        let disk_usage_cached = Arc::new(AtomicU64::new(0));

        Self {
            checker: tokio::task::spawn({
                let disk_usage = Arc::clone(&disk_usage);
                let disk_usage_cached = Arc::clone(&disk_usage_cached);
                let base_path = base_path.clone();

                async move {
                    loop {
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
                                for entry in path.read_dir().unwrap().flatten() {
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

                        tokio::time::sleep(tokio::time::Duration::from_secs(check_interval)).await;
                    }
                }
            }),
            base_path,

            disk_limit: AtomicI64::new(disk_limit as i64),
            disk_usage_cached,
            disk_usage,

            owner_uid: config.system.user.uid,
            owner_gid: config.system.user.gid,

            pulls: RwLock::new(HashMap::new()),
        }
    }

    pub fn pulls(&self) -> RwLockReadGuard<'_, HashMap<uuid::Uuid, Arc<RwLock<pull::Download>>>> {
        let mut pulls = self.pulls.write().unwrap();
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
        drop(pulls);

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

    pub fn base(&self) -> String {
        self.base_path.to_string_lossy().to_string()
    }

    #[inline]
    pub fn relative_path(&self, path: &Path) -> Option<PathBuf> {
        path.canonicalize()
            .ok()?
            .strip_prefix(&self.base_path)
            .ok()
            .map(|p| p.to_path_buf())
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

    pub fn safe_path(&self, path: &str) -> Option<PathBuf> {
        let safe_path = self.base_path.join(path.trim_start_matches('/'));

        let safe_path = if let Some(file_name) = safe_path.file_name() {
            let safe_parent = safe_path.parent()?;
            let safe_parent = safe_parent.canonicalize().ok()?;

            safe_parent.join(file_name)
        } else {
            safe_path.canonicalize().ok()?
        };

        if !safe_path.starts_with(&self.base_path) {
            return None;
        }

        Some(safe_path)
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

        if !self.is_safe_path(&old_parent) || !self.is_safe_path(&new_parent) {
            return Err(tokio::io::Error::new(
                tokio::io::ErrorKind::PermissionDenied,
                "Unsafe path",
            ));
        }

        let abs_new_path = new_parent.join(new_path.file_name().unwrap());

        if !self.is_safe_path(&abs_new_path) {
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

        let mut disk_usage = self.disk_usage.write().unwrap();
        disk_usage.update_size(path, delta);

        true
    }

    pub fn allocate_in_path(&self, path: &Path, delta: i64) -> bool {
        let components = self.path_to_components(path);

        self.allocate_in_path_raw(&components, delta)
    }

    pub fn is_safe_path(&self, path: &Path) -> bool {
        path.starts_with(&self.base_path)
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
                    tokio::fs::remove_dir_all(&path).await.unwrap_or(());
                } else {
                    tokio::fs::remove_file(&path).await.unwrap_or(());
                }
            }
        }
    }

    pub async fn chown_path(&self, path: &Path) {
        fn recursive_chown(path: &Path, owner_uid: u32, owner_gid: u32) {
            let metadata = path.symlink_metadata().unwrap();
            if metadata.is_dir() {
                for entry in path.read_dir().unwrap() {
                    let entry = entry.unwrap();
                    let path = entry.path();

                    recursive_chown(&path, owner_uid, owner_gid);
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
        tokio::fs::create_dir_all(&self.base_path)
            .await
            .unwrap_or(());
        std::os::unix::fs::chown(&self.base_path, Some(self.owner_uid), Some(self.owner_gid))
            .unwrap();
    }

    pub async fn destroy(&self) {
        tokio::fs::remove_dir_all(&self.base_path)
            .await
            .unwrap_or(());
    }

    pub async fn to_api_entry(
        &self,
        path: PathBuf,
        metadata: Metadata,
    ) -> crate::models::DirectoryEntry {
        let size = if metadata.is_dir() {
            let disk_usage = self.disk_usage.read().unwrap();
            let components = self.path_to_components(&path);

            disk_usage.get_size(&components).unwrap_or(0)
        } else {
            metadata.len()
        };

        let mime = if metadata.is_dir() {
            "inode/directory"
        } else if metadata.is_symlink() {
            "inode/symlink"
        } else {
            let mut buffer = [0; 128];
            let mut file = tokio::fs::File::open(&path).await.unwrap();
            let bytes_read = file.read(&mut buffer).await.unwrap_or(0);

            if let Some(mime) = infer::get(&buffer[..bytes_read]) {
                mime.mime_type()
            } else if std::str::from_utf8(&buffer[..bytes_read]).is_ok() {
                "text/plain"
            } else {
                "application/octet-stream"
            }
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
            directory: metadata.is_dir(),
            file: metadata.is_file(),
            symlink: metadata.is_symlink(),
            mime,
        }
    }
}

impl Drop for Filesystem {
    fn drop(&mut self) {
        self.checker.abort();
    }
}
