use serde::{
    Deserialize, Deserializer, Serialize,
    de::{SeqAccess, Visitor},
};
use std::{
    collections::{HashMap, HashSet},
    marker::PhantomData,
    ops::{Deref, DerefMut},
    sync::Arc,
};
use tokio::sync::RwLock;

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq, Hash)]
pub enum Permission {
    #[serde(rename = "*")]
    All,

    #[serde(rename = "websocket.connect")]
    WebsocketConnect,
    #[serde(rename = "control.console")]
    ControlConsole,
    #[serde(rename = "control.start")]
    ControlStart,
    #[serde(rename = "control.stop")]
    ControlStop,
    #[serde(rename = "control.restart")]
    ControlRestart,
    #[serde(rename = "admin.websocket.errors")]
    AdminWebsocketErrors,
    #[serde(rename = "admin.websocket.install")]
    AdminWebsocketInstall,
    #[serde(rename = "admin.websocket.transfer")]
    AdminWebsocketTransfer,
    #[serde(rename = "backup.read", alias = "backups.read")]
    BackupRead,
    #[serde(rename = "schedule.read", alias = "schedules.read")]
    ScheduleRead,

    #[serde(rename = "file.read", alias = "files.read")]
    FileRead,
    #[serde(rename = "file.read-content", alias = "files.read-content")]
    FileReadContent,
    #[serde(rename = "file.create", alias = "files.create")]
    FileCreate,
    #[serde(rename = "file.update", alias = "files.update")]
    FileUpdate,
    #[serde(rename = "file.delete", alias = "files.delete")]
    FileDelete,
    #[serde(rename = "file.archive", alias = "files.archive")]
    FileArchive,
}

impl Permission {
    pub fn is_admin(self) -> bool {
        matches!(
            self,
            Permission::AdminWebsocketErrors
                | Permission::AdminWebsocketInstall
                | Permission::AdminWebsocketTransfer
        )
    }

    pub fn matches(self, other: Permission) -> bool {
        self == other || (other == Permission::All && !other.is_admin())
    }
}

type UserPermissions = (
    Permissions,
    Option<ignore::overrides::Override>,
    std::time::Instant,
);
pub struct UserPermissionsMap {
    map: Arc<RwLock<HashMap<uuid::Uuid, UserPermissions>>>,
    task: tokio::task::JoinHandle<()>,
}

impl Default for UserPermissionsMap {
    fn default() -> Self {
        let map = Arc::new(RwLock::new(HashMap::new()));

        Self {
            map: Arc::clone(&map),
            task: tokio::spawn(async move {
                loop {
                    tokio::time::sleep(std::time::Duration::from_secs(60)).await;

                    let mut map = map.write().await;
                    map.retain(|_, (_, _, last_access)| {
                        last_access.elapsed().as_secs() < 60 * 60 * 24
                    });
                }
            }),
        }
    }
}

impl UserPermissionsMap {
    pub async fn has_permission(&self, user_uuid: uuid::Uuid, permission: Permission) -> bool {
        let mut map = self.map.write().await;
        if let Some((permissions, _, last_access)) = map.get_mut(&user_uuid) {
            *last_access = std::time::Instant::now();

            permissions.has_permission(permission)
        } else {
            false
        }
    }

    pub async fn is_ignored(
        &self,
        user_uuid: uuid::Uuid,
        path: impl AsRef<std::path::Path>,
        is_dir: bool,
    ) -> bool {
        let mut map = self.map.write().await;
        if let Some((_, ignored, last_access)) = map.get_mut(&user_uuid) {
            *last_access = std::time::Instant::now();

            ignored
                .as_ref()
                .map(|ig| ig.matched(path, is_dir).is_whitelist())
                .unwrap_or(false)
        } else {
            false
        }
    }

    pub async fn set_permissions(
        &self,
        user_uuid: uuid::Uuid,
        permissions: Permissions,
        ignored_files: &[String],
    ) {
        let mut overrides = ignore::overrides::OverrideBuilder::new("/");
        for file in ignored_files {
            overrides.add(file).ok();
        }

        self.map.write().await.insert(
            user_uuid,
            (
                permissions,
                overrides.build().ok(),
                std::time::Instant::now(),
            ),
        );
    }

    pub async fn is_contained(&self, user_id: uuid::Uuid) -> bool {
        self.map.read().await.contains_key(&user_id)
    }
}

impl Drop for UserPermissionsMap {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct Permissions(HashSet<Permission>);

impl Permissions {
    #[inline]
    pub fn has_permission(&self, permission: Permission) -> bool {
        for p in self.0.iter().copied() {
            if permission.matches(p) {
                return true;
            }
        }

        false
    }
}

impl Deref for Permissions {
    type Target = HashSet<Permission>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for Permissions {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl<'de> Deserialize<'de> for Permissions {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct PermissionsVisitor(PhantomData<fn() -> Permissions>);

        impl<'de> Visitor<'de> for PermissionsVisitor {
            type Value = Permissions;

            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("a sequence of permissions")
            }

            fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
            where
                A: SeqAccess<'de>,
            {
                let mut permissions = HashSet::new();

                while let Ok(Some(result)) = seq.next_element::<serde_json::Value>() {
                    if let Ok(permission) = serde_json::from_value::<Permission>(result) {
                        permissions.insert(permission);
                    }
                }

                Ok(Permissions(permissions))
            }
        }

        deserializer.deserialize_seq(PermissionsVisitor(PhantomData))
    }
}
