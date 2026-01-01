use serde::{Deserialize, Serialize};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU8, Ordering},
};
use utoipa::ToSchema;

#[derive(ToSchema, Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
#[schema(rename_all = "lowercase")]
#[repr(u8)]
pub enum ServerState {
    #[default]
    Offline,
    Starting,
    Stopping,
    Running,
}

impl ServerState {
    #[inline]
    pub fn to_str(self) -> &'static str {
        match self {
            ServerState::Offline => "offline",
            ServerState::Starting => "starting",
            ServerState::Stopping => "stopping",
            ServerState::Running => "running",
        }
    }
}

pub struct ServerStateLock {
    state: AtomicU8,
    locked: AtomicBool,
    sender: tokio::sync::broadcast::Sender<super::websocket::WebsocketMessage>,
    schedule_manager: Arc<super::schedule::manager::ScheduleManager>,
}

impl ServerStateLock {
    pub fn new(
        sender: tokio::sync::broadcast::Sender<super::websocket::WebsocketMessage>,
        schedule_manager: Arc<super::schedule::manager::ScheduleManager>,
    ) -> Self {
        Self {
            state: AtomicU8::new(0),
            locked: AtomicBool::new(false),
            sender,
            schedule_manager,
        }
    }

    #[inline]
    pub async fn set_state(&self, state: ServerState) {
        if self.get_state() == state {
            return;
        }

        self.state.store(state as u8, Ordering::SeqCst);
        self.schedule_manager
            .execute_server_state_trigger(state)
            .await;

        self.sender
            .send(super::websocket::WebsocketMessage::new(
                super::websocket::WebsocketEvent::ServerStatus,
                [state.to_str().to_string()].into(),
            ))
            .unwrap_or_default();
    }

    #[inline]
    pub fn get_state(&self) -> ServerState {
        match self.state.load(Ordering::SeqCst) {
            0 => ServerState::Offline,
            1 => ServerState::Starting,
            2 => ServerState::Stopping,
            3 => ServerState::Running,
            _ => ServerState::Offline,
        }
    }

    /// Executes an action with the server state locked.
    /// If the action fails, the state is reverted to the previous state.
    /// Returns `Ok(true)` if the action was executed successfully, `Ok(false)` if the lock was not acquired,
    /// and `Err` if an error occurred during the action execution.
    /// If `aquire_timeout` is `Some`, it will wait for the specified duration to acquire the lock.
    /// If the lock is not acquired within the timeout, it returns `Ok(false)`.
    pub async fn execute_action<F, Fut>(
        &self,
        state: ServerState,
        action: F,
        aquire_timeout: Option<std::time::Duration>,
    ) -> Result<bool, anyhow::Error>
    where
        F: FnOnce(bool) -> Fut,
        Fut: Future<Output = Result<(), anyhow::Error>>,
    {
        let old_state = self.get_state();

        let mut aquired = false;
        if let Some(timeout) = aquire_timeout {
            let instant = std::time::Instant::now();
            while instant.elapsed() < timeout {
                if !self.locked.load(Ordering::SeqCst) {
                    aquired = true;
                    break;
                }

                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
        } else if self.locked.load(Ordering::SeqCst) {
            return Ok(false);
        }

        self.locked.store(true, Ordering::SeqCst);

        self.set_state(state).await;
        if let Err(err) = action(aquired).await {
            tracing::error!("failed to execute power action: {:?}", err);

            self.set_state(old_state).await;
            self.locked.store(false, Ordering::SeqCst);

            Err(err)
        } else {
            self.locked.store(false, Ordering::SeqCst);

            Ok(true)
        }
    }
}
