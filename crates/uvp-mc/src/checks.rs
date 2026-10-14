//! 检查项 schema：kind 是性质的量词结构（bad_state＝永远不出现，
//! admission_closed＝门关死），when 与 predicate 是载荷、intents 指名要
//! 验关断的适格位。探索宇宙（哪些事实参与穷举）由检查原子经适格表达式
//! 传递闭包机械派生，不是用户输入。

use serde::{Deserialize, Serialize};

pub const DEFAULT_MAX_STATES: u64 = 300_000;

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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CheckKind {
    #[serde(rename = "bad_state")]
    BadState,
    #[serde(rename = "admission_closed")]
    AdmissionClosed,
}

impl CheckKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            CheckKind::BadState => "bad_state",
            CheckKind::AdmissionClosed => "admission_closed",
        }
    }
}
