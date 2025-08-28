use crate::server::websocket::{WebsocketEvent, WebsocketMessage};
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, pin::Pin, str::FromStr, sync::Arc};
use tokio::sync::{Mutex, RwLock};
use utoipa::ToSchema;

pub mod actions;
pub mod manager;

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum ScheduleTrigger {
    Cron {
        schedule: Box<cron::Schedule>,
    },
    PowerAction {
        action: crate::models::ServerPowerAction,
    },
    ServerState {
        state: crate::server::state::ServerState,
    },
    Crash,
}

impl PartialEq for ScheduleTrigger {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (ScheduleTrigger::Cron { schedule: s1 }, ScheduleTrigger::Cron { schedule: s2 }) => {
                s1.source() == s2.source()
            }
            (
                ScheduleTrigger::PowerAction { action: a1 },
                ScheduleTrigger::PowerAction { action: a2 },
            ) => a1 == a2,
            (
                ScheduleTrigger::ServerState { state: s1 },
                ScheduleTrigger::ServerState { state: s2 },
            ) => s1 == s2,
            (ScheduleTrigger::Crash, ScheduleTrigger::Crash) => true,
            _ => false,
        }
    }
}

#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ScheduleComparator {
    SmallerThan,
    SmallerThanOrEquals,
    Equal,
    GreaterThan,
    GreaterThanOrEquals,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum ScheduleCondition {
    None,
    And {
        conditions: Vec<ScheduleCondition>,
    },
    Or {
        conditions: Vec<ScheduleCondition>,
    },
    ServerState {
        state: crate::server::state::ServerState,
    },
    Uptime {
        comparator: ScheduleComparator,
        value: u64,
    },
    CpuUsage {
        comparator: ScheduleComparator,
        value: f64,
    },
    MemoryUsage {
        comparator: ScheduleComparator,
        value: u64,
    },
    DiskUsage {
        comparator: ScheduleComparator,
        value: u64,
    },
    FileExists {
        file: String,
    },
}

impl ScheduleCondition {
    pub fn evaluate<'a>(
        &'a self,
        server: &'a crate::server::Server,
    ) -> Pin<Box<dyn Future<Output = bool> + Send + 'a>> {
        Box::pin(async move {
            match self {
                ScheduleCondition::None => true,
                ScheduleCondition::And { conditions } => {
                    for condition in conditions {
                        if !condition.evaluate(server).await {
                            return false;
                        }
                    }

                    true
                }
                ScheduleCondition::Or { conditions } => {
                    for condition in conditions {
                        if condition.evaluate(server).await {
                            return true;
                        }
                    }

                    false
                }
                ScheduleCondition::ServerState { state: cond_state } => {
                    server.state.get_state() == *cond_state
                }
                ScheduleCondition::Uptime { comparator, value } => {
                    let resource_usage = server.resource_usage().await;

                    match comparator {
                        ScheduleComparator::SmallerThan => resource_usage.uptime < *value,
                        ScheduleComparator::SmallerThanOrEquals => resource_usage.uptime <= *value,
                        ScheduleComparator::Equal => resource_usage.uptime == *value,
                        ScheduleComparator::GreaterThan => resource_usage.uptime > *value,
                        ScheduleComparator::GreaterThanOrEquals => resource_usage.uptime >= *value,
                    }
                }
                ScheduleCondition::CpuUsage { comparator, value } => {
                    let resource_usage = server.resource_usage().await;

                    match comparator {
                        ScheduleComparator::SmallerThan => resource_usage.cpu_absolute < *value,
                        ScheduleComparator::SmallerThanOrEquals => {
                            resource_usage.cpu_absolute <= *value
                        }
                        ScheduleComparator::Equal => resource_usage.cpu_absolute == *value,
                        ScheduleComparator::GreaterThan => resource_usage.cpu_absolute > *value,
                        ScheduleComparator::GreaterThanOrEquals => {
                            resource_usage.cpu_absolute >= *value
                        }
                    }
                }
                ScheduleCondition::MemoryUsage { comparator, value } => {
                    let resource_usage = server.resource_usage().await;

                    match comparator {
                        ScheduleComparator::SmallerThan => resource_usage.memory_bytes < *value,
                        ScheduleComparator::SmallerThanOrEquals => {
                            resource_usage.memory_bytes <= *value
                        }
                        ScheduleComparator::Equal => resource_usage.memory_bytes == *value,
                        ScheduleComparator::GreaterThan => resource_usage.memory_bytes > *value,
                        ScheduleComparator::GreaterThanOrEquals => {
                            resource_usage.memory_bytes >= *value
                        }
                    }
                }
                ScheduleCondition::DiskUsage { comparator, value } => {
                    let resource_usage = server.resource_usage().await;

                    match comparator {
                        ScheduleComparator::SmallerThan => resource_usage.disk_bytes < *value,
                        ScheduleComparator::SmallerThanOrEquals => {
                            resource_usage.disk_bytes <= *value
                        }
                        ScheduleComparator::Equal => resource_usage.disk_bytes == *value,
                        ScheduleComparator::GreaterThan => resource_usage.disk_bytes > *value,
                        ScheduleComparator::GreaterThanOrEquals => {
                            resource_usage.disk_bytes >= *value
                        }
                    }
                }
                ScheduleCondition::FileExists { file } => {
                    server.filesystem.async_symlink_metadata(file).await.is_ok()
                }
            }
        })
    }
}

#[derive(Debug, ToSchema, Serialize)]
pub struct ApiScheduleCompletionStatus {
    pub uuid: uuid::Uuid,
    pub successful: bool,
    pub errors: HashMap<uuid::Uuid, String>,
    pub timestamp: chrono::DateTime<chrono::Utc>,
}

#[derive(ToSchema, Serialize)]
pub struct ScheduleStatus {
    pub running: bool,
    pub step: Option<uuid::Uuid>,
}

pub struct Schedule {
    pub uuid: uuid::Uuid,
    pub triggers: Vec<ScheduleTrigger>,
    pub condition: Arc<RwLock<ScheduleCondition>>,
    pub raw_actions: Arc<RwLock<Arc<Vec<super::configuration::ScheduleAction>>>>,
    pub status: Arc<RwLock<ScheduleStatus>>,
    pub completion_status: Arc<Mutex<Option<ApiScheduleCompletionStatus>>>,

    trigger_tasks: Vec<tokio::task::JoinHandle<()>>,

    executor_task: tokio::task::JoinHandle<()>,
    executor_notifier: Arc<tokio::sync::Notify>,
    executor_skip_notifier: Arc<tokio::sync::Notify>,
}

impl Schedule {
    pub fn new(
        server: crate::server::Server,
        raw_schedule: super::configuration::Schedule,
    ) -> Self {
        let executor_notifier = Arc::new(tokio::sync::Notify::new());
        let executor_skip_notifier = Arc::new(tokio::sync::Notify::new());

        let condition = Arc::new(RwLock::new(raw_schedule.condition));
        let raw_actions = Arc::new(RwLock::new(Arc::new(raw_schedule.actions)));
        let status = Arc::new(RwLock::new(ScheduleStatus {
            running: false,
            step: None,
        }));
        let completion_status = Arc::new(Mutex::new(None));

        let (triggers, trigger_tasks) = Self::create_trigger_tasks(
            server.clone(),
            raw_schedule.triggers,
            Arc::clone(&executor_notifier),
        );

        Self {
            uuid: raw_schedule.uuid,
            triggers,
            condition: Arc::clone(&condition),
            raw_actions: Arc::clone(&raw_actions),
            status: Arc::clone(&status),
            completion_status: Arc::clone(&completion_status),
            trigger_tasks,
            executor_task: Self::create_executor_task(
                server,
                raw_schedule.uuid,
                condition,
                raw_actions,
                Arc::clone(&executor_notifier),
                Arc::clone(&executor_skip_notifier),
                status,
                completion_status,
            ),
            executor_notifier,
            executor_skip_notifier,
        }
    }

    #[inline]
    pub fn trigger(&self, skip_condition: bool) {
        if skip_condition {
            self.executor_skip_notifier.notify_one();
        } else {
            self.executor_notifier.notify_one();
        }
    }

    pub async fn update(&self, raw_schedule: &super::configuration::Schedule) {
        *self.condition.write().await = raw_schedule.condition.clone();
        *self.raw_actions.write().await = Arc::new(raw_schedule.actions.clone());
    }

    pub fn recreate_triggers(
        &mut self,
        server: crate::server::Server,
        triggers: Vec<ScheduleTrigger>,
    ) {
        tracing::debug!(schedule = %self.uuid, "recreating triggers");

        for task in self.trigger_tasks.drain(..) {
            task.abort();
        }

        let (triggers, tasks) =
            Self::create_trigger_tasks(server, triggers, Arc::clone(&self.executor_notifier));

        self.triggers = triggers;
        self.trigger_tasks = tasks;
    }

    pub async fn recreate_executor(&mut self, server: crate::server::Server) {
        tracing::debug!(server = %server.uuid, schedule = %self.uuid, "recreating executor task");

        self.executor_task.abort();
        self.executor_task = Self::create_executor_task(
            server,
            self.uuid,
            Arc::clone(&self.condition),
            Arc::clone(&self.raw_actions),
            Arc::clone(&self.executor_notifier),
            Arc::clone(&self.executor_skip_notifier),
            Arc::clone(&self.status),
            Arc::clone(&self.completion_status),
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn create_executor_task(
        server: crate::server::Server,
        uuid: uuid::Uuid,
        condition: Arc<RwLock<ScheduleCondition>>,
        raw_actions: Arc<RwLock<Arc<Vec<super::configuration::ScheduleAction>>>>,
        executor_notifier: Arc<tokio::sync::Notify>,
        executor_skip_notifier: Arc<tokio::sync::Notify>,
        status: Arc<RwLock<ScheduleStatus>>,
        completion_status: Arc<Mutex<Option<ApiScheduleCompletionStatus>>>,
    ) -> tokio::task::JoinHandle<()> {
        tracing::debug!(server = %server.uuid, schedule = %uuid, "creating executor task");

        tokio::task::spawn(async move {
            loop {
                let skip_condition = tokio::select! {
                    _ = executor_skip_notifier.notified() => true,
                    _ = executor_notifier.notified() => false,
                };

                if !skip_condition && !condition.read().await.evaluate(&server).await {
                    continue;
                }

                tracing::debug!(server = %server.uuid, schedule = %uuid, skip_condition, "schedule condition met, executing actions");

                let raw_actions_lock = raw_actions.read().await;
                let raw_actions = Arc::clone(&*raw_actions_lock);
                drop(raw_actions_lock);

                let mut errors = HashMap::new();
                let mut successful = true;

                for raw_action in raw_actions.iter() {
                    *status.write().await = ScheduleStatus {
                        running: true,
                        step: Some(raw_action.uuid),
                    };
                    server
                        .websocket
                        .send(WebsocketMessage::new(
                            WebsocketEvent::ServerScheduleStatus,
                            &[
                                uuid.to_string(),
                                serde_json::to_string(&*status.read().await).unwrap(),
                            ],
                        ))
                        .ok();

                    match raw_action.action.execute(&server.app_state, &server).await {
                        Ok(()) => {}
                        Err(err) => {
                            errors.insert(raw_action.uuid, err.clone());
                            server
                                .websocket
                                .send(WebsocketMessage::new(
                                    WebsocketEvent::ServerScheduleStepError,
                                    &[raw_action.uuid.to_string(), err],
                                ))
                                .ok();

                            if !raw_action.action.ignore_failure() {
                                successful = false;
                                break;
                            }
                        }
                    }
                }

                tracing::debug!(server = %server.uuid, schedule = %uuid, errors = ?errors, "schedule actions executed");

                *status.write().await = ScheduleStatus {
                    running: false,
                    step: None,
                };
                server
                    .websocket
                    .send(WebsocketMessage::new(
                        WebsocketEvent::ServerScheduleStatus,
                        &[
                            uuid.to_string(),
                            serde_json::to_string(&*status.read().await).unwrap(),
                        ],
                    ))
                    .ok();

                *completion_status.lock().await = Some(ApiScheduleCompletionStatus {
                    uuid,
                    successful,
                    errors,
                    timestamp: chrono::Utc::now(),
                });
            }
        })
    }

    fn create_trigger_tasks(
        server: crate::server::Server,
        raw_triggers: Vec<ScheduleTrigger>,
        executor_notifier: Arc<tokio::sync::Notify>,
    ) -> (Vec<ScheduleTrigger>, Vec<tokio::task::JoinHandle<()>>) {
        let cron_count = raw_triggers
            .iter()
            .filter(|t| matches!(t, ScheduleTrigger::Cron { .. }))
            .count();
        let mut triggers = Vec::new();
        triggers.reserve_exact(raw_triggers.len() - cron_count);
        let mut tasks = Vec::new();
        tasks.reserve_exact(cron_count);

        for trigger in raw_triggers {
            match trigger {
                ScheduleTrigger::Cron { schedule } => {
                    tasks.push(tokio::task::spawn({
                        let executor_notifier = Arc::clone(&executor_notifier);
                        let server = server.clone();

                        async move {
                            loop {
                                let timezone_lock = server.configuration.read().await;
                                let timezone = timezone_lock
                                    .container
                                    .timezone
                                    .as_ref()
                                    .unwrap_or(&server.app_state.config.system.timezone);
                                let timezone =
                                    chrono_tz::Tz::from_str(timezone).unwrap_or(chrono_tz::UTC);
                                drop(timezone_lock);

                                let now_datetime = chrono::Utc::now().with_timezone(&timezone);
                                let target_datetime = match schedule.after(&now_datetime).next() {
                                    Some(dt) => dt,
                                    None => break,
                                };

                                let target_timestamp = target_datetime.timestamp();
                                let now_timestamp = now_datetime.timestamp();
                                let sleep_duration = target_timestamp - now_timestamp;
                                if sleep_duration <= 0 {
                                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                                    continue;
                                }

                                tokio::time::sleep(std::time::Duration::from_secs(
                                    sleep_duration as u64,
                                ))
                                .await;
                                executor_notifier.notify_one();
                            }
                        }
                    }));
                }
                _ => triggers.push(trigger),
            }
        }

        (triggers, tasks)
    }
}

impl Drop for Schedule {
    fn drop(&mut self) {
        for task in self.trigger_tasks.drain(..) {
            task.abort();
        }
        self.executor_task.abort();
    }
}
