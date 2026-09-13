//! 观察快照：链上事件的期望观察投影与 expected/observed 配对比对。

use chrono::{DateTime, Utc};
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet};

use crate::facts::OracleOrderState;
use crate::{field_eq, value_str, ReplayError, Result};

pub(crate) fn chain_event_to_expected_observation(event: &Value) -> Result<Value> {
    match value_str(event, "eventName")? {
        "HookReady" => Ok(json!({
            "eventName": "HookReady",
            "planId": value_str(event, "planId")?,
            "zhixuId": value_str(event, "zhixuId")?,
            "orderId": value_str(event, "orderId")?,
            "hookId": value_str(event, "hookId")?,
            "stageIdentifier": value_str(event, "stageIdentifier")?,
            "hookName": value_str(event, "hookName")?,
        })),
        "HookStatusChanged" => {
            let mut out = base_hook_observation(
                "HookStatusChanged",
                &OracleOrderState {
                    plan_id: value_str(event, "planId")?.to_string(),
                    zhixu_id: value_str(event, "zhixuId")?.to_string(),
                    order_id: value_str(event, "orderId")?.to_string(),
                    signals: BTreeMap::new(),
                    hook_statuses: BTreeMap::new(),
                    materialized_stages: BTreeMap::new(),
                },
                value_str(event, "hookId")?,
            );
            out.insert(
                "status".to_string(),
                Value::String(value_str(event, "status")?.to_string()),
            );
            if let Some(due_at) = event.get("dueAt").and_then(Value::as_str) {
                out.insert("dueAt".to_string(), Value::String(due_at.to_string()));
            }
            Ok(Value::Object(out))
        }
        other => Err(ReplayError::Message(format!(
            "unsupported expected observation {other}"
        ))),
    }
}

/// 观察配对契约：expected/observed 按 (planId, orderId, hookId) 分桶、桶内
/// 按到达序配对。键内字段一律字节精确匹配——编译器身份是大小写敏感的
/// （仅大小写不同的 stage/hook 是两个独立实体），折叠会错配或产生假
/// mismatch。全局下标配对会把不同 hook/订单间合法的事件流交错误配成
/// semantic-mismatch——交错是流布局，不是语义分叉。与合约
/// `_evaluateAffectedHooks` 的 per-key hookIds 序一致：每个事实键的观察
/// 序列只与该键自己的求值历史可比。
pub(crate) fn compare_hook_observations(expected: &[Value], observed: &[Value]) -> Vec<Value> {
    let mut mismatches = Vec::new();
    let mut expected_queues: BTreeMap<String, std::collections::VecDeque<&Value>> = BTreeMap::new();
    for item in expected {
        expected_queues
            .entry(hook_observation_key(item))
            .or_default()
            .push_back(item);
    }
    let mut observed_queues: BTreeMap<String, std::collections::VecDeque<&Value>> = BTreeMap::new();
    for item in observed {
        observed_queues
            .entry(hook_observation_key(item))
            .or_default()
            .push_back(item);
    }
    let keys: BTreeSet<String> = expected_queues
        .keys()
        .chain(observed_queues.keys())
        .cloned()
        .collect();
    for key in keys {
        let mut expected_queue = expected_queues.remove(&key).unwrap_or_default();
        let mut observed_queue = observed_queues.remove(&key).unwrap_or_default();
        for occurrence in 0.. {
            match (expected_queue.pop_front(), observed_queue.pop_front()) {
                (None, None) => break,
                (None, Some(observed_item)) => mismatches.push(json!({
                    "hook": key,
                    "occurrence": occurrence,
                    "reason": "unexpected-observed",
                    "observed": observed_item,
                })),
                (Some(expected_item), None) => mismatches.push(json!({
                    "hook": key,
                    "occurrence": occurrence,
                    "reason": "missing-observed",
                    "expected": expected_item,
                })),
                (Some(expected_item), Some(observed_item)) => {
                    if !same_hook_observation(expected_item, observed_item) {
                        mismatches.push(json!({
                            "hook": key,
                            "occurrence": occurrence,
                            "reason": "semantic-mismatch",
                            "expected": expected_item,
                            "observed": observed_item,
                        }));
                    }
                }
            }
        }
    }
    mismatches
}

pub(crate) fn hook_observation_key(observation: &Value) -> String {
    format!(
        "{}::{}::{}",
        observation
            .get("planId")
            .and_then(Value::as_str)
            .unwrap_or_default(),
        observation
            .get("orderId")
            .and_then(Value::as_str)
            .unwrap_or_default(),
        observation
            .get("hookId")
            .and_then(Value::as_str)
            .unwrap_or_default(),
    )
}

pub(crate) fn same_hook_observation(expected: &Value, observed: &Value) -> bool {
    let expected_name = expected.get("eventName").and_then(Value::as_str);
    let observed_name = observed.get("eventName").and_then(Value::as_str);
    if expected_name != observed_name {
        return false;
    }
    match expected_name {
        Some("HookReady") => {
            field_eq(expected, observed, "planId")
                && field_eq(expected, observed, "zhixuId")
                && field_eq(expected, observed, "orderId")
                && field_eq(expected, observed, "hookId")
                && field_eq(expected, observed, "stageIdentifier")
                && field_eq(expected, observed, "hookName")
        }
        Some("HookStatusChanged") => {
            field_eq(expected, observed, "planId")
                && field_eq(expected, observed, "zhixuId")
                && field_eq(expected, observed, "orderId")
                && field_eq(expected, observed, "hookId")
                && field_eq(expected, observed, "status")
                && same_due_at(
                    expected.get("dueAt").and_then(Value::as_str),
                    observed.get("dueAt").and_then(Value::as_str),
                )
        }
        _ => false,
    }
}

/// dueAt 按时刻归一化比较：两侧都是合法 RFC3339 时刻时比时间点——毫秒
/// 位数/时区偏移写法是渲染细节，逐字节强耦合会把同一时刻误报成 mismatch。
/// 时刻不可解析（或一侧缺失）时按字面/缺席比较，不静默放行。
pub(crate) fn same_due_at(left: Option<&str>, right: Option<&str>) -> bool {
    match (left, right) {
        (None, None) => true,
        (Some(left), Some(right)) => match (normalized_instant(left), normalized_instant(right)) {
            (Some(left), Some(right)) => left == right,
            _ => left == right,
        },
        _ => false,
    }
}

fn normalized_instant(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|timestamp| timestamp.with_timezone(&Utc))
}

pub(crate) fn base_hook_observation(
    event_name: &str,
    order: &OracleOrderState,
    hook_id: &str,
) -> Map<String, Value> {
    let mut out = Map::new();
    out.insert(
        "eventName".to_string(),
        Value::String(event_name.to_string()),
    );
    out.insert("planId".to_string(), Value::String(order.plan_id.clone()));
    out.insert("zhixuId".to_string(), Value::String(order.zhixu_id.clone()));
    out.insert("orderId".to_string(), Value::String(order.order_id.clone()));
    out.insert("hookId".to_string(), Value::String(hook_id.to_string()));
    out
}
