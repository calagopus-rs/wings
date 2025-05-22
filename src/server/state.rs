use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, AtomicU8};
use utoipa::ToSchema;

#[derive(ToSchema, Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "lowercase")]
#[schema(rename_all = "lowercase")]
pub enum ServerState {
    #[default]
    Offline,
    Starting,
    Stopping,
    Running,
}

impl From<u8> for ServerState {
    fn from(value: u8) -> Self {
        match value {
            0 => ServerState::Offline,
            1 => ServerState::Starting,
            2 => ServerState::Stopping,
            3 => ServerState::Running,
            _ => panic!("Invalid server state"),
        }
    }
}

impl From<ServerState> for u8 {
    fn from(value: ServerState) -> Self {
        match value {
            ServerState::Offline => 0,
            ServerState::Starting => 1,
            ServerState::Stopping => 2,
            ServerState::Running => 3,
        }
    }
}

pub struct ServerStateLock {
    state: AtomicU8,
    locked: AtomicBool,
    sender: tokio::sync::broadcast::Sender<super::websocket::WebsocketMessage>,
}

impl ServerStateLock {
    pub fn new(sender: tokio::sync::broadcast::Sender<super::websocket::WebsocketMessage>) -> Self {
        Self {
            state: AtomicU8::new(0),
            locked: AtomicBool::new(false),
            sender,
        }
    }

    pub fn set_state(&self, state: ServerState) {
        if self.get_state() == state {
            return;
        }

        self.state
            .store(state.into(), std::sync::atomic::Ordering::SeqCst);

        let state_str = serde_json::to_value(state).unwrap();
        let state_str = state_str.as_str().unwrap();

        self.sender
            .send(super::websocket::WebsocketMessage::new(
                super::websocket::WebsocketEvent::ServerStatus,
                &[state_str.to_string()],
            ))
            .unwrap_or_default();
    }

    pub fn get_state(&self) -> ServerState {
        ServerState::from(self.state.load(std::sync::atomic::Ordering::SeqCst))
    }

    pub async fn execute_action<F, Fut>(
        &self,
        state: ServerState,
        action: F,
        aquire_timeout: Option<std::time::Duration>,
    ) -> bool
    where
        F: FnOnce(bool) -> Fut,
        Fut: Future<Output = Result<(), anyhow::Error>>,
    {
        let old_state = self.get_state();

        let mut aquired = false;
        if let Some(timeout) = aquire_timeout {
            let instant = std::time::Instant::now();
            while instant.elapsed() < timeout {
                if !self.locked.load(std::sync::atomic::Ordering::SeqCst) {
                    aquired = true;
                    break;
                }

                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
        } else if self.locked.load(std::sync::atomic::Ordering::SeqCst) {
            return false;
        }

        self.locked.store(true, std::sync::atomic::Ordering::SeqCst);

        self.set_state(state);
        if let Err(err) = action(aquired).await {
            tracing::error!("failed to execute power action: {}", err);

            self.set_state(old_state);
            self.locked
                .store(false, std::sync::atomic::Ordering::SeqCst);

            false
        } else {
            self.locked
                .store(false, std::sync::atomic::Ordering::SeqCst);

            true
        }
    }
}
