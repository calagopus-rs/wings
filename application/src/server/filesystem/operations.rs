use serde::Serialize;
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, atomic::AtomicU64},
};
use tokio::sync::{RwLock, RwLockReadGuard};

fn serialize_arc<S>(value: &Arc<AtomicU64>, serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    serializer.serialize_u64(value.load(std::sync::atomic::Ordering::Relaxed))
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum FilesystemOperation {
    Compress {
        path: PathBuf,

        #[serde(serialize_with = "serialize_arc")]
        progress: Arc<AtomicU64>,
        #[serde(serialize_with = "serialize_arc")]
        total: Arc<AtomicU64>,
    },
    Decompress {
        path: PathBuf,
        destination: PathBuf,

        #[serde(serialize_with = "serialize_arc")]
        progress: Arc<AtomicU64>,
        #[serde(serialize_with = "serialize_arc")]
        total: Arc<AtomicU64>,
    },
    Pull {
        path: PathBuf,

        #[serde(serialize_with = "serialize_arc")]
        progress: Arc<AtomicU64>,
        #[serde(serialize_with = "serialize_arc")]
        total: Arc<AtomicU64>,
    },
}

type Operation = (FilesystemOperation, tokio::sync::oneshot::Sender<()>);
pub struct OperationManager {
    operations: Arc<RwLock<HashMap<uuid::Uuid, Operation>>>,
    sender: tokio::sync::broadcast::Sender<crate::server::websocket::WebsocketMessage>,
}

impl OperationManager {
    pub fn new(
        sender: tokio::sync::broadcast::Sender<crate::server::websocket::WebsocketMessage>,
    ) -> Self {
        Self {
            operations: Arc::new(RwLock::new(HashMap::new())),
            sender,
        }
    }

    pub async fn operations(&self) -> RwLockReadGuard<'_, HashMap<uuid::Uuid, Operation>> {
        self.operations.read().await
    }

    pub async fn add_operation<
        T: Send + 'static,
        F: Future<Output = Result<T, anyhow::Error>> + Send + 'static,
    >(
        &self,
        operation: FilesystemOperation,
        f: F,
    ) -> (
        uuid::Uuid,
        tokio::task::JoinHandle<Option<Result<T, anyhow::Error>>>,
    ) {
        let operation_uuid = uuid::Uuid::new_v4();
        let (abort_sender, abort_receiver) = tokio::sync::oneshot::channel();

        let handle = tokio::spawn({
            let operation = operation.clone();
            let operations = self.operations.clone();
            let sender = self.sender.clone();

            async move {
                let progress_task = async {
                    loop {
                        sender
                            .send(crate::server::websocket::WebsocketMessage::new(
                                crate::server::websocket::WebsocketEvent::ServerOperationProgress,
                                &[
                                    operation_uuid.to_string(),
                                    serde_json::to_string(&operation).unwrap(),
                                ],
                            ))
                            .ok();

                        tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                    }
                };

                let result = tokio::select! {
                    result = f => Some(result),
                    _ = progress_task => None,
                    _ = abort_receiver => None,
                };

                operations.write().await.remove(&operation_uuid);
                if let Some(Err(err)) = result.as_ref() {
                    let message = if let Some(err) = err.downcast_ref::<&str>() {
                        err.to_string()
                    } else if let Some(err) = err.downcast_ref::<String>() {
                        err.to_string()
                    } else if let Some(err) = err.downcast_ref::<std::io::Error>() {
                        err.to_string()
                    } else if let Some(err) = err.downcast_ref::<zip::result::ZipError>() {
                        match err {
                            zip::result::ZipError::Io(err) => err.to_string(),
                            _ => err.to_string(),
                        }
                    } else if let Some(err) = err.downcast_ref::<sevenz_rust2::Error>() {
                        match err {
                            sevenz_rust2::Error::Io(err, _) => err.to_string(),
                            _ => err.to_string(),
                        }
                    } else {
                        tracing::error!(
                            operation = ?operation_uuid,
                            "unknown operation error: {:#?}",
                            err
                        );

                        String::from("unknown error")
                    };

                    sender
                        .send(crate::server::websocket::WebsocketMessage::new(
                            crate::server::websocket::WebsocketEvent::ServerOperationError,
                            &[operation_uuid.to_string(), message],
                        ))
                        .ok();
                } else {
                    sender
                        .send(crate::server::websocket::WebsocketMessage::new(
                            crate::server::websocket::WebsocketEvent::ServerOperationCompleted,
                            &[operation_uuid.to_string()],
                        ))
                        .ok();
                }

                result
            }
        });

        self.operations
            .write()
            .await
            .insert(operation_uuid, (operation, abort_sender));

        (operation_uuid, handle)
    }

    pub async fn abort_operation(&self, operation_uuid: uuid::Uuid) -> bool {
        if let Some((_, abort_sender)) = self.operations.write().await.remove(&operation_uuid) {
            abort_sender.send(()).ok();
            return true;
        }

        false
    }
}
