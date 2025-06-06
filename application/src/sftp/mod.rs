use crate::{
    routes::State,
    server::{
        activity::{Activity, ActivityEvent},
        permissions::{Permission, Permissions},
    },
};
use russh_sftp::protocol::{
    Data, File, FileAttributes, Handle, Name, OpenFlags, Status, StatusCode,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha1::Digest;
use std::{
    collections::HashMap,
    fs::{Metadata, OpenOptions},
    io::SeekFrom,
    net::{IpAddr, SocketAddr},
    os::unix::fs::{FileExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio::{
    io::{AsyncReadExt, AsyncSeekExt},
    sync::Mutex,
};

mod auth;

pub struct Server {
    pub state: State,
}

impl russh::server::Server for Server {
    type Handler = auth::SshSession;

    fn new_client(&mut self, client: Option<SocketAddr>) -> Self::Handler {
        auth::SshSession {
            state: Arc::clone(&self.state),
            server: None,

            user_ip: client.map(|addr| addr.ip()),
            user_uuid: None,
            user_permissions: Default::default(),

            clients: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

struct FileHandle {
    path: PathBuf,
    path_components: Vec<String>,

    file: Option<Arc<std::fs::File>>,
    dir: Option<tokio::fs::ReadDir>,

    consumed: u64,
    size: u64,
}

struct SftpSession {
    state: State,
    server: crate::server::Server,

    user_ip: Option<IpAddr>,
    user_uuid: Option<uuid::Uuid>,
    user_permissions: Permissions,

    handle_id: u64,
    handles: HashMap<String, FileHandle>,
}

impl SftpSession {
    #[inline]
    fn convert_entry(path: &Path, metadata: Metadata) -> File {
        let mut attrs = FileAttributes {
            size: Some(metadata.len()),
            atime: None,
            mtime: Some(
                metadata
                    .modified()
                    .unwrap()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_secs() as u32,
            ),
            permissions: Some(metadata.permissions().mode()),
            ..Default::default()
        };

        attrs.set_dir(metadata.is_dir());
        attrs.set_regular(metadata.is_file());
        attrs.set_symlink(metadata.is_symlink());

        File::new(
            path.file_name().unwrap().to_string_lossy().to_string(),
            attrs,
        )
    }

    #[inline]
    fn next_handle_id(&mut self) -> String {
        let id = self.handle_id;
        self.handle_id += 1;

        format!("{:x}", id)
    }

    #[inline]
    fn has_permission(&self, permission: Permission) -> bool {
        for p in self.user_permissions.iter().copied() {
            if permission.matches(p) {
                return true;
            }
        }

        false
    }

    #[inline]
    fn allow_action(&self) -> bool {
        !self.server.is_locked_state()
    }
}

impl russh_sftp::server::Handler for SftpSession {
    type Error = StatusCode;

    #[inline]
    fn unimplemented(&self) -> Self::Error {
        StatusCode::OpUnsupported
    }

    async fn init(
        &mut self,
        _version: u32,
        _extensions: HashMap<String, String>,
    ) -> Result<russh_sftp::protocol::Version, Self::Error> {
        let mut version = russh_sftp::protocol::Version::new();
        version
            .extensions
            .insert("check-file".to_string(), "1".to_string());
        version
            .extensions
            .insert("copy-file".to_string(), "1".to_string());

        Ok(version)
    }

    #[inline]
    async fn close(&mut self, id: u32, handle: String) -> Result<Status, Self::Error> {
        self.handles.remove(&handle);

        Ok(Status {
            id,
            status_code: StatusCode::Ok,
            error_message: "Ok".to_string(),
            language_tag: "en-US".to_string(),
        })
    }

    #[inline]
    async fn realpath(&mut self, id: u32, path: String) -> Result<Name, Self::Error> {
        if path == "/.." || path == "." {
            return Ok(Name {
                id,
                files: vec![File::dummy("/".to_string())],
            });
        }

        if let Some(Ok(path)) = self.server.filesystem.safe_path(&path).await.map(|p| {
            p.strip_prefix(&self.server.filesystem.base_path)
                .map(|p| p.to_path_buf())
        }) {
            Ok(Name {
                id,
                files: vec![File::dummy(format!("/{}", path.display()))],
            })
        } else {
            Err(StatusCode::NoSuchFile)
        }
    }

    async fn opendir(&mut self, id: u32, path: String) -> Result<Handle, Self::Error> {
        if !self.allow_action() {
            return Err(StatusCode::PermissionDenied);
        }

        if self.handles.len() >= 256 {
            return Err(StatusCode::Failure);
        }

        if !self.has_permission(Permission::FileRead) {
            return Err(StatusCode::PermissionDenied);
        }

        let handle = self.next_handle_id();

        if let Some(path) = self.server.filesystem.safe_path(&path).await {
            if !path.is_dir() {
                return Err(StatusCode::NoSuchFile);
            }

            if self.server.filesystem.is_ignored(&path, false).await {
                return Err(StatusCode::NoSuchFile);
            }

            let path_components = self.server.filesystem.path_to_components(&path);
            let dir = match tokio::fs::read_dir(&path).await {
                Ok(dir) => dir,
                Err(_) => return Err(StatusCode::NoSuchFile),
            };

            self.handles.insert(
                handle.clone(),
                FileHandle {
                    path,
                    path_components,
                    file: None,
                    dir: Some(dir),
                    consumed: 0,
                    size: 0,
                },
            );

            Ok(Handle { id, handle })
        } else {
            Err(StatusCode::NoSuchFile)
        }
    }

    async fn readdir(&mut self, id: u32, handle: String) -> Result<Name, Self::Error> {
        if !self.allow_action() {
            return Err(StatusCode::PermissionDenied);
        }

        let handle = match self.handles.get_mut(&handle) {
            Some(handle) => handle,
            None => return Err(StatusCode::NoSuchFile),
        };

        if handle.consumed >= self.state.config.system.sftp.directory_entry_limit {
            return Err(StatusCode::Eof);
        }

        let dir = match &mut handle.dir {
            Some(dir) => dir,
            None => return Err(StatusCode::NoSuchFile),
        };

        let mut files = Vec::new();

        loop {
            let file = match dir.next_entry().await {
                Ok(file) => file,
                Err(_) => return Err(StatusCode::NoSuchFile),
            };

            let file = match file {
                Some(file) => file,
                None => {
                    if files.is_empty() {
                        return Err(StatusCode::Eof);
                    }

                    break;
                }
            };

            let path = file.path();
            let metadata = match tokio::fs::symlink_metadata(&path).await {
                Ok(metadata) => metadata,
                Err(_) => continue,
            };

            if self
                .server
                .filesystem
                .is_ignored(&path, metadata.is_dir())
                .await
            {
                continue;
            }

            files.push(Self::convert_entry(&path, metadata));
            handle.consumed += 1;

            if handle.consumed >= self.state.config.system.sftp.directory_entry_limit
                || files.len() >= self.state.config.system.sftp.directory_entry_send_amount
            {
                tracing::debug!(
                    "{} entries sent early in sftp readdir ({} total)",
                    files.len(),
                    handle.consumed,
                );

                break;
            }
        }

        Ok(Name { id, files })
    }

    async fn remove(&mut self, id: u32, filename: String) -> Result<Status, Self::Error> {
        if !self.allow_action() {
            return Err(StatusCode::PermissionDenied);
        }

        if self.state.config.system.sftp.read_only {
            return Err(StatusCode::PermissionDenied);
        }

        if !self.has_permission(Permission::FileDelete) {
            return Err(StatusCode::PermissionDenied);
        }

        if let Some(path) = self.server.filesystem.safe_path(&filename).await {
            let parent = match path.parent() {
                Some(parent) => parent,
                None => return Err(StatusCode::NoSuchFile),
            };

            if let Ok(metadata) = tokio::fs::symlink_metadata(&path).await {
                if !metadata.is_file() {
                    return Err(StatusCode::NoSuchFile);
                }

                if self
                    .server
                    .filesystem
                    .is_ignored(&path, metadata.is_dir())
                    .await
                {
                    return Err(StatusCode::NoSuchFile);
                }

                if tokio::fs::remove_file(&path).await.is_err() {
                    return Err(StatusCode::NoSuchFile);
                }

                self.server
                    .filesystem
                    .allocate_in_path(parent, -(metadata.len() as i64))
                    .await;
                self.server
                    .activity
                    .log_activity(Activity {
                        event: ActivityEvent::SftpDelete,
                        user: self.user_uuid,
                        ip: self.user_ip,
                        metadata: Some(json!({
                            "files": [self.server.filesystem.relative_path(&path)],
                        })),
                        timestamp: chrono::Utc::now(),
                    })
                    .await;
            }

            Ok(Status {
                id,
                status_code: StatusCode::Ok,
                error_message: "Ok".to_string(),
                language_tag: "en-US".to_string(),
            })
        } else {
            Err(StatusCode::NoSuchFile)
        }
    }

    async fn rmdir(&mut self, id: u32, path: String) -> Result<Status, Self::Error> {
        if !self.allow_action() {
            return Err(StatusCode::PermissionDenied);
        }

        if self.state.config.system.sftp.read_only {
            return Err(StatusCode::PermissionDenied);
        }

        if !self.has_permission(Permission::FileDelete) {
            return Err(StatusCode::PermissionDenied);
        }

        if let Some(path) = self.server.filesystem.safe_path(&path).await {
            if !path.is_dir() {
                return Err(StatusCode::NoSuchFile);
            }

            if self.server.filesystem.is_ignored(&path, true).await {
                return Err(StatusCode::NoSuchFile);
            }

            if path != self.server.filesystem.base_path
                && tokio::fs::remove_dir(&path).await.is_err()
            {
                return Err(StatusCode::NoSuchFile);
            }

            self.server
                .activity
                .log_activity(Activity {
                    event: ActivityEvent::SftpDelete,
                    user: self.user_uuid,
                    ip: self.user_ip,
                    metadata: Some(json!({
                        "files": [self.server.filesystem.relative_path(&path)],
                    })),
                    timestamp: chrono::Utc::now(),
                })
                .await;

            Ok(Status {
                id,
                status_code: StatusCode::Ok,
                error_message: "Ok".to_string(),
                language_tag: "en-US".to_string(),
            })
        } else {
            Err(StatusCode::NoSuchFile)
        }
    }

    async fn mkdir(
        &mut self,
        id: u32,
        path: String,
        attrs: FileAttributes,
    ) -> Result<Status, Self::Error> {
        if !self.allow_action() {
            return Err(StatusCode::PermissionDenied);
        }

        if self.state.config.system.sftp.read_only {
            return Err(StatusCode::PermissionDenied);
        }

        if !self.has_permission(Permission::FileCreate) {
            return Err(StatusCode::PermissionDenied);
        }

        if let Some(path) = self.server.filesystem.safe_path(&path).await {
            if path.exists() {
                return Err(StatusCode::Failure);
            }

            if tokio::fs::create_dir(&path).await.is_err() {
                return Err(StatusCode::NoSuchFile);
            }

            self.server.filesystem.chown_path(&path).await;
            if let Some(permissions) = attrs.permissions {
                let mut permissions = std::fs::Permissions::from_mode(permissions);
                permissions.set_mode(permissions.mode() & 0o777);

                tokio::fs::set_permissions(&path, permissions)
                    .await
                    .unwrap()
            }

            self.server
                .activity
                .log_activity(Activity {
                    event: ActivityEvent::SftpCreateDirectory,
                    user: self.user_uuid,
                    ip: self.user_ip,
                    metadata: Some(json!({
                        "files": [self.server.filesystem.relative_path(&path)],
                    })),
                    timestamp: chrono::Utc::now(),
                })
                .await;

            Ok(Status {
                id,
                status_code: StatusCode::Ok,
                error_message: "Ok".to_string(),
                language_tag: "en-US".to_string(),
            })
        } else {
            Err(StatusCode::NoSuchFile)
        }
    }

    async fn rename(
        &mut self,
        id: u32,
        old_path: String,
        new_path: String,
    ) -> Result<Status, Self::Error> {
        if !self.allow_action() {
            return Err(StatusCode::PermissionDenied);
        }

        if self.state.config.system.sftp.read_only {
            return Err(StatusCode::PermissionDenied);
        }

        if !self.has_permission(Permission::FileUpdate) {
            return Err(StatusCode::PermissionDenied);
        }

        if let Some(old_path) = self.server.filesystem.safe_path(&old_path).await {
            let old_metadata = match tokio::fs::symlink_metadata(&old_path).await {
                Ok(metadata) => metadata,
                Err(_) => return Err(StatusCode::NoSuchFile),
            };

            if let Some(new_path) = self.server.filesystem.safe_path(&new_path).await {
                if new_path.exists()
                    || self
                        .server
                        .filesystem
                        .is_ignored(&old_path, old_metadata.is_dir())
                        .await
                    || self
                        .server
                        .filesystem
                        .is_ignored(&new_path, old_metadata.is_dir())
                        .await
                {
                    return Err(StatusCode::NoSuchFile);
                }

                if self
                    .server
                    .filesystem
                    .rename_path(&old_path, &new_path)
                    .await
                    .is_err()
                {
                    return Err(StatusCode::NoSuchFile);
                }

                self.server
                    .activity
                    .log_activity(Activity {
                        event: ActivityEvent::SftpRename,
                        user: self.user_uuid,
                        ip: self.user_ip,
                        metadata: Some(json!({
                            "files": [
                                {
                                    "from": self.server.filesystem.relative_path(&old_path),
                                    "to": self.server.filesystem.relative_path(&new_path),
                                }
                            ],
                        })),
                        timestamp: chrono::Utc::now(),
                    })
                    .await;

                Ok(Status {
                    id,
                    status_code: StatusCode::Ok,
                    error_message: "Ok".to_string(),
                    language_tag: "en-US".to_string(),
                })
            } else {
                Err(StatusCode::NoSuchFile)
            }
        } else {
            Err(StatusCode::NoSuchFile)
        }
    }

    async fn setstat(
        &mut self,
        id: u32,
        path: String,
        attrs: FileAttributes,
    ) -> Result<Status, Self::Error> {
        if !self.allow_action() {
            return Err(StatusCode::PermissionDenied);
        }

        if self.state.config.system.sftp.read_only {
            return Err(StatusCode::PermissionDenied);
        }

        if !self.has_permission(Permission::FileUpdate) {
            return Err(StatusCode::PermissionDenied);
        }

        if let Some(path) = self.server.filesystem.safe_path(&path).await {
            let metadata = match tokio::fs::symlink_metadata(&path).await {
                Ok(metadata) => metadata,
                Err(_) => return Err(StatusCode::NoSuchFile),
            };

            if self
                .server
                .filesystem
                .is_ignored(&path, metadata.is_dir())
                .await
            {
                return Err(StatusCode::NoSuchFile);
            }

            if let Some(permissions) = attrs.permissions {
                let mut permissions = std::fs::Permissions::from_mode(permissions);
                permissions.set_mode(permissions.mode() & 0o777);

                tokio::fs::set_permissions(&path, permissions)
                    .await
                    .unwrap();
            }

            Ok(Status {
                id,
                status_code: StatusCode::Ok,
                error_message: "Ok".to_string(),
                language_tag: "en-US".to_string(),
            })
        } else {
            Err(StatusCode::NoSuchFile)
        }
    }

    async fn fsetstat(
        &mut self,
        id: u32,
        handle: String,
        attrs: FileAttributes,
    ) -> Result<Status, Self::Error> {
        if !self.allow_action() {
            return Err(StatusCode::PermissionDenied);
        }

        let handle = match self.handles.get(&handle) {
            Some(handle) => handle,
            None => return Err(StatusCode::NoSuchFile),
        };

        self.setstat(id, handle.path.to_string_lossy().to_string(), attrs)
            .await
    }

    async fn stat(
        &mut self,
        id: u32,
        path: String,
    ) -> Result<russh_sftp::protocol::Attrs, Self::Error> {
        if !self.has_permission(Permission::FileRead) {
            return Err(StatusCode::PermissionDenied);
        }

        if let Some(path) = self.server.filesystem.safe_path(&path).await {
            let metadata = match tokio::fs::symlink_metadata(&path).await {
                Ok(metadata) => metadata,
                Err(_) => return Err(StatusCode::NoSuchFile),
            };

            if self
                .server
                .filesystem
                .is_ignored(&path, metadata.is_dir())
                .await
            {
                return Err(StatusCode::NoSuchFile);
            }

            let file = Self::convert_entry(&path, metadata);

            Ok(russh_sftp::protocol::Attrs {
                id,
                attrs: file.attrs,
            })
        } else {
            Err(StatusCode::NoSuchFile)
        }
    }

    async fn fstat(
        &mut self,
        id: u32,
        handle: String,
    ) -> Result<russh_sftp::protocol::Attrs, Self::Error> {
        if !self.allow_action() {
            return Err(StatusCode::PermissionDenied);
        }

        let handle = match self.handles.get(&handle) {
            Some(handle) => handle,
            None => return Err(StatusCode::NoSuchFile),
        };

        self.stat(id, handle.path.to_string_lossy().to_string())
            .await
    }

    async fn lstat(
        &mut self,
        id: u32,
        path: String,
    ) -> Result<russh_sftp::protocol::Attrs, Self::Error> {
        if !self.allow_action() {
            return Err(StatusCode::PermissionDenied);
        }

        if !self.has_permission(Permission::FileRead) {
            return Err(StatusCode::PermissionDenied);
        }

        if let Some(path) = self.server.filesystem.safe_symlink_path(&path).await {
            let metadata = match tokio::fs::symlink_metadata(&path).await {
                Ok(metadata) => metadata,
                Err(_) => return Err(StatusCode::NoSuchFile),
            };

            if self
                .server
                .filesystem
                .is_ignored(&path, metadata.is_dir())
                .await
            {
                return Err(StatusCode::NoSuchFile);
            }

            let file = Self::convert_entry(&path, metadata);

            Ok(russh_sftp::protocol::Attrs {
                id,
                attrs: file.attrs,
            })
        } else {
            Err(StatusCode::NoSuchFile)
        }
    }

    async fn readlink(&mut self, id: u32, path: String) -> Result<Name, Self::Error> {
        if !self.allow_action() {
            return Err(StatusCode::PermissionDenied);
        }

        if !self.has_permission(Permission::FileRead) {
            return Err(StatusCode::PermissionDenied);
        }

        if let Some(path) = self.server.filesystem.safe_symlink_path(&path).await {
            let metadata = match tokio::fs::symlink_metadata(&path).await {
                Ok(metadata) => metadata,
                Err(_) => return Err(StatusCode::NoSuchFile),
            };

            if self
                .server
                .filesystem
                .is_ignored(&path, metadata.is_dir())
                .await
            {
                return Err(StatusCode::NoSuchFile);
            }

            let file = Self::convert_entry(&path, metadata);

            Ok(Name {
                id,
                files: vec![file],
            })
        } else {
            Err(StatusCode::NoSuchFile)
        }
    }

    async fn symlink(
        &mut self,
        id: u32,
        linkpath: String,
        targetpath: String,
    ) -> Result<Status, Self::Error> {
        if !self.allow_action() {
            return Err(StatusCode::PermissionDenied);
        }

        if self.state.config.system.sftp.read_only {
            return Err(StatusCode::PermissionDenied);
        }

        if !self.has_permission(Permission::FileCreate) {
            return Err(StatusCode::PermissionDenied);
        }

        if linkpath == targetpath {
            return Err(StatusCode::NoSuchFile);
        }

        if let Some(linkpath) = self.server.filesystem.safe_symlink_path(&linkpath).await {
            if linkpath.exists() {
                return Err(StatusCode::NoSuchFile);
            }

            if let Some(targetpath) = self.server.filesystem.safe_path(&targetpath).await {
                let metadata = match tokio::fs::symlink_metadata(&targetpath).await {
                    Ok(metadata) => metadata,
                    Err(_) => return Err(StatusCode::NoSuchFile),
                };

                if self
                    .server
                    .filesystem
                    .is_ignored(&targetpath, metadata.is_dir())
                    .await
                {
                    return Err(StatusCode::NoSuchFile);
                }

                if tokio::fs::symlink(&targetpath, &linkpath).await.is_err() {
                    return Err(StatusCode::NoSuchFile);
                }

                self.server
                    .activity
                    .log_activity(Activity {
                        event: ActivityEvent::SftpCreate,
                        user: self.user_uuid,
                        ip: self.user_ip,
                        metadata: Some(json!({
                            "files": [self.server.filesystem.relative_path(&linkpath)],
                        })),
                        timestamp: chrono::Utc::now(),
                    })
                    .await;

                Ok(Status {
                    id,
                    status_code: StatusCode::Ok,
                    error_message: "Ok".to_string(),
                    language_tag: "en-US".to_string(),
                })
            } else {
                Err(StatusCode::NoSuchFile)
            }
        } else {
            Err(StatusCode::NoSuchFile)
        }
    }

    async fn open(
        &mut self,
        id: u32,
        filename: String,
        pflags: russh_sftp::protocol::OpenFlags,
        _attrs: FileAttributes,
    ) -> Result<Handle, Self::Error> {
        if !self.allow_action() {
            return Err(StatusCode::PermissionDenied);
        }

        if self.handles.len() >= 256 {
            return Err(StatusCode::Failure);
        }

        if (pflags.contains(OpenFlags::WRITE) || pflags.contains(OpenFlags::APPEND))
            && !self.has_permission(Permission::FileUpdate)
        {
            return Err(StatusCode::PermissionDenied);
        }
        if pflags.contains(OpenFlags::CREATE) && !self.has_permission(Permission::FileCreate) {
            return Err(StatusCode::PermissionDenied);
        }
        if pflags.contains(OpenFlags::TRUNCATE) && !self.has_permission(Permission::FileDelete) {
            return Err(StatusCode::PermissionDenied);
        }
        if pflags.contains(OpenFlags::READ) && !self.has_permission(Permission::FileReadContent) {
            return Err(StatusCode::PermissionDenied);
        }

        let handle = self.next_handle_id();

        if let Some(path) = self.server.filesystem.safe_path(&filename).await {
            if path.exists() && !path.is_file() {
                return Err(StatusCode::NoSuchFile);
            }

            if self.server.filesystem.is_ignored(&path, false).await {
                return Err(StatusCode::NoSuchFile);
            }

            let mut activity_event = None;
            if pflags.contains(OpenFlags::TRUNCATE) || pflags.contains(OpenFlags::CREATE) {
                activity_event = Some(ActivityEvent::SftpCreate);
            } else if pflags.contains(OpenFlags::WRITE) || pflags.contains(OpenFlags::APPEND) {
                activity_event = Some(ActivityEvent::SftpWrite);
            }

            let (file, metadata) = tokio::task::spawn_blocking({
                let path = path.clone();

                move || {
                    let file = OpenOptions::from(pflags).open(&path).unwrap();
                    let metadata = file.metadata().unwrap();

                    (file, metadata)
                }
            })
            .await
            .unwrap();

            let path_components = self.server.filesystem.path_to_components(&path);

            if let Some(event) = activity_event {
                self.server
                    .activity
                    .log_activity(Activity {
                        event,
                        user: self.user_uuid,
                        ip: self.user_ip,
                        metadata: Some(json!({
                            "files": [self.server.filesystem.relative_path(&path)],
                        })),
                        timestamp: chrono::Utc::now(),
                    })
                    .await;
            }

            self.handles.insert(
                handle.clone(),
                FileHandle {
                    path,
                    path_components,
                    file: Some(Arc::new(file)),
                    dir: None,
                    consumed: 0,
                    size: metadata.len(),
                },
            );

            Ok(Handle { id, handle })
        } else {
            Err(StatusCode::NoSuchFile)
        }
    }

    #[inline]
    async fn read(
        &mut self,
        id: u32,
        handle: String,
        offset: u64,
        len: u32,
    ) -> Result<russh_sftp::protocol::Data, Self::Error> {
        if !self.allow_action() {
            return Err(StatusCode::PermissionDenied);
        }

        let handle = match self.handles.get_mut(&handle) {
            Some(handle) => handle,
            None => return Err(StatusCode::NoSuchFile),
        };

        if handle.consumed >= handle.size || offset >= handle.size {
            return Err(StatusCode::Eof);
        }

        let file = match &handle.file {
            Some(file) => file,
            None => {
                return Err(StatusCode::NoSuchFile);
            }
        };

        let buf = tokio::task::spawn_blocking({
            let file = Arc::clone(file);

            move || {
                let mut buf = vec![0; len.min(16 * 1024 * 1024) as usize];
                let bytes_read = file.read_at(&mut buf, offset).unwrap();

                buf.truncate(bytes_read);
                buf
            }
        })
        .await
        .unwrap();

        handle.consumed += buf.len() as u64;

        Ok(Data { id, data: buf })
    }

    #[inline]
    async fn write(
        &mut self,
        id: u32,
        handle: String,
        offset: u64,
        data: Vec<u8>,
    ) -> Result<Status, Self::Error> {
        if !self.allow_action() {
            return Err(StatusCode::PermissionDenied);
        }

        let handle = match self.handles.get_mut(&handle) {
            Some(handle) => handle,
            None => return Err(StatusCode::NoSuchFile),
        };

        if self.state.config.system.sftp.read_only {
            return Err(StatusCode::PermissionDenied);
        }

        let file = match &handle.file {
            Some(file) => file,
            None => {
                return Err(StatusCode::NoSuchFile);
            }
        };

        if !self
            .server
            .filesystem
            .allocate_in_path_raw(
                &handle.path_components[0..handle.path_components.len() - 1],
                data.len() as i64,
            )
            .await
        {
            return Err(StatusCode::Failure);
        }

        tokio::task::spawn_blocking({
            let file = Arc::clone(file);

            move || {
                file.write_all_at(&data, offset).unwrap();
                true
            }
        })
        .await
        .unwrap();

        Ok(Status {
            id,
            status_code: StatusCode::Ok,
            error_message: "Ok".to_string(),
            language_tag: "en-US".to_string(),
        })
    }

    async fn extended(
        &mut self,
        id: u32,
        command: String,
        data: Vec<u8>,
    ) -> Result<russh_sftp::protocol::Packet, Self::Error> {
        if !self.allow_action() {
            return Err(StatusCode::PermissionDenied);
        }

        match command.as_str() {
            "check-file" | "check-file-name" => {
                if !self.has_permission(Permission::FileRead) {
                    return Err(StatusCode::PermissionDenied);
                }

                #[derive(Deserialize)]
                struct CheckFileName {
                    file_name: String,
                    hash: String,

                    start_offset: u64,
                    length: u64,
                }

                let request: CheckFileName = match russh_sftp::de::from_bytes(&mut data.into()) {
                    Ok(request) => request,
                    Err(_) => return Err(StatusCode::BadMessage),
                };

                let file_name = if command == "check-file-name" {
                    request.file_name
                } else {
                    match self.handles.get(&request.file_name) {
                        Some(handle) => handle.path.to_string_lossy().to_string(),
                        None => return Err(StatusCode::NoSuchFile),
                    }
                };

                if let Some(path) = self.server.filesystem.safe_path(&file_name).await {
                    if path.exists() {
                        if let Ok(metadata) = tokio::fs::symlink_metadata(&path).await {
                            if metadata.is_file() {
                                if self
                                    .server
                                    .filesystem
                                    .is_ignored(&path, metadata.is_dir())
                                    .await
                                {
                                    return Err(StatusCode::NoSuchFile);
                                }

                                let mut file = match tokio::fs::File::open(&path).await {
                                    Ok(file) => file,
                                    Err(_) => return Err(StatusCode::NoSuchFile),
                                };

                                if request.start_offset != 0 {
                                    file.seek(SeekFrom::Start(request.start_offset))
                                        .await
                                        .map_err(|_| StatusCode::Failure)?;
                                }
                                let mut total_bytes_read = 0;

                                let mut hash_algorithm = None;
                                for h in request.hash.split(',') {
                                    if ["md5", "sha1", "sha256", "sha512"].contains(&h) {
                                        hash_algorithm = Some(h);
                                        break;
                                    }
                                }

                                let hash: Vec<u8> = match hash_algorithm {
                                    Some("md5") => {
                                        let mut hasher = md5::Context::new();

                                        let mut buffer = [0; 8192];
                                        loop {
                                            let bytes_read = file.read(&mut buffer).await.unwrap();
                                            total_bytes_read += bytes_read as u64;

                                            if bytes_read == 0 {
                                                break;
                                            }

                                            let bytes_read = if request.length > 0 {
                                                if total_bytes_read > request.length {
                                                    (request.length
                                                        - (total_bytes_read - bytes_read as u64))
                                                        as usize
                                                } else {
                                                    bytes_read
                                                }
                                            } else {
                                                bytes_read
                                            };

                                            hasher.consume(&buffer[..bytes_read]);
                                        }

                                        (*hasher.compute()).into()
                                    }
                                    Some("sha1") => {
                                        let mut hasher = sha1::Sha1::new();

                                        let mut buffer = [0; 8192];
                                        loop {
                                            let bytes_read = file.read(&mut buffer).await.unwrap();
                                            total_bytes_read += bytes_read as u64;

                                            if bytes_read == 0 {
                                                break;
                                            }

                                            let bytes_read = if request.length > 0 {
                                                if total_bytes_read > request.length {
                                                    (request.length
                                                        - (total_bytes_read - bytes_read as u64))
                                                        as usize
                                                } else {
                                                    bytes_read
                                                }
                                            } else {
                                                bytes_read
                                            };

                                            hasher.update(&buffer[..bytes_read]);
                                        }

                                        (*hasher.finalize()).into()
                                    }
                                    Some("sha256") => {
                                        let mut hasher = sha2::Sha256::new();

                                        let mut buffer = [0; 8192];
                                        loop {
                                            let bytes_read = file.read(&mut buffer).await.unwrap();
                                            total_bytes_read += bytes_read as u64;

                                            if bytes_read == 0 {
                                                break;
                                            }

                                            let bytes_read = if request.length > 0 {
                                                if total_bytes_read > request.length {
                                                    (request.length
                                                        - (total_bytes_read - bytes_read as u64))
                                                        as usize
                                                } else {
                                                    bytes_read
                                                }
                                            } else {
                                                bytes_read
                                            };

                                            hasher.update(&buffer[..bytes_read]);
                                        }

                                        (*hasher.finalize()).into()
                                    }
                                    Some("sha512") => {
                                        let mut hasher = sha2::Sha512::new();

                                        let mut buffer = [0; 8192];
                                        loop {
                                            let bytes_read = file.read(&mut buffer).await.unwrap();
                                            total_bytes_read += bytes_read as u64;

                                            if bytes_read == 0 {
                                                break;
                                            }

                                            let bytes_read = if request.length > 0 {
                                                if total_bytes_read > request.length {
                                                    (request.length
                                                        - (total_bytes_read - bytes_read as u64))
                                                        as usize
                                                } else {
                                                    bytes_read
                                                }
                                            } else {
                                                bytes_read
                                            };

                                            hasher.update(&buffer[..bytes_read]);
                                        }

                                        (*hasher.finalize()).into()
                                    }
                                    _ => return Err(StatusCode::BadMessage),
                                };

                                #[derive(Serialize)]
                                struct CheckFileNameReply<'a> {
                                    hash_algorithm: Option<&'a str>,

                                    #[serde(serialize_with = "russh_sftp::ser::data_serialize")]
                                    hash: Vec<u8>,
                                }

                                return Ok(russh_sftp::protocol::Packet::ExtendedReply(
                                    russh_sftp::protocol::ExtendedReply {
                                        id,
                                        data: russh_sftp::ser::to_bytes(&CheckFileNameReply {
                                            hash_algorithm,
                                            hash,
                                        })
                                        .unwrap()
                                        .into(),
                                    },
                                ));
                            }
                        }
                    }
                }

                Err(StatusCode::OpUnsupported)
            }
            "copy-file" => {
                if !self.has_permission(Permission::FileReadContent)
                    || !self.has_permission(Permission::FileCreate)
                {
                    return Err(StatusCode::PermissionDenied);
                }

                #[derive(Deserialize)]
                struct CopyFileRequest {
                    source: String,
                    destination: String,
                    overwrite: u8,
                }

                let request: CopyFileRequest = match russh_sftp::de::from_bytes(&mut data.into()) {
                    Ok(request) => request,
                    Err(_) => return Err(StatusCode::BadMessage),
                };

                if let Some(source_path) = self.server.filesystem.safe_path(&request.source).await {
                    let metadata = match tokio::fs::symlink_metadata(&source_path).await {
                        Ok(metadata) => metadata,
                        Err(_) => return Err(StatusCode::NoSuchFile),
                    };

                    if metadata.is_file() {
                        if self.server.filesystem.is_ignored(&source_path, false).await {
                            return Err(StatusCode::NoSuchFile);
                        }

                        if let Some(destination_path) =
                            self.server.filesystem.safe_path(&request.destination).await
                        {
                            if destination_path.exists() && request.overwrite == 0 {
                                return Err(StatusCode::NoSuchFile);
                            }

                            if !self
                                .server
                                .filesystem
                                .allocate_in_path(
                                    destination_path.parent().unwrap(),
                                    metadata.len() as i64,
                                )
                                .await
                            {
                                return Err(StatusCode::Failure);
                            }

                            tokio::fs::copy(&source_path, &destination_path)
                                .await
                                .map_err(|_| StatusCode::NoSuchFile)?;

                            self.server
                                .activity
                                .log_activity(Activity {
                                    event: ActivityEvent::SftpCreate,
                                    user: self.user_uuid,
                                    ip: self.user_ip,
                                    metadata: Some(json!({
                                        "files": [self.server.filesystem.relative_path(&destination_path)],
                                    })),
                                    timestamp: chrono::Utc::now(),
                                })
                                .await;

                            return Ok(russh_sftp::protocol::Packet::Status(Status {
                                id,
                                status_code: StatusCode::Ok,
                                error_message: "Ok".to_string(),
                                language_tag: "en-US".to_string(),
                            }));
                        }
                    }
                }

                Err(StatusCode::NoSuchFile)
            }
            _ => Err(StatusCode::OpUnsupported),
        }
    }
}
