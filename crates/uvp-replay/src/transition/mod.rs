//! 状态迁移：链上事件吸收（观察裁剪/出生推导）、信号登记触发的求值与
//! 指令栈求值器。

use serde_json::{json, Map, Value};
use std::collections::BTreeMap;

use crate::facts::{find_hook, order_key, HookRuntime, OracleOrderState, OracleState};
use crate::snapshot::{base_hook_observation, chain_event_to_expected_observation, same_due_at};
use crate::{chain_event_id, seconds_from_iso, value_i64, value_str, ReplayError, Result};

pub(crate) fn absorb_chain_observation(
    state: &mut OracleState,
    expected: &mut Vec<Value>,
    observed: &mut Vec<Value>,
    last_status: &mut BTreeMap<String, (String, Option<String>)>,
    event: &Value,
) -> Result<()> {
    match value_str(event, "eventName")? {
        "HookReady" => {
            expected.push(chain_event_to_expected_observation(event)?);
            derive_order_link_birth(state, observed, event)?;
            Ok(())
        }
        "HookStatusChanged" => {
            let status = value_str(event, "status")?.to_string();
            if status == "ready" || status == "init" {
                return Ok(());
            }
            let due_at = event
                .get("dueAt")
                .and_then(Value::as_str)
                .map(str::to_string);
            let key = format!(
                "{}#{}",
                order_key(value_str(event, "planId")?, value_str(event, "orderId")?),
                value_str(event, "hookId")?
            );
            if last_status
                .get(&key)
                .is_some_and(|(previous_status, previous_due)| {
                    *previous_status == status
                        && same_due_at(previous_due.as_deref(), due_at.as_deref())
                })
            {
                return Ok(());
            }
            last_status.insert(key, (status, due_at));
            expected.push(chain_event_to_expected_observation(event)?);
            Ok(())
        }
        other => Err(ReplayError::Message(format!(
            "unsupported expected observation {other}"
        ))),
    }
}

fn derive_order_link_birth(
    state: &mut OracleState,
    observed: &mut Vec<Value>,
    event: &Value,
) -> Result<()> {
    let (Ok(plan_id), Ok(order_id), Ok(hook_id)) = (
        value_str(event, "planId"),
        value_str(event, "orderId"),
        value_str(event, "hookId"),
    ) else {
        return Ok(());
    };
    if !state.orders.contains_key(&order_key(plan_id, order_id)) {
        return Ok(());
    }
    let Some(plan) = state.plans.get(plan_id) else {
        return Ok(());
    };
    let Ok(hook) = find_hook(plan, hook_id) else {
        return Ok(());
    };
    if order_trigger_kind(&hook)? != "mint" {
        return Ok(());
    }
    let stage_id = value_str(&hook, "stageId")?.to_string();
    let stage_identifier = value_str(&hook, "stageIdentifier")?.to_string();
    let hook_name = value_str(&hook, "hookName")?.to_string();
    let Some(order) = state.orders.get_mut(&order_key(plan_id, order_id)) else {
        return Ok(());
    };
    let mut runtime = order
        .hook_statuses
        .remove(hook_id)
        .unwrap_or_else(HookRuntime::init);
    if runtime.ready_emitted {
        order.hook_statuses.insert(hook_id.to_string(), runtime);
        return Ok(());
    }
    runtime.status = "ready".to_string();
    runtime.due_at = None;
    runtime.ready_emitted = true;
    let zhixu_id = order.zhixu_id.clone();
    order.hook_statuses.insert(hook_id.to_string(), runtime);
    order.materialized_stages.insert(stage_id, true);
    let mut ready = Map::new();
    ready.insert(
        "eventName".to_string(),
        Value::String("HookReady".to_string()),
    );
    ready.insert("planId".to_string(), Value::String(plan_id.to_string()));
    ready.insert("zhixuId".to_string(), Value::String(zhixu_id));
    ready.insert("orderId".to_string(), Value::String(order_id.to_string()));
    ready.insert("hookId".to_string(), Value::String(hook_id.to_string()));
    ready.insert(
        "stageIdentifier".to_string(),
        Value::String(stage_identifier),
    );
    ready.insert("hookName".to_string(), Value::String(hook_name));
    observed.push(Value::Object(ready));
    Ok(())
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct EvalValue {
    pub(crate) value: bool,
    pub(crate) wait: bool,
    pub(crate) cancel: bool,
    pub(crate) due_at: Option<i64>,
    pub(crate) anchor_at: Option<i64>,
}

pub(crate) fn record_signal_and_evaluate(
    state: &mut OracleState,
    event: &Value,
) -> Result<Vec<Value>> {
    let plan_id = value_str(event, "planId")?;
    let zhixu_id = value_str(event, "zhixuId")?;
    let order_id = value_str(event, "orderId")?;
    let order_key = order_key(plan_id, order_id);
    let order = state.orders.get_mut(&order_key).ok_or_else(|| {
        ReplayError::Message(format!(
            "chain oracle missing order {plan_id}:{zhixu_id}:{order_id}"
        ))
    })?;
    let signal_key = value_str(event, "signalKey")?.to_string();
    if order.signals.contains_key(&signal_key) {
        return Ok(Vec::new());
    }
    let signal_tx = value_str(event, "transactionHash")?;
    let is_first_signal = order.signals.is_empty();
    let order_link_born = state
        .order_trigger_tx
        .get(&order_key)
        .is_some_and(|trigger_tx| trigger_tx != signal_tx);
    let birth_channel = is_first_signal && !order_link_born;
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
    let mut trigger_hooks = Vec::new();
    let mut watcher_hooks = Vec::new();
    for hook in hooks {
        if hook_is_order_trigger(&hook)? {
            if birth_channel {
                trigger_hooks.push(hook);
            }
        } else {
            watcher_hooks.push(hook);
        }
    }
    for hook in &trigger_hooks {
        observations.extend(evaluate_hook(
            order,
            hook,
            value_str(event, "submittedAt")?,
        )?);
    }
    for hook in &watcher_hooks {
        observations.extend(evaluate_hook(
            order,
            hook,
            value_str(event, "submittedAt")?,
        )?);
    }
    Ok(observations)
}

pub(crate) fn evaluate_timer_hook(state: &mut OracleState, event: &Value) -> Result<Vec<Value>> {
    let plan_id = value_str(event, "planId")?;
    let zhixu_id = value_str(event, "zhixuId")?;
    let order_id = value_str(event, "orderId")?;
    let order_key = order_key(plan_id, order_id);
    let order = state.orders.get_mut(&order_key).ok_or_else(|| {
        ReplayError::Message(format!(
            "chain oracle missing order {plan_id}:{zhixu_id}:{order_id}"
        ))
    })?;
    let plan = state.plans.get(&order.plan_id).cloned().ok_or_else(|| {
        ReplayError::Message(format!("chain oracle missing plan {}", order.plan_id))
    })?;
    let hook_id = value_str(event, "hookId")?;
    let hook = find_hook(&plan, hook_id)?;
    let poked_at = value_str(event, "pokedAt")?;
    let eligible = match order.hook_statuses.get(hook_id) {
        Some(runtime) if runtime.status == "wait" => match runtime.due_at.as_deref() {
            Some(due_at) => seconds_from_iso(poked_at)? >= seconds_from_iso(due_at)?,
            None => false,
        },
        _ => false,
    };
    if !eligible {
        return Ok(Vec::new());
    }
    evaluate_hook(order, &hook, poked_at)
}

pub(crate) fn hook_is_order_trigger(hook: &Value) -> Result<bool> {
    Ok(matches!(order_trigger_kind(hook)?, "mint" | "dock"))
}

fn order_trigger_kind(hook: &Value) -> Result<&str> {
    let kind = hook
        .get("orderTriggerKind")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            ReplayError::Message(format!(
                "hook {} must carry an orderTriggerKind field",
                value_str(hook, "hookId").unwrap_or("<unknown>")
            ))
        })?;
    if matches!(kind, "mint" | "dock" | "none") {
        Ok(kind)
    } else {
        Err(ReplayError::Message(format!(
            "hook {} carries unsupported orderTriggerKind {kind}",
            value_str(hook, "hookId").unwrap_or("<unknown>")
        )))
    }
}

fn hook_emits_ready(hook: &Value) -> Result<bool> {
    match hook.get("emitReady") {
        Some(Value::Bool(value)) => Ok(*value),
        _ => Err(ReplayError::Message(format!(
            "hook {} emitReady must be a boolean",
            value_str(hook, "hookId").unwrap_or("<unknown>")
        ))),
    }
}

pub(crate) fn evaluate_hook(
    order: &mut OracleOrderState,
    hook: &Value,
    now: &str,
) -> Result<Vec<Value>> {
    let hook_id = value_str(hook, "hookId")?;
    let emits_ready = hook_emits_ready(hook)?;
    let is_trigger = hook_is_order_trigger(hook)?;
    let previous = order
        .hook_statuses
        .get(hook_id)
        .cloned()
        .unwrap_or_else(HookRuntime::init);
    if previous.status == "cxl" || previous.status == "ready" {
        return Ok(Vec::new());
    }
    let stage_id = value_str(hook, "stageId")?;
    let stage_materialized = order
        .materialized_stages
        .get(stage_id)
        .copied()
        .unwrap_or(false);
    if !is_trigger && !emits_ready && !stage_materialized {
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
        next.due_at = result.due_at.map(crate::render_due_at).transpose()?;
    } else if result.value {
        next.status = "ready".to_string();
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
    if next.status == "ready" && is_trigger && !stage_materialized {
        order.materialized_stages.insert(stage_id.to_string(), true);
    }
    if next.status == "ready" && emits_ready && !previous.ready_emitted {
        if !is_trigger && !stage_materialized {
            order.materialized_stages.insert(stage_id.to_string(), true);
        }
        next.ready_emitted = true;
        order.hook_statuses.insert(hook_id.to_string(), next);
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

pub(crate) fn evaluate_instructions(
    order: &OracleOrderState,
    instructions: &[Value],
    now: &str,
) -> Result<EvalValue> {
    let mut stack = Vec::new();
    for instruction in instructions {
        match value_str(instruction, "op")? {
            "SIGNAL" => stack.push(signal_value(order, value_str(instruction, "signalKey")?)?),
            "NOT" => {
                let Some(value) = stack.pop() else {
                    return Err(ReplayError::Message(
                        "malformed instruction plan: NOT requires one operand on the stack"
                            .to_string(),
                    ));
                };
                stack.push(not_value(value));
            }
            "DELAY" => {
                let Some(value) = stack.pop() else {
                    return Err(ReplayError::Message(
                        "malformed instruction plan: DELAY requires one operand on the stack"
                            .to_string(),
                    ));
                };
                stack.push(delay_value(
                    value,
                    value_i64(instruction, "delaySeconds")?,
                    now,
                )?);
            }
            "AND" | "OR" => {
                let op_name = value_str(instruction, "op")?;
                let is_and = op_name == "AND";
                let arity = value_i64(instruction, "arity")?;
                if arity < 2 {
                    return Err(ReplayError::Message(format!(
                        "malformed instruction plan: {} arity must be at least 2",
                        op_name
                    )));
                }
                let arity = arity as usize;
                if stack.len() < arity {
                    return Err(ReplayError::Message(format!(
                        "malformed instruction plan: {} requires {arity} operands but {} remain",
                        op_name,
                        stack.len()
                    )));
                }
                let terms = stack.split_off(stack.len() - arity);
                let combine = |left, right| {
                    if is_and {
                        and_value(left, right)
                    } else {
                        or_value(left, right)
                    }
                };
                let combined = terms.into_iter().reduce(combine);
                stack.push(combined.ok_or_else(|| {
                    ReplayError::Message(
                        "malformed instruction plan: boolean instruction produced no value"
                            .to_string(),
                    )
                })?);
            }
            other => {
                return Err(ReplayError::Message(format!(
                    "unsupported chain-mode instruction {other}"
                )))
            }
        }
    }
    if stack.len() != 1 {
        return Err(ReplayError::Message(format!(
            "malformed instruction plan: expected exactly one result value, found {}",
            stack.len()
        )));
    }
    Ok(stack[0])
}

pub(crate) fn signal_value(order: &OracleOrderState, signal_key: &str) -> Result<EvalValue> {
    let Some(signal) = order.signals.get(signal_key) else {
        return Ok(false_value());
    };
    let submitted_at = seconds_from_iso(value_str(signal, "submittedAt")?)?;
    Ok(EvalValue {
        value: true,
        wait: false,
        cancel: false,
        due_at: None,
        anchor_at: Some(submitted_at),
    })
}

pub(crate) fn false_value() -> EvalValue {
    EvalValue {
        value: false,
        wait: false,
        cancel: false,
        due_at: None,
        anchor_at: None,
    }
}

pub(crate) fn not_value(value: EvalValue) -> EvalValue {
    if value.wait {
        return EvalValue {
            value: true,
            wait: false,
            cancel: false,
            due_at: value.due_at,
            anchor_at: None,
        };
    }
    if value.value {
        return EvalValue {
            value: false,
            wait: false,
            cancel: true,
            due_at: None,
            anchor_at: None,
        };
    }
    EvalValue {
        value: true,
        wait: false,
        cancel: false,
        due_at: None,
        anchor_at: None,
    }
}

pub(crate) fn delay_value(value: EvalValue, delay_seconds: i64, now: &str) -> Result<EvalValue> {
    if delay_seconds <= 0 {
        return Err(ReplayError::Message(
            "malformed instruction plan: DELAY delaySeconds must be positive".to_string(),
        ));
    }
    if value.cancel || !value.value {
        return Ok(value);
    }
    let Some(anchor_at) = value.anchor_at else {
        return Err(ReplayError::Message(
            "malformed instruction plan: DELAY requires a positively anchored operand (no positive anchor)"
                .to_string(),
        ));
    };
    let due_at = anchor_at.checked_add(delay_seconds).ok_or_else(|| {
        ReplayError::Message("delay computation overflows the replay timestamp range".to_string())
    })?;
    if seconds_from_iso(now)? < due_at {
        return Ok(EvalValue {
            value: false,
            wait: true,
            cancel: false,
            due_at: Some(due_at),
            anchor_at: value.anchor_at,
        });
    }
    Ok(EvalValue {
        value: true,
        wait: false,
        cancel: false,
        due_at: None,
        anchor_at: Some(due_at),
    })
}

fn and_value(left: EvalValue, right: EvalValue) -> EvalValue {
    if left.cancel || right.cancel {
        return EvalValue {
            value: false,
            wait: false,
            cancel: true,
            due_at: None,
            anchor_at: None,
        };
    }
    if left.value && right.value {
        return EvalValue {
            value: true,
            wait: false,
            cancel: false,
            due_at: min_due(left.due_at, right.due_at),
            anchor_at: max_anchor(left.anchor_at, right.anchor_at),
        };
    }
    if (left.wait && (right.value || right.wait)) || (right.wait && (left.value || left.wait)) {
        return EvalValue {
            value: false,
            wait: true,
            cancel: false,
            due_at: max_due(
                left.wait.then_some(left.due_at).flatten(),
                right.wait.then_some(right.due_at).flatten(),
            ),
            anchor_at: max_anchor(left.anchor_at, right.anchor_at),
        };
    }
    false_value()
}

pub(crate) fn or_value(left: EvalValue, right: EvalValue) -> EvalValue {
    if left.value || right.value {
        let left_wins = if left.value && right.value {
            match (left.anchor_at, right.anchor_at) {
                (Some(left_anchor), Some(right_anchor)) => left_anchor <= right_anchor,
                (Some(_), None) => true,
                (None, Some(_)) => false,
                (None, None) => true,
            }
        } else {
            left.value
        };
        return EvalValue {
            value: true,
            wait: false,
            cancel: false,
            due_at: if left_wins { left.due_at } else { right.due_at },
            anchor_at: if left_wins {
                left.anchor_at
            } else {
                right.anchor_at
            },
        };
    }
    if left.wait || right.wait {
        return EvalValue {
            value: false,
            wait: true,
            cancel: false,
            due_at: min_due(left.due_at, right.due_at),
            anchor_at: min_anchor(left.anchor_at, right.anchor_at),
        };
    }
    if left.cancel && right.cancel {
        return EvalValue {
            value: false,
            wait: false,
            cancel: true,
            due_at: None,
            anchor_at: None,
        };
    }
    false_value()
}

fn max_anchor(left: Option<i64>, right: Option<i64>) -> Option<i64> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.max(right)),
        (Some(only), None) | (None, Some(only)) => Some(only),
        (None, None) => None,
    }
}

fn min_anchor(left: Option<i64>, right: Option<i64>) -> Option<i64> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.min(right)),
        (Some(only), None) | (None, Some(only)) => Some(only),
        (None, None) => None,
    }
}

fn max_due(left: Option<i64>, right: Option<i64>) -> Option<i64> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.max(right)),
        (Some(only), None) | (None, Some(only)) => Some(only),
        (None, None) => None,
    }
}

fn min_due(left: Option<i64>, right: Option<i64>) -> Option<i64> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.min(right)),
        (Some(only), None) | (None, Some(only)) => Some(only),
        (None, None) => None,
    }
}
