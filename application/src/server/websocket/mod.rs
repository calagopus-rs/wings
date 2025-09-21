use super::permissions::Permissions;
use axum::extract::ws::{Message, WebSocket};
use futures_util::{SinkExt, stream::SplitSink};
use serde::{
    Deserialize, Deserializer, Serialize,
    de::{SeqAccess, Visitor},
    ser::SerializeSeq,
};
use std::{marker::PhantomData, sync::Arc};
use tokio::sync::Mutex;

pub mod handler;
mod jwt;
mod message_handler;

#[derive(Deserialize)]
pub struct WebsocketJwtPayload {
    #[serde(flatten)]
    pub base: crate::remote::jwt::BasePayload,

    pub user_uuid: uuid::Uuid,
    pub server_uuid: uuid::Uuid,
    pub permissions: Permissions,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
pub enum WebsocketEvent {
    #[serde(rename = "auth success")]
    AuthenticationSuccess,
    #[serde(rename = "token expiring")]
    TokenExpiring,
    #[serde(rename = "token expired")]
    TokenExpired,
    #[serde(rename = "auth")]
    Authentication,
    #[serde(rename = "set state")]
    SetState,
    #[serde(rename = "send logs")]
    SendServerLogs,
    #[serde(rename = "send command")]
    SendCommand,
    #[serde(rename = "send stats")]
    SendStats,
    #[serde(rename = "daemon error")]
    Error,
    #[serde(rename = "jwt error")]
    JwtError,

    #[serde(rename = "stats")]
    ServerStats,
    #[serde(rename = "status")]
    ServerStatus,
    #[serde(rename = "console output")]
    ServerConsoleOutput,
    #[serde(rename = "install output")]
    ServerInstallOutput,
    #[serde(rename = "image pull progress")]
    ServerImagePullProgress,
    #[serde(rename = "image pull completed")]
    ServerImagePullCompleted,
    #[serde(rename = "install started")]
    ServerInstallStarted,
    #[serde(rename = "install completed")]
    ServerInstallCompleted,
    #[serde(rename = "daemon message")]
    ServerDaemonMessage,
    #[serde(rename = "backup progress")]
    ServerBackupProgress,
    #[serde(rename = "backup completed")]
    ServerBackupCompleted,
    #[serde(rename = "backup restore progress")]
    ServerBackupRestoreProgress,
    #[serde(rename = "backup restore completed")]
    ServerBackupRestoreCompleted,
    #[serde(rename = "transfer logs")]
    ServerTransferLogs,
    #[serde(rename = "transfer status")]
    ServerTransferStatus,
    #[serde(rename = "schedule status")]
    ServerScheduleStatus,
    #[serde(rename = "schedule step error")]
    ServerScheduleStepError,
    #[serde(rename = "operation progress")]
    ServerOperationProgress,
    #[serde(rename = "operation completed")]
    ServerOperationCompleted,
    #[serde(rename = "operation error")]
    ServerOperationError,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct WebsocketMessage {
    pub event: WebsocketEvent,

    #[serde(deserialize_with = "string_vec_or_empty")]
    #[serde(serialize_with = "arc_vec")]
    pub args: Arc<Vec<String>>,
}

fn string_vec_or_empty<'de, D>(deserializer: D) -> Result<Arc<Vec<String>>, D::Error>
where
    D: Deserializer<'de>,
{
    struct StringVecVisitor(PhantomData<Vec<String>>);

    impl<'de> Visitor<'de> for StringVecVisitor {
        type Value = Arc<Vec<String>>;

        fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
            formatter.write_str("a string array or null")
        }

        fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
        where
            A: SeqAccess<'de>,
        {
            let mut vec = Vec::new();
            while let Some(element) = seq.next_element::<Option<String>>()? {
                if let Some(value) = element {
                    vec.push(value);
                }
            }
            Ok(Arc::new(vec))
        }

        fn visit_unit<E>(self) -> Result<Self::Value, E>
        where
            E: serde::de::Error,
        {
            Ok(Arc::new(Vec::new()))
        }
    }

    deserializer.deserialize_any(StringVecVisitor(PhantomData))
}

fn arc_vec<S>(vec: &Arc<Vec<String>>, serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    let mut seq = serializer.serialize_seq(Some(vec.len()))?;
    for item in vec.iter() {
        seq.serialize_element(item)?;
    }

    seq.end()
}

impl WebsocketMessage {
    #[inline]
    pub fn new(event: WebsocketEvent, data: &[String]) -> Self {
        Self {
            event,
            args: Arc::new(data.to_vec()),
        }
    }
}

#[inline]
async fn send_message(sender: &Mutex<SplitSink<WebSocket, Message>>, message: WebsocketMessage) {
    let message = serde_json::to_string(&message).unwrap();
    let message = Message::Text(message.into());

    let mut sender = sender.lock().await;
    if let Err(err) = sender.send(message).await {
        tracing::error!("failed to send websocket message: {:#?}", err);
    }
}
