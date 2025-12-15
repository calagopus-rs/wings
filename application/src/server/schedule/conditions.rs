use super::actions::{ScheduleDynamicParameter, ScheduleVariable};
use serde::{Deserialize, Serialize};
use std::pin::Pin;

#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SchedulePreConditionComparator {
    SmallerThan,
    SmallerThanOrEquals,
    Equal,
    GreaterThan,
    GreaterThanOrEquals,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum SchedulePreCondition {
    None,
    And {
        conditions: Vec<SchedulePreCondition>,
    },
    Or {
        conditions: Vec<SchedulePreCondition>,
    },
    Not {
        condition: Box<SchedulePreCondition>,
    },
    ServerState {
        state: crate::server::state::ServerState,
    },
    Uptime {
        comparator: SchedulePreConditionComparator,
        value: u64,
    },
    CpuUsage {
        comparator: SchedulePreConditionComparator,
        value: f64,
    },
    MemoryUsage {
        comparator: SchedulePreConditionComparator,
        value: u64,
    },
    DiskUsage {
        comparator: SchedulePreConditionComparator,
        value: u64,
    },
    FileExists {
        file: compact_str::CompactString,
    },
}

impl SchedulePreCondition {
    pub fn evaluate<'a>(
        &'a self,
        server: &'a crate::server::Server,
    ) -> Pin<Box<dyn Future<Output = bool> + Send + 'a>> {
        Box::pin(async move {
            match self {
                SchedulePreCondition::None => true,
                SchedulePreCondition::And { conditions } => {
                    for condition in conditions {
                        if !condition.evaluate(server).await {
                            return false;
                        }
                    }

                    true
                }
                SchedulePreCondition::Or { conditions } => {
                    for condition in conditions {
                        if condition.evaluate(server).await {
                            return true;
                        }
                    }

                    false
                }
                SchedulePreCondition::Not { condition } => !condition.evaluate(server).await,
                SchedulePreCondition::ServerState { state: cond_state } => {
                    server.state.get_state() == *cond_state
                }
                SchedulePreCondition::Uptime { comparator, value } => {
                    let resource_usage = server.resource_usage().await;

                    match comparator {
                        SchedulePreConditionComparator::SmallerThan => {
                            resource_usage.uptime < *value
                        }
                        SchedulePreConditionComparator::SmallerThanOrEquals => {
                            resource_usage.uptime <= *value
                        }
                        SchedulePreConditionComparator::Equal => resource_usage.uptime == *value,
                        SchedulePreConditionComparator::GreaterThan => {
                            resource_usage.uptime > *value
                        }
                        SchedulePreConditionComparator::GreaterThanOrEquals => {
                            resource_usage.uptime >= *value
                        }
                    }
                }
                SchedulePreCondition::CpuUsage { comparator, value } => {
                    let resource_usage = server.resource_usage().await;

                    match comparator {
                        SchedulePreConditionComparator::SmallerThan => {
                            resource_usage.cpu_absolute < *value
                        }
                        SchedulePreConditionComparator::SmallerThanOrEquals => {
                            resource_usage.cpu_absolute <= *value
                        }
                        SchedulePreConditionComparator::Equal => {
                            resource_usage.cpu_absolute == *value
                        }
                        SchedulePreConditionComparator::GreaterThan => {
                            resource_usage.cpu_absolute > *value
                        }
                        SchedulePreConditionComparator::GreaterThanOrEquals => {
                            resource_usage.cpu_absolute >= *value
                        }
                    }
                }
                SchedulePreCondition::MemoryUsage { comparator, value } => {
                    let resource_usage = server.resource_usage().await;

                    match comparator {
                        SchedulePreConditionComparator::SmallerThan => {
                            resource_usage.memory_bytes < *value
                        }
                        SchedulePreConditionComparator::SmallerThanOrEquals => {
                            resource_usage.memory_bytes <= *value
                        }
                        SchedulePreConditionComparator::Equal => {
                            resource_usage.memory_bytes == *value
                        }
                        SchedulePreConditionComparator::GreaterThan => {
                            resource_usage.memory_bytes > *value
                        }
                        SchedulePreConditionComparator::GreaterThanOrEquals => {
                            resource_usage.memory_bytes >= *value
                        }
                    }
                }
                SchedulePreCondition::DiskUsage { comparator, value } => {
                    let resource_usage = server.resource_usage().await;

                    match comparator {
                        SchedulePreConditionComparator::SmallerThan => {
                            resource_usage.disk_bytes < *value
                        }
                        SchedulePreConditionComparator::SmallerThanOrEquals => {
                            resource_usage.disk_bytes <= *value
                        }
                        SchedulePreConditionComparator::Equal => {
                            resource_usage.disk_bytes == *value
                        }
                        SchedulePreConditionComparator::GreaterThan => {
                            resource_usage.disk_bytes > *value
                        }
                        SchedulePreConditionComparator::GreaterThanOrEquals => {
                            resource_usage.disk_bytes >= *value
                        }
                    }
                }
                SchedulePreCondition::FileExists { file } => {
                    server.filesystem.async_symlink_metadata(file).await.is_ok()
                }
            }
        })
    }
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
    Not {
        condition: Box<ScheduleCondition>,
    },
    VariableExists {
        variable: ScheduleVariable,
    },
    VariableEquals {
        variable: ScheduleVariable,
        equals: ScheduleDynamicParameter,
    },
    VariableContains {
        variable: ScheduleVariable,
        contains: ScheduleDynamicParameter,
    },
    VariableStartsWith {
        variable: ScheduleVariable,
        starts_with: ScheduleDynamicParameter,
    },
    VariableEndsWith {
        variable: ScheduleVariable,
        ends_with: ScheduleDynamicParameter,
    },
}

impl ScheduleCondition {
    pub fn evaluate<'a>(
        &'a self,
        _server: &'a crate::server::Server,
        execution_context: &'a mut super::ScheduleExecutionContext,
    ) -> Pin<Box<dyn Future<Output = bool> + Send + 'a>> {
        Box::pin(async move {
            match self {
                ScheduleCondition::None => true,
                ScheduleCondition::And { conditions } => {
                    for condition in conditions {
                        if !condition.evaluate(_server, execution_context).await {
                            return false;
                        }
                    }

                    true
                }
                ScheduleCondition::Or { conditions } => {
                    for condition in conditions {
                        if condition.evaluate(_server, execution_context).await {
                            return true;
                        }
                    }

                    false
                }
                ScheduleCondition::Not { condition } => {
                    !condition.evaluate(_server, execution_context).await
                }
                ScheduleCondition::VariableExists { variable } => execution_context
                    .get_variable_by_str(&variable.variable)
                    .is_some(),
                ScheduleCondition::VariableEquals { variable, equals } => {
                    let Some(value) = execution_context.get_variable_by_str(&variable.variable)
                    else {
                        return false;
                    };

                    Some(value) == execution_context.resolve_parameter(equals)
                }
                ScheduleCondition::VariableContains { variable, contains } => {
                    let Some(value) = execution_context.get_variable_by_str(&variable.variable)
                    else {
                        return false;
                    };
                    let Some(contains) = execution_context.resolve_parameter(contains) else {
                        return false;
                    };

                    value.contains(&**contains)
                }
                ScheduleCondition::VariableStartsWith {
                    variable,
                    starts_with,
                } => {
                    let Some(value) = execution_context.get_variable_by_str(&variable.variable)
                    else {
                        return false;
                    };
                    let Some(starts_with) = execution_context.resolve_parameter(starts_with) else {
                        return false;
                    };

                    value.starts_with(&**starts_with)
                }
                ScheduleCondition::VariableEndsWith {
                    variable,
                    ends_with,
                } => {
                    let Some(value) = execution_context.get_variable_by_str(&variable.variable)
                    else {
                        return false;
                    };
                    let Some(ends_with) = execution_context.resolve_parameter(ends_with) else {
                        return false;
                    };

                    value.ends_with(&**ends_with)
                }
            }
        })
    }
}
