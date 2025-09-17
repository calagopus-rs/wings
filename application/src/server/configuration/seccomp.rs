use serde::{Deserialize, Serialize};
use std::sync::LazyLock;

const RAW_BASE_SECCOMP: &str = include_str!("../../../seccomp.min.json");
static BASE_SECCOMP: LazyLock<Seccomp> =
    LazyLock::new(|| serde_json::from_str(RAW_BASE_SECCOMP).unwrap());

#[derive(Deserialize, Serialize, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    #[serde(rename = "SCMP_ACT_KILL")]
    Kill,
    #[serde(rename = "SCMP_ACT_KILL_PROCESS")]
    KillProcess,
    #[serde(rename = "SCMP_ACT_KILL_THREAD")]
    KillThread,
    #[serde(rename = "SCMP_ACT_TRAP")]
    Trap,
    #[serde(rename = "SCMP_ACT_ERRNO")]
    Errno,
    #[serde(rename = "SCMP_ACT_TRACE")]
    Trace,
    #[serde(rename = "SCMP_ACT_ALLOW")]
    Allow,
    #[serde(rename = "SCMP_ACT_LOG")]
    Log,
    #[serde(rename = "SCMP_ACT_NOTIFY")]
    Notify,
}

#[derive(Deserialize, Serialize, Clone, Copy, PartialEq, Eq)]
pub enum Operator {
    #[serde(rename = "SCMP_CMP_NE")]
    NotEqual,
    #[serde(rename = "SCMP_CMP_LT")]
    LessThan,
    #[serde(rename = "SCMP_CMP_LE")]
    LessThanEqual,
    #[serde(rename = "SCMP_CMP_EQ")]
    Equal,
    #[serde(rename = "SCMP_CMP_GE")]
    GreaterThan,
    #[serde(rename = "SCMP_CMP_GT")]
    GreaterThanEqual,
    #[serde(rename = "SCMP_CMP_MASKED_EQ")]
    MaskedEqual,
}

#[derive(Deserialize, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Arg {
    index: u32,
    value: u64,
    value_two: u64,
    op: Operator,
}

#[derive(Deserialize, Serialize, Clone)]
pub struct Filter {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    caps: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    arches: Vec<String>,
}

#[derive(Deserialize, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Arch {
    architecture: String,
    #[serde(default)]
    sub_architectures: Vec<String>,
}

#[derive(Deserialize, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Syscall {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    name: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    names: Vec<String>,
    action: Action,
    args: Option<Vec<Arg>>,
    comment: String,
    includes: Filter,
    excludes: Filter,
    #[serde(default)]
    errno_ret: u32,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    errno: String,
}

#[derive(Deserialize, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Seccomp {
    default_action: Action,
    default_errno_ret: u32,
    default_errno: String,
    arch_map: Vec<Arch>,
    syscalls: Vec<Syscall>,
}

impl Default for Seccomp {
    fn default() -> Self {
        BASE_SECCOMP.clone()
    }
}

impl Seccomp {
    pub fn remove_names(&mut self, names: &[String], action: Action) -> &mut Self {
        for syscall in self.syscalls.iter_mut() {
            if syscall.action == action {
                syscall.names.retain(|n| !names.contains(n));
            }
        }

        self
    }

    pub fn to_string(&self) -> Result<String, serde_json::Error> {
        let mut string = Vec::new();
        string.reserve_exact(8 + RAW_BASE_SECCOMP.len());
        string.extend_from_slice(b"seccomp=");
        serde_json::to_writer(&mut string, &self)?;

        Ok(unsafe { String::from_utf8_unchecked(string) })
    }
}
