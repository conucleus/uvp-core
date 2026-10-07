//! manifest：不变量与效果门控的数据面（uvp.mc.manifest.v1）。内核业务盲，
//! 一切业务因果（回执前置、终态模式、检查谓词）都以本结构喂入；TOML 与
//! JSON 同构（CLI 食 TOML 文件，FFI/进程调用走 JSON）。

use serde::{Deserialize, Serialize};

pub const MANIFEST_SCHEMA_VERSION: &str = "uvp.mc.manifest.v1";
pub const DEFAULT_MAX_STATES: u64 = 300_000;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub struct Manifest {
    pub schema_version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_states: Option<u64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub effects: Vec<Effect>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub checks: Vec<Check>,
}

impl Manifest {
    pub fn max_states(&self) -> u64 {
        self.max_states.unwrap_or(DEFAULT_MAX_STATES)
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub struct Effect {
    pub fact: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requires_any_guards: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requires_all_facts: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requires_any_facts: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unless_any_facts: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub struct Check {
    pub id: String,
    pub kind: CheckKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub predicate: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub intents: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scope: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CheckKind {
    #[serde(rename = "bad_state")]
    BadState,
    #[serde(rename = "admission_closed")]
    AdmissionClosed,
    #[serde(rename = "deadlock_free")]
    DeadlockFree,
    #[serde(rename = "coreach")]
    CoReach,
    #[serde(rename = "time_only_closure")]
    TimeOnlyClosure,
}

impl CheckKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            CheckKind::BadState => "bad_state",
            CheckKind::AdmissionClosed => "admission_closed",
            CheckKind::DeadlockFree => "deadlock_free",
            CheckKind::CoReach => "coreach",
            CheckKind::TimeOnlyClosure => "time_only_closure",
        }
    }
}
