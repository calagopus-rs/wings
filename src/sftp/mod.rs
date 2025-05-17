use crate::routes::State;
use crate::server::activity::{Activity, ActivityEvent};
use crate::server::permissions::{Permission, Permissions};
use russh_sftp::protocol::{
    Data, File, FileAttributes, Handle, Name, OpenFlags, Status, StatusCode,
};
use serde_json::json;
use std::collections::HashMap;
use std::fs::{Metadata, OpenOptions};
use std::net::{IpAddr, SocketAddr};
use std::os::unix::fs::{FileExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::Mutex;

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

    consumed: u64,
    size: u64,
}

struct SftpSession {
    state: State,
    server: Arc<crate::server::Server>,

    user_ip: Option<IpAddr>,
    user_uuid: Option<uuid::Uuid>,
    user_permissions: Permissions,

    handle_id: u64,
    handles: HashMap<String, FileHandle>,
}

impl SftpSession {
    #[inline]
    async fn convert_entry(path: &Path, metadata: Metadata) -> File {
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
        if let Some(Ok(path)) = self.server.filesystem.safe_path(&path).map(|p| {
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

        if let Some(path) = self.server.filesystem.safe_path(&path) {
            if !path.is_dir() {
                return Err(StatusCode::NoSuchFile);
            }

            if self.server.filesystem.is_ignored(&path, false) {
                return Err(StatusCode::NoSuchFile);
            }

            let path_components = self.server.filesystem.path_to_components(&path);

            self.handles.insert(
                handle.clone(),
                FileHandle {
                    path,
                    path_components,
                    file: None,
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

        if handle.consumed > 0 {
            return Err(StatusCode::Eof);
        }

        let mut files = Vec::new();

        let mut read_dir = tokio::fs::read_dir(&handle.path).await.unwrap();
        while let Ok(Some(file)) = read_dir.next_entry().await {
            let path = file.path();
            let metadata = match tokio::fs::symlink_metadata(&path).await {
                Ok(metadata) => metadata,
                Err(_) => continue,
            };

            if self.server.filesystem.is_ignored(&path, metadata.is_dir()) {
                continue;
            }

            files.push(Self::convert_entry(&path, metadata).await);
        }

        handle.consumed = 1;

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

        if let Some(path) = self.server.filesystem.safe_path(&filename) {
            let parent = match path.parent() {
                Some(parent) => parent,
                None => return Err(StatusCode::NoSuchFile),
            };

            if let Ok(metadata) = tokio::fs::symlink_metadata(&path).await {
                if !metadata.is_file() {
                    return Err(StatusCode::NoSuchFile);
                }

                if self.server.filesystem.is_ignored(&path, metadata.is_dir()) {
                    return Err(StatusCode::NoSuchFile);
                }

                if tokio::fs::remove_file(&path).await.is_err() {
                    return Err(StatusCode::NoSuchFile);
                }

                self.server
                    .filesystem
                    .allocate_in_path(parent, -(metadata.len() as i64));
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

        if let Some(path) = self.server.filesystem.safe_path(&path) {
            if !path.is_dir() {
                return Err(StatusCode::NoSuchFile);
            }

            if self.server.filesystem.is_ignored(&path, true) {
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

        if let Some(path) = self.server.filesystem.safe_path(&path) {
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

        if let Some(old_path) = self.server.filesystem.safe_path(&old_path) {
            let old_metadata = match tokio::fs::symlink_metadata(&old_path).await {
                Ok(metadata) => metadata,
                Err(_) => return Err(StatusCode::NoSuchFile),
            };

            if let Some(new_path) = self.server.filesystem.safe_path(&new_path) {
                if new_path.exists()
                    || self
                        .server
                        .filesystem
                        .is_ignored(&old_path, old_metadata.is_dir())
                    || self
                        .server
                        .filesystem
                        .is_ignored(&new_path, old_metadata.is_dir())
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

        if let Some(path) = self.server.filesystem.safe_path(&path) {
            let metadata = match tokio::fs::symlink_metadata(&path).await {
                Ok(metadata) => metadata,
                Err(_) => return Err(StatusCode::NoSuchFile),
            };

            if self.server.filesystem.is_ignored(&path, metadata.is_dir()) {
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

        if let Some(path) = self.server.filesystem.safe_path(&path) {
            let metadata = match tokio::fs::symlink_metadata(&path).await {
                Ok(metadata) => metadata,
                Err(_) => return Err(StatusCode::NoSuchFile),
            };

            if self.server.filesystem.is_ignored(&path, metadata.is_dir()) {
                return Err(StatusCode::NoSuchFile);
            }

            let file = Self::convert_entry(&path, metadata).await;

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
        self.stat(id, path).await
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

        if let Some(path) = self.server.filesystem.safe_path(&filename) {
            if path.exists() && !path.is_file() {
                return Err(StatusCode::NoSuchFile);
            }

            if self.server.filesystem.is_ignored(&path, false) {
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
                let mut buf = vec![0; len as usize];
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

        let success = tokio::task::spawn_blocking({
            let file = Arc::clone(file);
            let filesystem = Arc::clone(&self.server.filesystem);
            let components = handle.path_components[0..handle.path_components.len() - 1].to_vec();

            move || {
                if !filesystem.allocate_in_path_raw(&components, data.len() as i64) {
                    return false;
                }

                file.write_all_at(&data, offset).unwrap();
                true
            }
        })
        .await
        .unwrap();

        if !success {
            return Err(StatusCode::Failure);
        }

        Ok(Status {
            id,
            status_code: StatusCode::Ok,
            error_message: "Ok".to_string(),
            language_tag: "en-US".to_string(),
        })
    }
}
