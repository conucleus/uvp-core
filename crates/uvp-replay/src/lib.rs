use chrono::{DateTime, TimeZone, Utc};
use serde::Deserialize;
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ReplayError {
    #[error("{0}")]
    Message(String),
}

type Result<T> = std::result::Result<T, ReplayError>;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReplayRequest {
    #[serde(default)]
    events: Option<Vec<Value>>,
    #[serde(default)]
    options: ReplayOptions,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
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
        events.sort_by(|left, right| {
            event_i64(left, "blockNumber")
                .cmp(&event_i64(right, "blockNumber"))
                .then(event_i64(left, "logIndex").cmp(&event_i64(right, "logIndex")))
        });
    }

    let mut state = OracleState::default();
    let mut expected = Vec::new();
    let mut observed = Vec::new();

    for event in &events {
        let event_name = value_str(event, "eventName")?;
        match event_name {
            "PlanRegistered" => {
                let plan = event.get("plan").cloned().ok_or_else(|| {
                    ReplayError::Message("PlanRegistered.plan is required".to_string())
                })?;
                let plan_id = value_str(&plan, "planId")?.to_string();
                state.plans.insert(plan_id, plan);
            }
            "OrderRegistered" => {
                let zhixu_id = value_str(event, "zhixuId")?.to_string();
                let order_id = value_str(event, "orderId")?.to_string();
                state.orders.insert(
                    order_key(&zhixu_id, &order_id),
                    OracleOrderState {
                        plan_id: value_str(event, "planId")?.to_string(),
                        zhixu_id,
                        order_id,
                        signals: BTreeMap::new(),
                        hook_statuses: BTreeMap::new(),
                        materialized_stages: BTreeMap::new(),
                    },
                );
            }
            "SignalSubmitted" => {
                observed.extend(record_signal_and_evaluate(&mut state, event)?);
            }
            "TimerPoked" => {
                observed.extend(evaluate_timer_hook(&mut state, event)?);
            }
            "HookReady" | "HookStatusChanged" => {
                expected.push(chain_event_to_expected_observation(event)?);
            }
            "OrderMaterialized" | "OrderTriggered" | "OrderLinked" | "StageMaterialized" => {}
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

#[derive(Default)]
struct OracleState {
    plans: BTreeMap<String, Value>,
    orders: BTreeMap<String, OracleOrderState>,
}

struct OracleOrderState {
    plan_id: String,
    zhixu_id: String,
    order_id: String,
    signals: BTreeMap<String, Value>,
    hook_statuses: BTreeMap<String, HookRuntime>,
    materialized_stages: BTreeMap<String, bool>,
}

#[derive(Clone)]
struct HookRuntime {
    status: String,
    due_at: Option<String>,
    ready_emitted: bool,
}

#[derive(Clone, Copy)]
struct EvalValue {
    value: bool,
    wait: bool,
    cancel: bool,
    due_at: i64,
    anchor_at: i64,
}

impl OracleState {
    fn to_json(&self) -> Value {
        json!({
            "plans": self.plans,
            "orders": self.orders.iter().map(|(key, order)| (key.clone(), order.to_json())).collect::<Map<_, _>>(),
        })
    }
}

impl OracleOrderState {
    fn to_json(&self) -> Value {
        let hook_statuses = self
            .hook_statuses
            .iter()
            .map(|(key, runtime)| (key.clone(), runtime.to_json()))
            .collect::<Map<_, _>>();
        json!({
            "planId": self.plan_id,
            "zhixuId": self.zhixu_id,
            "orderId": self.order_id,
            "signals": self.signals,
            "hookStatuses": hook_statuses,
            "materializedStages": self.materialized_stages,
        })
    }
}

impl HookRuntime {
    fn init() -> Self {
        Self {
            status: "init".to_string(),
            due_at: None,
            ready_emitted: false,
        }
    }

    fn to_json(&self) -> Value {
        let mut out = Map::new();
        out.insert("status".to_string(), Value::String(self.status.clone()));
        if let Some(due_at) = &self.due_at {
            out.insert("dueAt".to_string(), Value::String(due_at.clone()));
        }
        out.insert("readyEmitted".to_string(), Value::Bool(self.ready_emitted));
        Value::Object(out)
    }
}

fn parse_replay_request(input: &str) -> Result<(Vec<Value>, ReplayOptions)> {
    let value: Value = serde_json::from_str(input)
        .map_err(|err| ReplayError::Message(format!("invalid replay request: {err}")))?;
    if let Some(items) = value.as_array() {
        return Ok((items.clone(), ReplayOptions::default()));
    }
    let request: ReplayRequest = serde_json::from_value(value)
        .map_err(|err| ReplayError::Message(format!("invalid replay request: {err}")))?;
    Ok((request.events.unwrap_or_default(), request.options))
}

fn record_signal_and_evaluate(state: &mut OracleState, event: &Value) -> Result<Vec<Value>> {
    let zhixu_id = value_str(event, "zhixuId")?;
    let order_id = value_str(event, "orderId")?;
    let order_key = order_key(zhixu_id, order_id);
    let order = state.orders.get_mut(&order_key).ok_or_else(|| {
        ReplayError::Message(format!("chain oracle missing order {zhixu_id}:{order_id}"))
    })?;
    let signal_key = value_str(event, "signalKey")?.to_string();
    if order.signals.contains_key(&signal_key) {
        return Ok(Vec::new());
    }
    order.signals.insert(
        signal_key.clone(),
        json!({
            "eventId": chain_event_id(event)?,
            "sourceId": value_str(event, "sourceId")?,
            "signalId": value_str(event, "signalId")?,
            "signalKey": signal_key,
            "senderId": value_str(event, "senderId")?,
            "submittedAt": value_str(event, "submittedAt")?,
        }),
    );

    let plan = state.plans.get(&order.plan_id).cloned().ok_or_else(|| {
        ReplayError::Message(format!("chain oracle missing plan {}", order.plan_id))
    })?;
    let hook_ids = plan
        .get("dependencyIndex")
        .and_then(|index| index.get(value_str(event, "signalKey").unwrap_or_default()))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let hooks = hook_ids
        .iter()
        .map(|hook_id| find_hook(&plan, hook_id.as_str().unwrap_or_default()))
        .collect::<Result<Vec<_>>>()?;
    let mut observations = Vec::new();
    for hook in hooks.iter().filter(|hook| {
        hook.get("isTrigger")
            .and_then(Value::as_bool)
            .unwrap_or(false)
    }) {
        observations.extend(evaluate_hook(
            order,
            hook,
            value_str(event, "submittedAt")?,
        )?);
    }
    for hook in hooks.iter().filter(|hook| {
        !hook
            .get("isTrigger")
            .and_then(Value::as_bool)
            .unwrap_or(false)
    }) {
        observations.extend(evaluate_hook(
            order,
            hook,
            value_str(event, "submittedAt")?,
        )?);
    }
    Ok(observations)
}

fn evaluate_timer_hook(state: &mut OracleState, event: &Value) -> Result<Vec<Value>> {
    let zhixu_id = value_str(event, "zhixuId")?;
    let order_id = value_str(event, "orderId")?;
    let order_key = order_key(zhixu_id, order_id);
    let order = state.orders.get_mut(&order_key).ok_or_else(|| {
        ReplayError::Message(format!("chain oracle missing order {zhixu_id}:{order_id}"))
    })?;
    let plan = state.plans.get(&order.plan_id).cloned().ok_or_else(|| {
        ReplayError::Message(format!("chain oracle missing plan {}", order.plan_id))
    })?;
    let hook = find_hook(&plan, value_str(event, "hookId")?)?;
    evaluate_hook(order, &hook, value_str(event, "pokedAt")?)
}

fn evaluate_hook(order: &mut OracleOrderState, hook: &Value, now: &str) -> Result<Vec<Value>> {
    let hook_id = value_str(hook, "hookId")?;
    let previous = order
        .hook_statuses
        .get(hook_id)
        .cloned()
        .unwrap_or_else(HookRuntime::init);
    if previous.status == "cxl" || previous.status == "reg" {
        return Ok(Vec::new());
    }
    let is_trigger = hook
        .get("isTrigger")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let stage_id = value_str(hook, "stageId")?;
    if !is_trigger
        && !order
            .materialized_stages
            .get(stage_id)
            .copied()
            .unwrap_or(false)
    {
        return Ok(Vec::new());
    }

    let result = evaluate_instructions(
        order,
        hook.get("instructions")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                ReplayError::Message(format!("chain oracle hook {hook_id} missing instructions"))
            })?,
        now,
    )?;
    let mut next = HookRuntime {
        status: "init".to_string(),
        due_at: None,
        ready_emitted: previous.ready_emitted,
    };
    if result.cancel {
        next.status = "cxl".to_string();
    } else if result.wait {
        next.status = "wait".to_string();
        next.due_at = iso_from_seconds(result.due_at);
    } else if result.value {
        next.status = "reg".to_string();
    }
    order
        .hook_statuses
        .insert(hook_id.to_string(), next.clone());

    let mut observations = Vec::new();
    if (previous.status != next.status || previous.due_at != next.due_at) && next.status == "wait" {
        let mut waiting = base_hook_observation("HookStatusChanged", order, hook_id);
        waiting.insert("status".to_string(), Value::String("wait".to_string()));
        if let Some(due_at) = &next.due_at {
            waiting.insert("dueAt".to_string(), Value::String(due_at.clone()));
        }
        observations.push(Value::Object(waiting));
    }
    if previous.status != next.status && next.status == "cxl" {
        let mut changed = base_hook_observation("HookStatusChanged", order, hook_id);
        changed.insert("status".to_string(), Value::String("cxl".to_string()));
        observations.push(Value::Object(changed));
    }
    if next.status == "reg" && is_trigger && !previous.ready_emitted {
        next.ready_emitted = true;
        order.hook_statuses.insert(hook_id.to_string(), next);
        order.materialized_stages.insert(stage_id.to_string(), true);
        let mut ready = base_hook_observation("HookReady", order, hook_id);
        ready.insert(
            "stageIdentifier".to_string(),
            Value::String(value_str(hook, "stageIdentifier")?.to_string()),
        );
        ready.insert(
            "hookName".to_string(),
            Value::String(value_str(hook, "hookName")?.to_string()),
        );
        observations.push(Value::Object(ready));
    }
    Ok(observations)
}

fn evaluate_instructions(
    order: &OracleOrderState,
    instructions: &[Value],
    now: &str,
) -> Result<EvalValue> {
    let mut stack = Vec::new();
    for instruction in instructions {
        match value_str(instruction, "op")? {
            "SIGNAL" => stack.push(signal_value(order, value_str(instruction, "signalKey")?)),
            "NOT" => {
                let value = stack.pop().unwrap_or_else(false_value);
                stack.push(not_value(value));
            }
            "DELAY" => {
                let value = stack.pop().unwrap_or_else(false_value);
                stack.push(delay_value(
                    value,
                    value_i64(instruction, "delaySeconds")?,
                    now,
                )?);
            }
            "AND" => {
                let arity = value_i64(instruction, "arity")?.max(0) as usize;
                let split = stack.len().saturating_sub(arity);
                let terms = stack.split_off(split);
                stack.push(
                    terms
                        .into_iter()
                        .reduce(and_value)
                        .unwrap_or_else(false_value),
                );
            }
            "OR" => {
                let arity = value_i64(instruction, "arity")?.max(0) as usize;
                let split = stack.len().saturating_sub(arity);
                let terms = stack.split_off(split);
                stack.push(
                    terms
                        .into_iter()
                        .reduce(or_value)
                        .unwrap_or_else(false_value),
                );
            }
            other => {
                return Err(ReplayError::Message(format!(
                    "unsupported chain-mode instruction {other}"
                )))
            }
        }
    }
    Ok(stack.first().copied().unwrap_or_else(false_value))
}

fn signal_value(order: &OracleOrderState, signal_key: &str) -> EvalValue {
    let Some(signal) = order.signals.get(signal_key) else {
        return false_value();
    };
    let submitted_at = value_str(signal, "submittedAt")
        .and_then(seconds_from_iso)
        .unwrap_or(0);
    EvalValue {
        value: true,
        wait: false,
        cancel: false,
        due_at: 0,
        anchor_at: submitted_at,
    }
}

fn false_value() -> EvalValue {
    EvalValue {
        value: false,
        wait: false,
        cancel: false,
        due_at: 0,
        anchor_at: 0,
    }
}

fn not_value(value: EvalValue) -> EvalValue {
    if value.value || value.wait {
        return EvalValue {
            value: false,
            wait: false,
            cancel: true,
            due_at: 0,
            anchor_at: 0,
        };
    }
    EvalValue {
        value: true,
        wait: false,
        cancel: false,
        due_at: 0,
        anchor_at: 0,
    }
}

fn delay_value(value: EvalValue, delay_seconds: i64, now: &str) -> Result<EvalValue> {
    if value.cancel || !value.value {
        return Ok(value);
    }
    let due_at = value.anchor_at + delay_seconds;
    if seconds_from_iso(now)? < due_at {
        return Ok(EvalValue {
            value: false,
            wait: true,
            cancel: false,
            due_at,
            anchor_at: value.anchor_at,
        });
    }
    Ok(EvalValue {
        value: true,
        wait: false,
        cancel: false,
        due_at: 0,
        anchor_at: value.anchor_at,
    })
}

fn and_value(left: EvalValue, right: EvalValue) -> EvalValue {
    if left.cancel || right.cancel {
        return EvalValue {
            value: false,
            wait: false,
            cancel: true,
            due_at: 0,
            anchor_at: 0,
        };
    }
    if left.value && right.value {
        return EvalValue {
            value: true,
            wait: false,
            cancel: false,
            due_at: 0,
            anchor_at: left.anchor_at.max(right.anchor_at),
        };
    }
    if (left.wait && (right.value || right.wait)) || (right.wait && (left.value || left.wait)) {
        return EvalValue {
            value: false,
            wait: true,
            cancel: false,
            due_at: left.due_at.max(right.due_at),
            anchor_at: left.anchor_at.max(right.anchor_at),
        };
    }
    false_value()
}

fn or_value(left: EvalValue, right: EvalValue) -> EvalValue {
    if left.value || right.value {
        return EvalValue {
            value: true,
            wait: false,
            cancel: false,
            due_at: 0,
            anchor_at: left.anchor_at.max(right.anchor_at),
        };
    }
    if left.wait || right.wait {
        return EvalValue {
            value: false,
            wait: true,
            cancel: false,
            due_at: min_non_zero(left.due_at, right.due_at),
            anchor_at: left.anchor_at.max(right.anchor_at),
        };
    }
    if left.cancel && right.cancel {
        return EvalValue {
            value: false,
            wait: false,
            cancel: true,
            due_at: 0,
            anchor_at: 0,
        };
    }
    false_value()
}

fn chain_event_to_expected_observation(event: &Value) -> Result<Value> {
    match value_str(event, "eventName")? {
        "HookReady" => Ok(json!({
            "eventName": "HookReady",
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
                    plan_id: String::new(),
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

fn compare_hook_observations(expected: &[Value], observed: &[Value]) -> Vec<Value> {
    let mut mismatches = Vec::new();
    let length = expected.len().max(observed.len());
    for index in 0..length {
        match (expected.get(index), observed.get(index)) {
            (None, Some(observed_item)) => mismatches.push(json!({
                "index": index,
                "reason": "unexpected-observed",
                "observed": observed_item,
            })),
            (Some(expected_item), None) => mismatches.push(json!({
                "index": index,
                "reason": "missing-observed",
                "expected": expected_item,
            })),
            (Some(expected_item), Some(observed_item))
                if !same_hook_observation(expected_item, observed_item) =>
            {
                mismatches.push(json!({
                    "index": index,
                    "reason": "semantic-mismatch",
                    "expected": expected_item,
                    "observed": observed_item,
                }));
            }
            _ => {}
        }
    }
    mismatches
}

fn same_hook_observation(expected: &Value, observed: &Value) -> bool {
    let expected_name = expected.get("eventName").and_then(Value::as_str);
    let observed_name = observed.get("eventName").and_then(Value::as_str);
    if expected_name != observed_name {
        return false;
    }
    match expected_name {
        Some("HookReady") => {
            field_eq(expected, observed, "zhixuId")
                && field_eq(expected, observed, "orderId")
                && field_lower_eq(expected, observed, "hookId")
                && field_eq(expected, observed, "stageIdentifier")
                && field_eq(expected, observed, "hookName")
        }
        Some("HookStatusChanged") => {
            field_eq(expected, observed, "zhixuId")
                && field_eq(expected, observed, "orderId")
                && field_lower_eq(expected, observed, "hookId")
                && field_eq(expected, observed, "status")
                && field_eq(expected, observed, "dueAt")
        }
        _ => false,
    }
}

fn find_hook(plan: &Value, hook_id: &str) -> Result<Value> {
    let hooks = plan
        .get("compiledHooks")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            ReplayError::Message("chain oracle plan missing compiledHooks".to_string())
        })?;
    hooks
        .iter()
        .find(|hook| {
            hook.get("hookId")
                .and_then(Value::as_str)
                .is_some_and(|candidate| candidate.eq_ignore_ascii_case(hook_id))
        })
        .cloned()
        .ok_or_else(|| ReplayError::Message(format!("chain oracle missing hook {hook_id}")))
}

fn base_hook_observation(
    event_name: &str,
    order: &OracleOrderState,
    hook_id: &str,
) -> Map<String, Value> {
    let mut out = Map::new();
    out.insert(
        "eventName".to_string(),
        Value::String(event_name.to_string()),
    );
    out.insert("zhixuId".to_string(), Value::String(order.zhixu_id.clone()));
    out.insert("orderId".to_string(), Value::String(order.order_id.clone()));
    out.insert("hookId".to_string(), Value::String(hook_id.to_string()));
    out
}

fn order_key(zhixu_id: &str, order_id: &str) -> String {
    format!("{zhixu_id}::{order_id}")
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
    if value == 0 {
        return None;
    }
    Some(
        Utc.timestamp_opt(value, 0)
            .single()?
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
    )
}

fn min_non_zero(left: i64, right: i64) -> i64 {
    if left == 0 {
        return right;
    }
    if right == 0 {
        return left;
    }
    left.min(right)
}

fn field_eq(left: &Value, right: &Value, key: &str) -> bool {
    left.get(key) == right.get(key)
}

fn field_lower_eq(left: &Value, right: &Value, key: &str) -> bool {
    left.get(key)
        .and_then(Value::as_str)
        .zip(right.get(key).and_then(Value::as_str))
        .is_some_and(|(left, right)| left.eq_ignore_ascii_case(right))
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn replays_ready_hook() {
        let events = vec![
            json!({
                "eventName": "PlanRegistered",
                "blockNumber": 1,
                "logIndex": 0,
                "transactionHash": "0x01",
                "plan": {
                    "planId": "0x01",
                    "zhixuId": "demo",
                    "version": "1",
                    "compiledHooks": [{
                        "hookId": "0x10",
                        "stageId": "0x20",
                        "stageIdentifier": "flow.start",
                        "hookName": "START",
                        "isTrigger": true,
                        "instructions": [{
                            "op": "SIGNAL",
                            "sourceId": "0x30",
                            "signalId": "0x40",
                            "signalKey": "0x50"
                        }]
                    }],
                    "dependencyIndex": { "0x50": ["0x10"] }
                }
            }),
            json!({
                "eventName": "OrderRegistered",
                "blockNumber": 2,
                "logIndex": 0,
                "transactionHash": "0x02",
                "planId": "0x01",
                "zhixuId": "demo",
                "orderId": "order-1",
                "registeredAt": "2026-04-27T00:00:00.000Z"
            }),
            json!({
                "eventName": "SignalSubmitted",
                "blockNumber": 3,
                "logIndex": 0,
                "transactionHash": "0x03",
                "zhixuId": "demo",
                "orderId": "order-1",
                "sourceId": "0x30",
                "signalId": "0x40",
                "signalKey": "0x50",
                "senderId": "sender",
                "submittedAt": "2026-04-27T00:00:00.000Z"
            }),
        ];
        let result = replay_chain_events(
            events,
            &ReplayOptions {
                sort: None,
                strict: Some(false),
            },
        )
        .unwrap();
        assert_eq!(
            result["observed"][0],
            json!({
                "eventName": "HookReady",
                "zhixuId": "demo",
                "orderId": "order-1",
                "hookId": "0x10",
                "stageIdentifier": "flow.start",
                "hookName": "START"
            })
        );
    }
}
