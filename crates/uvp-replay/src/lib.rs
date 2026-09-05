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
                let plan_id = value_str(event, "planId")?.to_string();
                let zhixu_id = value_str(event, "zhixuId")?.to_string();
                let order_id = value_str(event, "orderId")?.to_string();
                state.orders.insert(
                    order_key(&plan_id, &order_id),
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
            "SignalSubmitted" => {
                observed.extend(record_signal_and_evaluate(&mut state, event)?);
            }
            "TimerPoked" => {
                observed.extend(evaluate_timer_hook(&mut state, event)?);
            }
            "HookReady" | "HookStatusChanged" => {
                expected.push(chain_event_to_expected_observation(event)?);
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
            "OrderMaterialized" | "OrderTriggered" | "OrderLinked" => {}
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

#[derive(Default)]
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

#[derive(Clone, Copy, Debug)]
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
    let events = request.events.ok_or_else(|| {
        ReplayError::Message("replay request is missing the required \"events\" field".to_string())
    })?;
    Ok((events, request.options))
}

fn record_signal_and_evaluate(state: &mut OracleState, event: &Value) -> Result<Vec<Value>> {
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
    // 与合约 `_evaluateAffectedHooks` 相同的两遍顺序：先 trigger 后普通
    // watcher，保证出生/物化边先于同键观察者求值。
    let mut trigger_hooks = Vec::new();
    let mut watcher_hooks = Vec::new();
    for hook in hooks {
        if hook_is_order_trigger(&hook)? {
            trigger_hooks.push(hook);
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

fn evaluate_timer_hook(state: &mut OracleState, event: &Value) -> Result<Vec<Value>> {
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
    let hook = find_hook(&plan, value_str(event, "hookId")?)?;
    evaluate_hook(order, &hook, value_str(event, "pokedAt")?)
}

/// hook-plan v2 把单一 `isTrigger` 拆成 `orderTriggerKind`(mint|dock|none)
/// 加 `emitReady`（PRD94 §3.4）。legacy v1 `isTrigger` 布尔回退已随语义冻结
/// 批次删除：oracle 只认 v2 字段，缺失即结构性错误（fail-closed）。
fn hook_is_order_trigger(hook: &Value) -> Result<bool> {
    let kind = hook
        .get("orderTriggerKind")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            ReplayError::Message(format!(
            "hook {} must carry an orderTriggerKind field (legacy isTrigger artifacts are retired)",
            value_str(hook, "hookId").unwrap_or("<unknown>")
        ))
        })?;
    match kind {
        "mint" | "dock" => Ok(true),
        "none" => Ok(false),
        other => Err(ReplayError::Message(format!(
            "hook {} carries unsupported orderTriggerKind {other}",
            value_str(hook, "hookId").unwrap_or("<unknown>")
        ))),
    }
}

/// `emitReady` controls the observable readiness event independently from
/// order/stage materialization. The legacy "absent field falls back to
/// isTrigger" reading is retired: v2 artifacts must carry the boolean.
fn hook_emits_ready(hook: &Value) -> Result<bool> {
    match hook.get("emitReady") {
        Some(Value::Bool(value)) => Ok(*value),
        _ => Err(ReplayError::Message(format!(
            "hook {} emitReady must be a boolean (legacy isTrigger artifacts are retired)",
            value_str(hook, "hookId").unwrap_or("<unknown>")
        ))),
    }
}

fn evaluate_hook(order: &mut OracleOrderState, hook: &Value, now: &str) -> Result<Vec<Value>> {
    let hook_id = value_str(hook, "hookId")?;
    let emits_ready = hook_emits_ready(hook)?;
    let is_trigger = hook_is_order_trigger(hook)?;
    let previous = order
        .hook_statuses
        .get(hook_id)
        .cloned()
        .unwrap_or_else(HookRuntime::init);
    if previous.status == "cxl" || previous.status == "reg" {
        return Ok(Vec::new());
    }
    let stage_id = value_str(hook, "stageId")?;
    let stage_materialized = order
        .materialized_stages
        .get(stage_id)
        .copied()
        .unwrap_or(false);
    // 对齐合约 `_evaluateHook` 的初始化守卫：order-trigger 与 EMIT_READY
    // hook 允许先于阶段物化求值（前者是出生边、后者是 executor dispatch
    // 边，Ready 时物化自身阶段）；纯 flags=0 watcher 在阶段未物化时跳过
    // （合约侧重放该形态不可达——编译器已拒绝，这里是防御纵深）。
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
    // 阶段物化三线统一（簇 A）：`orderTriggerKind` 与 `emitReady` hook 都
    // 物化自身阶段——前者是出生边，后者是 executor dispatch 边（合约
    // `_evaluateHook` 的 EMIT_READY 分支同样调用 _materializeStage）。仅
    // emitReady=false 的沉默 trigger 物化但不发 HookReady。
    if next.status == "reg" && is_trigger && !stage_materialized {
        order.materialized_stages.insert(stage_id.to_string(), true);
    }
    if next.status == "reg" && emits_ready && !previous.ready_emitted {
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

fn evaluate_instructions(
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
                let is_and = value_str(instruction, "op")? == "AND";
                let arity = value_i64(instruction, "arity")?;
                if arity <= 0 {
                    return Err(ReplayError::Message(format!(
                        "malformed instruction plan: {} arity must be positive",
                        if is_and { "AND" } else { "OR" }
                    )));
                }
                let arity = arity as usize;
                if stack.len() < arity {
                    return Err(ReplayError::Message(format!(
                        "malformed instruction plan: {} requires {arity} operands but {} remain",
                        if is_and { "AND" } else { "OR" },
                        stack.len()
                    )));
                }
                let terms = stack.split_off(stack.len() - arity);
                let combined = if is_and {
                    terms.into_iter().reduce(and_value)
                } else {
                    terms.into_iter().reduce(or_value)
                };
                stack.push(combined.ok_or_else(|| {
                    ReplayError::Message(
                        "malformed instruction plan: boolean instruction produced no value"
                            .to_string(),
                    )
                })?);
            }
            // MERGE（撮合扇入，semantic 0.6）已作废：语料与编译器不再产出该
            // 指令，求值器一并移除——编码层保留的合约侧校验不再有合法生产者。
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

fn signal_value(order: &OracleOrderState, signal_key: &str) -> Result<EvalValue> {
    let Some(signal) = order.signals.get(signal_key) else {
        return Ok(false_value());
    };
    let submitted_at = seconds_from_iso(value_str(signal, "submittedAt")?)?;
    Ok(EvalValue {
        value: true,
        wait: false,
        cancel: false,
        due_at: 0,
        anchor_at: submitted_at,
    })
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
    let due_at = value.anchor_at.checked_add(delay_seconds).ok_or_else(|| {
        ReplayError::Message("delay computation overflows the replay timestamp range".to_string())
    })?;
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
        // 锚点推进（semantic 0.5 链式裁决）：延时到期时刻本身成为新的锚点，
        // 使 `(A+5s)+10s` 的外层延时从 A+5s 起算，与生产求值器一致。
        anchor_at: due_at,
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
    // Arrival-time causality (semantic 0.5): the earliest RECEIVED signal is
    // the cause. Only READY branches compete for the anchor; a waiting
    // branch's stale anchor must not win — the ready winner keeps its own
    // timer. Matches the core evaluator (uvp-hook-dsl Expr::Or) and the
    // contract's _orValue (P1-5: the replay oracle previously min-merged a
    // waiting branch's anchor into a ready result, producing due dates
    // earlier than the contract for `(a | (b +delay)) +outer` plans).
    if left.value || right.value {
        let anchor = if left.value && right.value {
            min_anchor(left.anchor_at, right.anchor_at)
        } else if left.value {
            left.anchor_at
        } else {
            right.anchor_at
        };
        return EvalValue {
            value: true,
            wait: false,
            cancel: false,
            due_at: 0,
            anchor_at: anchor,
        };
    }
    if left.wait || right.wait {
        return EvalValue {
            value: false,
            wait: true,
            cancel: false,
            due_at: min_non_zero(left.due_at, right.due_at),
            anchor_at: min_anchor(left.anchor_at, right.anchor_at),
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

fn min_anchor(left: i64, right: i64) -> i64 {
    if left == 0 {
        return right;
    }
    if right == 0 || left < right {
        return left;
    }
    right
}

fn chain_event_to_expected_observation(event: &Value) -> Result<Value> {
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
            field_eq(expected, observed, "planId")
                && field_eq(expected, observed, "zhixuId")
                && field_eq(expected, observed, "orderId")
                && field_lower_eq(expected, observed, "hookId")
                && field_eq(expected, observed, "stageIdentifier")
                && field_eq(expected, observed, "hookName")
        }
        Some("HookStatusChanged") => {
            field_eq(expected, observed, "planId")
                && field_eq(expected, observed, "zhixuId")
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
    out.insert("planId".to_string(), Value::String(order.plan_id.clone()));
    out.insert("zhixuId".to_string(), Value::String(order.zhixu_id.clone()));
    out.insert("orderId".to_string(), Value::String(order.order_id.clone()));
    out.insert("hookId".to_string(), Value::String(hook_id.to_string()));
    out
}

/// Plan identity is part of the chain order address. Keeping the canonical
/// `(planId, orderId)` pair in the serialized key prevents two plans for the
/// same Zhixu from overwriting or sharing signals when they reuse an order id.
fn order_key(plan_id: &str, order_id: &str) -> String {
    format!("{plan_id}::{order_id}")
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
    fn or_merge_keeps_earliest_anchor() {
        let left = EvalValue {
            value: true,
            wait: false,
            cancel: false,
            due_at: 0,
            anchor_at: 100,
        };
        let right = EvalValue {
            value: true,
            wait: false,
            cancel: false,
            due_at: 0,
            anchor_at: 5,
        };
        let merged = or_value(left, right);
        assert_eq!(merged.anchor_at, 5);
    }

    #[test]
    fn or_merge_ready_winner_keeps_own_anchor_without_waiting_branch() {
        // P1-5 回归：ready×wait 混合时，等待分支的陈旧锚点不得参与归约——
        // 就绪胜者自带计时器（对齐 hook-dsl Expr::Or 与合约 _orValue）。
        // 此前 oracle 取 min(1000, 10)=10，对 `(a | (b +100s)) +50s` 形态
        // 给出比合约更早的 due，产生假 mismatch。
        let ready = EvalValue {
            value: true,
            wait: false,
            cancel: false,
            due_at: 0,
            anchor_at: 1000,
        };
        let waiting = EvalValue {
            value: false,
            wait: true,
            cancel: false,
            due_at: 110,
            anchor_at: 10,
        };
        let merged = or_value(ready, waiting);
        assert!(merged.value);
        assert!(!merged.wait);
        assert_eq!(merged.anchor_at, 1000);
        let merged = or_value(waiting, ready);
        assert!(merged.value);
        assert_eq!(merged.anchor_at, 1000);
    }

    #[test]
    fn rejects_merge_instruction_as_retired() {
        // MERGE（semantic 0.6 撮合扇入）已作废：求值器不再认识该指令。
        let instructions = vec![
            json!({"op": "SIGNAL", "signalKey": "0xaa"}),
            json!({"op": "SIGNAL", "signalKey": "0xbb"}),
            json!({"op": "MERGE", "arity": 2}),
        ];
        let error = evaluate_instructions(
            &OracleOrderState::default(),
            &instructions,
            "2026-04-27T00:00:00Z",
        )
        .unwrap_err();
        assert!(error
            .to_string()
            .contains("unsupported chain-mode instruction"));
    }

    #[test]
    fn rejects_not_without_operand() {
        let instructions = vec![json!({"op": "NOT"})];
        let error = evaluate_instructions(
            &OracleOrderState::default(),
            &instructions,
            "2026-04-27T00:00:00Z",
        )
        .unwrap_err();
        assert!(error.to_string().contains("NOT requires one operand"));
    }

    #[test]
    fn rejects_arity_exceeding_stack() {
        let instructions = vec![
            json!({"op": "SIGNAL", "signalKey": "0x50"}),
            json!({"op": "AND", "arity": 2}),
        ];
        let error = evaluate_instructions(
            &OracleOrderState::default(),
            &instructions,
            "2026-04-27T00:00:00Z",
        )
        .unwrap_err();
        assert!(error.to_string().contains("requires 2 operands"));
    }

    #[test]
    fn rejects_leftover_stack_values() {
        let instructions = vec![
            json!({"op": "SIGNAL", "signalKey": "0x50"}),
            json!({"op": "SIGNAL", "signalKey": "0x51"}),
        ];
        let error = evaluate_instructions(
            &OracleOrderState::default(),
            &instructions,
            "2026-04-27T00:00:00Z",
        )
        .unwrap_err();
        assert!(error.to_string().contains("exactly one result value"));
    }

    #[test]
    fn rejects_overflowing_delay_computation() {
        let anchored = EvalValue {
            value: true,
            wait: false,
            cancel: false,
            due_at: 0,
            anchor_at: i64::MAX,
        };
        let error = delay_value(anchored, 30 * 24 * 60 * 60, "2026-04-27T00:00:00Z").unwrap_err();
        assert!(error
            .to_string()
            .contains("overflows the replay timestamp range"));
    }

    #[test]
    fn rejects_invalid_submitted_at() {
        let mut order = OracleOrderState::default();
        order.signals.insert(
            "0x50".to_string(),
            json!({"submittedAt": "not-a-timestamp"}),
        );
        let error = signal_value(&order, "0x50").unwrap_err();
        assert!(error.to_string().contains("invalid chain oracle timestamp"));
    }

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
                        "orderTriggerKind": "mint",
                        "emitReady": true,
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
                "planId": "0x01",
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
                "planId": "0x01",
                "zhixuId": "demo",
                "orderId": "order-1",
                "hookId": "0x10",
                "stageIdentifier": "flow.start",
                "hookName": "START"
            })
        );
    }

    #[test]
    fn emit_ready_is_independent_from_order_materialization() {
        let mut order = OracleOrderState {
            zhixu_id: "demo".to_string(),
            order_id: "order-1".to_string(),
            ..OracleOrderState::default()
        };
        order.signals.insert(
            "0x50".to_string(),
            json!({"submittedAt": "2026-04-27T00:00:00.000Z"}),
        );

        let silent_trigger = json!({
            "hookId": "flow.start#SILENT",
            "stageId": "flow.start",
            "stageIdentifier": "flow.start",
            "hookName": "SILENT",
            "orderTriggerKind": "mint",
            "emitReady": false,
            "instructions": [{"op": "SIGNAL", "signalKey": "0x50"}]
        });
        let trigger_observations =
            evaluate_hook(&mut order, &silent_trigger, "2026-04-27T00:00:00.000Z")
                .expect("silent trigger should evaluate");
        assert!(trigger_observations.is_empty());
        assert!(order.materialized_stages["flow.start"]);
        assert_eq!(order.hook_statuses["flow.start#SILENT"].status, "reg");
        assert!(!order.hook_statuses["flow.start#SILENT"].ready_emitted);

        let ordinary_hook = json!({
            "hookId": "flow.start#OBSERVE",
            "stageId": "flow.start",
            "stageIdentifier": "flow.start",
            "hookName": "OBSERVE",
            "orderTriggerKind": "none",
            "emitReady": true,
            "instructions": [{"op": "SIGNAL", "signalKey": "0x50"}]
        });
        let observations = evaluate_hook(&mut order, &ordinary_hook, "2026-04-27T00:00:00.000Z")
            .expect("materialized ordinary hook should evaluate");
        assert_eq!(observations.len(), 1);
        assert_eq!(observations[0]["eventName"], "HookReady");
        assert_eq!(observations[0]["hookId"], "flow.start#OBSERVE");
    }

    #[test]
    fn emit_ready_hook_materializes_stage_before_materialization() {
        // 簇 A 对齐：EMIT_READY hook 是 executor dispatch 边——阶段未物化时
        // 仍求值，Ready 时物化自身阶段并发 HookReady（普通 executor 阶段的
        // 标准形态：全部 receive hook emit-ready）。此前 oracle 对所有非
        // trigger hook 一律"未物化即跳过"，正常编译产物的回放必然 mismatch。
        let mut order = OracleOrderState {
            zhixu_id: "demo".to_string(),
            order_id: "order-1".to_string(),
            ..OracleOrderState::default()
        };
        order.signals.insert(
            "0x50".to_string(),
            json!({"submittedAt": "2026-04-27T00:00:00.000Z"}),
        );

        let emit_ready_hook = json!({
            "hookId": "flow.execute#READY",
            "stageId": "flow.execute",
            "stageIdentifier": "flow.execute",
            "hookName": "READY",
            "orderTriggerKind": "none",
            "emitReady": true,
            "instructions": [{"op": "SIGNAL", "signalKey": "0x50"}]
        });
        assert!(
            !order.materialized_stages.contains_key("flow.execute"),
            "precondition: stage not yet materialized"
        );
        let observations = evaluate_hook(&mut order, &emit_ready_hook, "2026-04-27T00:00:00.000Z")
            .expect("emit-ready hook must evaluate before stage materialization");
        assert_eq!(observations.len(), 1);
        assert_eq!(observations[0]["eventName"], "HookReady");
        assert_eq!(observations[0]["hookId"], "flow.execute#READY");
        assert!(
            order.materialized_stages["flow.execute"],
            "emit-ready readiness must materialize its own stage"
        );

        // 阶段物化后，同阶段的纯 flags=0 watcher 获得求值资格。
        let watcher = json!({
            "hookId": "flow.execute#WATCH",
            "stageId": "flow.execute",
            "stageIdentifier": "flow.execute",
            "hookName": "WATCH",
            "orderTriggerKind": "none",
            "emitReady": false,
            "instructions": [{"op": "SIGNAL", "signalKey": "0x50"}]
        });
        let watcher_observations = evaluate_hook(&mut order, &watcher, "2026-04-27T00:00:00.000Z")
            .expect("watcher must evaluate once its stage materialized");
        assert!(
            watcher_observations.is_empty(),
            "flags=0 watcher emits nothing"
        );
        assert_eq!(order.hook_statuses["flow.execute#WATCH"].status, "reg");
    }

    #[test]
    fn flags_zero_watcher_on_unmaterialized_stage_is_skipped() {
        // 防御纵深：编译器已拒绝该形态（不可物化阶段不得挂 receive hook）；
        // oracle 对漏网产物按合约 A3 修复口径跳过（不 revert、不观察）。
        let mut order = OracleOrderState {
            zhixu_id: "demo".to_string(),
            order_id: "order-1".to_string(),
            ..OracleOrderState::default()
        };
        order.signals.insert(
            "0x50".to_string(),
            json!({"submittedAt": "2026-04-27T00:00:00.000Z"}),
        );
        let watcher = json!({
            "hookId": "flow.ghost#WATCH",
            "stageId": "flow.ghost",
            "stageIdentifier": "flow.ghost",
            "hookName": "WATCH",
            "orderTriggerKind": "none",
            "emitReady": false,
            "instructions": [{"op": "SIGNAL", "signalKey": "0x50"}]
        });
        let observations = evaluate_hook(&mut order, &watcher, "2026-04-27T00:00:00.000Z")
            .expect("unmaterialized flags=0 watcher must be skipped, not an error");
        assert!(observations.is_empty());
        assert!(!order.hook_statuses.contains_key("flow.ghost#WATCH"));
    }

    #[test]
    fn stage_materialized_event_backfills_materialization() {
        // StageMaterialized 事件被消费：链上物化事实回填 oracle 状态，后续
        // 依赖该阶段的 watcher 求值据此放行。
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
                        "hookId": "flow.exec#WATCH",
                        "stageId": "flow.exec",
                        "stageIdentifier": "flow.exec",
                        "hookName": "WATCH",
                        "orderTriggerKind": "none",
                        "emitReady": false,
                        "instructions": [{"op": "SIGNAL", "signalKey": "0x50"}]
                    }],
                    "dependencyIndex": { "0x50": ["flow.exec#WATCH"] }
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
                "eventName": "StageMaterialized",
                "blockNumber": 3,
                "logIndex": 0,
                "transactionHash": "0x03",
                "planId": "0x01",
                "orderId": "order-1",
                "stageId": "flow.exec",
                "triggerHookId": "flow.exec#BIRTH",
                "sourceId": "0x30",
                "signalId": "0x40"
            }),
            json!({
                "eventName": "SignalSubmitted",
                "blockNumber": 4,
                "logIndex": 0,
                "transactionHash": "0x04",
                "planId": "0x01",
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
            result["state"]["orders"]["0x01::order-1"]["hookStatuses"]["flow.exec#WATCH"]["status"],
            "reg",
            "watcher must evaluate after the chain-emitted StageMaterialized"
        );
    }

    #[test]
    fn legacy_is_trigger_hooks_are_rejected() {
        // legacy 清零：v1 isTrigger 产物不再兼容读取，缺失 v2 字段即错误。
        let mut order = OracleOrderState {
            zhixu_id: "demo".to_string(),
            order_id: "order-1".to_string(),
            ..OracleOrderState::default()
        };
        order.signals.insert(
            "0x50".to_string(),
            json!({"submittedAt": "2026-04-27T00:00:00.000Z"}),
        );
        let legacy_hook = json!({
            "hookId": "flow.start#LEGACY",
            "stageId": "flow.start",
            "stageIdentifier": "flow.start",
            "hookName": "LEGACY",
            "isTrigger": true,
            "instructions": [{"op": "SIGNAL", "signalKey": "0x50"}]
        });
        let error = evaluate_hook(&mut order, &legacy_hook, "2026-04-27T00:00:00.000Z")
            .expect_err("legacy isTrigger hook must be rejected");
        assert!(
            error.to_string().contains("retired"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn replay_scopes_same_order_id_by_plan() {
        let mut events = Vec::new();
        for (index, plan_id) in ["plan-a", "plan-b"].iter().enumerate() {
            let block = (index * 3 + 1) as i64;
            events.push(json!({
                "eventName": "PlanRegistered",
                "blockNumber": block,
                "logIndex": 0,
                "transactionHash": format!("0xplan{index}"),
                "plan": {
                    "planId": plan_id,
                    "zhixuId": "same-zhixu",
                    "compiledHooks": [{
                        "hookId": "flow.start#READY",
                        "stageId": "flow.start",
                        "stageIdentifier": "flow.start",
                        "hookName": "READY",
                        "orderTriggerKind": "mint",
                        "emitReady": true,
                        "instructions": [{"op": "SIGNAL", "signalKey": "0x50"}]
                    }],
                    "dependencyIndex": {"0x50": ["flow.start#READY"]}
                }
            }));
            events.push(json!({
                "eventName": "OrderRegistered",
                "blockNumber": block + 1,
                "logIndex": 0,
                "transactionHash": format!("0xorder{index}"),
                "planId": plan_id,
                "zhixuId": "same-zhixu",
                "orderId": "reused-order",
                "registeredAt": "2026-04-27T00:00:00.000Z"
            }));
            events.push(json!({
                "eventName": "SignalSubmitted",
                "blockNumber": block + 2,
                "logIndex": 0,
                "transactionHash": format!("0xsignal{index}"),
                "planId": plan_id,
                "zhixuId": "same-zhixu",
                "orderId": "reused-order",
                "sourceId": "0x30",
                "signalId": "0x40",
                "signalKey": "0x50",
                "senderId": format!("sender-{index}"),
                "submittedAt": "2026-04-27T00:00:00.000Z"
            }));
        }

        let result = replay_chain_events(
            events,
            &ReplayOptions {
                sort: Some(true),
                strict: Some(false),
            },
        )
        .expect("plan-scoped order addresses should replay");
        let orders = result["state"]["orders"]
            .as_object()
            .expect("state orders should be an object");
        assert_eq!(orders.len(), 2);
        assert!(orders.contains_key("plan-a::reused-order"));
        assert!(orders.contains_key("plan-b::reused-order"));
        assert_eq!(result["observed"].as_array().unwrap().len(), 2);
    }
}
