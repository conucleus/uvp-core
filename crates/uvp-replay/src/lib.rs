//! uvp-replay：链上事件流对 hook 状态机的回放 oracle。crate 根保留
//! 入口（replay_json/replay_chain_events）、请求信封、跨模块共享的
//! JSON/时间读取辅助与公共导出；生产逻辑见 `facts/`、`transition/`、
//! `snapshot/`。

use chrono::{DateTime, TimeZone, Utc};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use thiserror::Error;

mod facts;
mod snapshot;
mod transition;

use facts::{order_key, validate_plan_registration_gates, OracleOrderState, OracleState};
use snapshot::compare_hook_observations;
use transition::{absorb_chain_observation, evaluate_timer_hook, record_signal_and_evaluate};

#[cfg(test)]
mod tests;

#[cfg(test)]
// 模块内测试直引求值内核与观察配对的内部构件（见 tests.rs）。
use snapshot::{hook_observation_key, same_hook_observation};
#[cfg(test)]
use transition::{
    delay_value, evaluate_hook, evaluate_instructions, false_value, not_value, or_value,
    signal_value, EvalValue,
};

#[derive(Debug, Error)]
pub enum ReplayError {
    #[error("{0}")]
    Message(String),
}

type Result<T> = std::result::Result<T, ReplayError>;

#[derive(Debug, Deserialize)]
// FFI/NAPI 最外层请求信封（对象形态）：未知字段确定性拒绝（拼错的调用方
// 输入不得被静默忽略成零值语义）。裸事件数组形态不经此结构。
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ReplayRequest {
    #[serde(default)]
    events: Option<Vec<Value>>,
    #[serde(default)]
    options: ReplayOptions,
}

#[derive(Debug, Default, Deserialize)]
// options 与外层信封同口径拒绝未知字段：拼错的键（如 strick）不得被
// 静默忽略成缺省语义。
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ReplayOptions {
    #[serde(default)]
    sort: Option<bool>,
    #[serde(default)]
    strict: Option<bool>,
}

pub fn replay_json(input: &str) -> String {
    let result = parse_replay_request(input).and_then(|(events, options)| {
        let result = replay_chain_events(events, &options)?;
        if options.strict.unwrap_or(true)
            && result
                .get("mismatches")
                .and_then(Value::as_array)
                .is_some_and(|items| !items.is_empty())
        {
            return Err(ReplayError::Message(format!(
                "chain replay mismatched {} hook observation(s)",
                result
                    .get("mismatches")
                    .and_then(Value::as_array)
                    .map_or(0, Vec::len)
            )));
        }
        Ok(result)
    });
    envelope_json(result)
}

pub fn replay_chain_events(mut events: Vec<Value>, options: &ReplayOptions) -> Result<Value> {
    if options.sort.unwrap_or(true) {
        // blockNumber/logIndex 是排序键：非整数（或缺失）折 0 会把事件流
        // 静默重排成错误因果序——排序前响亮校验，不做默认值兜底。
        for event in &events {
            for key in ["blockNumber", "logIndex"] {
                value_i64(event, key).map_err(|err| {
                    ReplayError::Message(format!(
                        "chain oracle event {} (sorting refuses to fold a non-integer to 0): {err}",
                        value_str(event, "eventName").unwrap_or("<unknown>")
                    ))
                })?;
            }
        }
        events.sort_by(|left, right| {
            event_i64(left, "blockNumber")
                .cmp(&event_i64(right, "blockNumber"))
                .then(event_i64(left, "logIndex").cmp(&event_i64(right, "logIndex")))
        });
    }

    let mut state = OracleState::default();
    let mut expected = Vec::new();
    let mut observed = Vec::new();
    let mut last_status_observations: BTreeMap<String, (String, Option<String>)> = BTreeMap::new();

    for event in &events {
        let event_name = value_str(event, "eventName")?;
        match event_name {
            "PlanRegistered" => {
                let plan = event.get("plan").cloned().ok_or_else(|| {
                    ReplayError::Message("PlanRegistered.plan is required".to_string())
                })?;
                let plan_id = value_str(&plan, "planId")?.to_string();
                validate_plan_registration_gates(&plan)?;
                state.plans.insert(plan_id, plan);
            }
            "OrderRegistered" => {
                let plan_id = value_str(event, "planId")?.to_string();
                let zhixu_id = value_str(event, "zhixuId")?.to_string();
                let order_id = value_str(event, "orderId")?.to_string();
                let key = order_key(&plan_id, &order_id);
                // 合约对重复注册 revert OrderAlreadyRegistered——订单在链上
                // 恰注册一次，事件流中的重复 OrderRegistered 只能是投影
                // 重放/重发。吸收（保留已积累的信号与钩子状态），不按
                // "重新注册"清空状态：第二次注册从未在链上发生，重置会把
                // 已验证的事实吞成空洞。同键不同 zhixu 的身份矛盾是损坏
                // 的事件流，响亮失败。
                if let Some(existing) = state.orders.get(&key) {
                    if existing.zhixu_id != zhixu_id {
                        return Err(ReplayError::Message(format!(
                            "duplicate OrderRegistered for {key} carries a different zhixuId ({} != {}): a contradictory event stream",
                            existing.zhixu_id, zhixu_id
                        )));
                    }
                } else {
                    state.orders.insert(
                        key,
                        OracleOrderState {
                            plan_id,
                            zhixu_id,
                            order_id,
                            signals: BTreeMap::new(),
                            hook_statuses: BTreeMap::new(),
                            materialized_stages: BTreeMap::new(),
                        },
                    );
                }
            }
            "SignalSubmitted" => {
                observed.extend(record_signal_and_evaluate(&mut state, event)?);
            }
            "TimerPoked" => {
                observed.extend(evaluate_timer_hook(&mut state, event)?);
            }
            "HookReady" | "HookStatusChanged" => {
                absorb_chain_observation(
                    &mut state,
                    &mut expected,
                    &mut observed,
                    &mut last_status_observations,
                    event,
                )?;
            }
            // StageMaterialized 被 oracle 消费：链上物化事实回填本地状态，
            // 与 oracle 自推导的物化路径（trigger / emit-ready hook Ready）
            // 互为补充，后续依赖该阶段的 watcher 求值据此放行。
            "StageMaterialized" => {
                let plan_id = value_str(event, "planId")?;
                let order_id = value_str(event, "orderId")?;
                let stage_id = value_str(event, "stageId")?;
                let order = state
                    .orders
                    .get_mut(&order_key(plan_id, order_id))
                    .ok_or_else(|| {
                        ReplayError::Message(format!(
                            "chain oracle missing order {plan_id}:{order_id}"
                        ))
                    })?;
                order.materialized_stages.insert(stage_id.to_string(), true);
            }
            // OrderTriggered 被记录为出生事务标记（outside 出生事实与其同
            // 事务；order-link 出生不 _recordSignal，见 record_signal_and_evaluate
            // 的出生通道判别）。合约一单恰发一次 OrderTriggered：同键再次
            // 到达且事务哈希不同是损坏的事件流——静默覆盖会改写出生通道
            // 判别基準，响亮失败（与 OrderRegistered 的矛盾流检测同形）。
            "OrderTriggered" => {
                let order_key =
                    order_key(value_str(event, "planId")?, value_str(event, "orderId")?);
                let trigger_tx = value_str(event, "transactionHash")?.to_string();
                match state.order_trigger_tx.get(&order_key) {
                    Some(existing) if *existing != trigger_tx => {
                        return Err(ReplayError::Message(format!(
                            "duplicate OrderTriggered for {order_key} carries a different transactionHash ({existing} != {trigger_tx}): a contradictory event stream"
                        )));
                    }
                    Some(_) => {}
                    None => {
                        state.order_trigger_tx.insert(order_key, trigger_tx);
                    }
                }
            }
            "OrderMaterialized" | "OrderLinked" => {}
            other => {
                return Err(ReplayError::Message(format!(
                    "unsupported chain-mode value {other}"
                )))
            }
        }
    }

    let mismatches = compare_hook_observations(&expected, &observed);
    Ok(json!({
        "state": state.to_json(),
        "expected": expected,
        "observed": observed,
        "mismatches": mismatches,
    }))
}

fn parse_replay_request(input: &str) -> Result<(Vec<Value>, ReplayOptions)> {
    let value: Value = serde_json::from_str(input)
        .map_err(|err| ReplayError::Message(format!("invalid replay request: {err}")))?;
    if let Some(items) = value.as_array() {
        return Ok((items.clone(), ReplayOptions::default()));
    }
    let request: ReplayRequest = serde_json::from_value(value)
        .map_err(|err| ReplayError::Message(format!("invalid replay request: {err}")))?;
    let events = request.events.ok_or_else(|| {
        ReplayError::Message("replay request is missing the required \"events\" field".to_string())
    })?;
    Ok((events, request.options))
}

fn chain_event_id(event: &Value) -> Result<String> {
    Ok(format!(
        "{}:{}:{}",
        value_i64(event, "blockNumber")?,
        value_i64(event, "logIndex")?,
        value_str(event, "transactionHash")?
    ))
}

fn seconds_from_iso(value: &str) -> Result<i64> {
    DateTime::parse_from_rfc3339(value)
        .map(|timestamp| timestamp.timestamp())
        .map_err(|_| ReplayError::Message(format!("invalid chain oracle timestamp {value}")))
}

fn iso_from_seconds(value: i64) -> Option<String> {
    // "无 due"由 EvalValue::due_at 的显式 Option 区分，epoch 0 不是哨兵：
    // epoch 0 的等待期限照常渲染，poke 资格闸按存在性判断。
    Some(
        Utc.timestamp_opt(value, 0)
            .single()?
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
    )
}

/// dueAt 渲染失败要响亮失败：把不可渲染的期限折成 None 会让等待行变成
/// 无期限的永久 wait（poke 资格闸按存在性判永不合资格）——静默吞掉一个
/// 确定性的毒输入，不如报错让事件流的问题暴露。
fn render_due_at(seconds: i64) -> Result<String> {
    iso_from_seconds(seconds).ok_or_else(|| {
        ReplayError::Message(format!(
            "wait dueAt {seconds} cannot be rendered as a timestamp (delay horizon out of the renderable range): refusing to fold to an undated permanent wait"
        ))
    })
}

fn field_eq(left: &Value, right: &Value, key: &str) -> bool {
    left.get(key) == right.get(key)
}

fn value_str<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| ReplayError::Message(format!("{key} must be a string")))
}

fn value_i64(value: &Value, key: &str) -> Result<i64> {
    value
        .get(key)
        .and_then(Value::as_i64)
        .ok_or_else(|| ReplayError::Message(format!("{key} must be an integer")))
}

fn event_i64(value: &Value, key: &str) -> i64 {
    value.get(key).and_then(Value::as_i64).unwrap_or(0)
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct Envelope {
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    value: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    diagnostics: Option<Vec<Diagnostic>>,
}

#[derive(Debug, serde::Serialize)]
struct Diagnostic {
    message: String,
}

fn envelope_json(result: Result<Value>) -> String {
    let envelope = match result {
        Ok(value) => Envelope {
            ok: true,
            value: Some(value),
            diagnostics: None,
        },
        Err(err) => Envelope {
            ok: false,
            value: None,
            diagnostics: Some(vec![Diagnostic {
                message: err.to_string(),
            }]),
        },
    };
    serde_json::to_string(&envelope).expect("replay envelope should serialize")
}
