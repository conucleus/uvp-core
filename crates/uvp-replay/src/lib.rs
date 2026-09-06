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
    let mut last_status_observations: BTreeMap<String, (String, Option<String>)> = BTreeMap::new();

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

/// 链上可观察事件（HookReady/HookStatusChanged）的吸收口。真实事件流与
/// oracle 模型存在两类系统性分叉，规则如下（与 crate README 同步）：
///
/// 裁剪（trim）：
/// - `HookStatusChanged(status=ready)`：合约对 →Ready 先 emit 状态变更再
///   emit HookReady；oracle 只以 HookReady 观察就绪，ready 状态变更被裁剪
///   出 expected，不参与比对。
/// - 完全重复的 `HookStatusChanged`（同 hook、同 status、同 dueAt）：投影
///   重放/重排可能逐字重复，重复被吸收，不产生 missing-observed 假阳性。
///
/// 推导（derive）：
/// - 合约出生路径三种：outside mint（`triggerOrderFromOutsideFor`）与 dock
///   （`createDockedOrderFromModule`）出生把事实 `_recordSignal` 落在本订单，
///   裸 atom 出生 hook 由正常求值自然产生 HookReady，无需推导；order-link
///   mint 出生（`triggerOrderFromSignalFromModule`）不 `_recordSignal` 但
///   emit HookReady。oracle 据链上 HookReady 反推：order-trigger hook 补
///   runtime ready/readyEmitted 并物化其阶段，同时把该观察记入 observed
///   （接受链上断言）。非 trigger hook 的无信号 HookReady 不推导，保持
///   mismatch 暴露真实异常。
fn absorb_chain_observation(
    state: &mut OracleState,
    expected: &mut Vec<Value>,
    observed: &mut Vec<Value>,
    last_status: &mut BTreeMap<String, (String, Option<String>)>,
    event: &Value,
) -> Result<()> {
    match value_str(event, "eventName")? {
        "HookReady" => {
            expected.push(chain_event_to_expected_observation(event)?);
            derive_order_link_birth(state, observed, event);
            Ok(())
        }
        "HookStatusChanged" => {
            let status = value_str(event, "status")?.to_string();
            if status == "ready" {
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
                    *previous_status == status && *previous_due == due_at
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

/// order-link 出生推导：三种出生形态中仅 order-link mint 无法由求值推导
/// （详见 `absorb_chain_observation` 的推导规则）。
fn derive_order_link_birth(state: &mut OracleState, observed: &mut Vec<Value>, event: &Value) {
    let (Ok(plan_id), Ok(order_id), Ok(hook_id)) = (
        value_str(event, "planId"),
        value_str(event, "orderId"),
        value_str(event, "hookId"),
    ) else {
        return;
    };
    if !state.orders.contains_key(&order_key(plan_id, order_id)) {
        return;
    }
    let Some(plan) = state.plans.get(plan_id) else {
        return;
    };
    let Ok(hook) = find_hook(plan, hook_id) else {
        return;
    };
    // 仅出生边（order-trigger/mint）接受链上断言：dock 出生与普通就绪都能
    // 由本订单已记录事实推导，无信号的 HookReady 对它们仍是真异常。
    if !hook_is_order_trigger(&hook).unwrap_or(false) {
        return;
    }
    let stage_id = value_str(&hook, "stageId").unwrap_or_default().to_string();
    let stage_identifier = value_str(&hook, "stageIdentifier")
        .unwrap_or_default()
        .to_string();
    let hook_name = value_str(&hook, "hookName").unwrap_or_default().to_string();
    let Some(order) = state.orders.get_mut(&order_key(plan_id, order_id)) else {
        return;
    };
    let mut runtime = order
        .hook_statuses
        .remove(hook_id)
        .unwrap_or_else(HookRuntime::init);
    if runtime.ready_emitted {
        order.hook_statuses.insert(hook_id.to_string(), runtime);
        return;
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
    let hook_id = value_str(event, "hookId")?;
    let hook = find_hook(&plan, hook_id)?;
    let poked_at = value_str(event, "pokedAt")?;
    // 对齐合约 pokeTimer 的两道门（TimerNotWaiting/TimerNotDue 在合约侧是
    // revert，真实事件流不会携带越权 poke；oracle 的自推导状态若尚未
    // wait 或未到期，直接重评只会凭空产生观察）——未到期/非等待的 poke
    // 跳过，不产生 unexpected-observed 假阳性。
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

/// hook-plan v2 把单一 `isTrigger` 拆成 `orderTriggerKind`(mint|dock|none)
/// 加 `emitReady`（PRD94 §3.4）。oracle 只认 v2 字段，缺失即结构性错误
/// （fail-closed，不做隐式回退）。
fn hook_is_order_trigger(hook: &Value) -> Result<bool> {
    let kind = hook
        .get("orderTriggerKind")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            ReplayError::Message(format!(
                "hook {} must carry an orderTriggerKind field",
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
/// order/stage materialization. v2 artifacts must carry the boolean; absence
/// is a structural error, not an implied default.
fn hook_emits_ready(hook: &Value) -> Result<bool> {
    match hook.get("emitReady") {
        Some(Value::Bool(value)) => Ok(*value),
        _ => Err(ReplayError::Message(format!(
            "hook {} emitReady must be a boolean",
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
    if previous.status == "cxl" || previous.status == "ready" {
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
    // 阶段物化三线统一（簇 A）：`orderTriggerKind` 与 `emitReady` hook 都
    // 物化自身阶段——前者是出生边，后者是 executor dispatch 边（合约
    // `_evaluateHook` 的 EMIT_READY 分支同样调用 _materializeStage）。仅
    // emitReady=false 的沉默 trigger 物化但不发 HookReady；该形态已被
    // UVPStateMachine commitPlan 注册守卫（SilentOrderTriggerHook）拒绝，
    // 编译器产物恒为 trigger|EMIT_READY，此分支只是防御性死代码。
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
            "AND" | "OR" | "MERGE" => {
                let op_name = value_str(instruction, "op")?;
                let is_and = op_name == "AND";
                let is_merge = op_name == "MERGE";
                let arity = value_i64(instruction, "arity")?;
                // 合约编码门（_validateHook）：AND/OR/MERGE 的 arity ≥ 2
                // （MERGE 的 k=1 观察入口是 cloud 运行时投递形态，链上无
                // 对应物，编码层即拒绝）。解码层同口径拒绝 k=1。
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
                    } else if is_merge {
                        merge_value(left, right)
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
            // 求值器只认冻结指令集（SIGNAL/NOT/AND/OR/DELAY/MERGE）；其他
            // 指令一律 unsupported（合约侧编码校验同样不为其发放合法生产者）。
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

fn merge_value(left: EvalValue, right: EvalValue) -> EvalValue {
    // 撮合扇入（合约 _mergeValue 逐字对齐）：任一路贡献信号在场即就绪，
    // 锚点取在场分支中最早到达（先到因果）；无等待/取消分支——合约编码层
    // 约束操作数必须是裸 SIGNAL 引用，wait/cancel 不可达，按缺席处理。
    // 权威 DSL（uvp.semantic.v1）已退役 MERGE 表达式语法，官方编译器不产出
    // 该指令；合约仍接受手工 plan 的 op=Merge，oracle 必须同口径求值。
    if left.value && right.value {
        return EvalValue {
            value: true,
            wait: false,
            cancel: false,
            due_at: 0,
            anchor_at: min_anchor(left.anchor_at, right.anchor_at),
        };
    }
    if left.value {
        return EvalValue {
            value: true,
            wait: false,
            cancel: false,
            due_at: 0,
            anchor_at: left.anchor_at,
        };
    }
    if right.value {
        return EvalValue {
            value: true,
            wait: false,
            cancel: false,
            due_at: 0,
            anchor_at: right.anchor_at,
        };
    }
    false_value()
}

fn or_value(left: EvalValue, right: EvalValue) -> EvalValue {
    // OR 的延时锚点取"最早成熟时刻"（semantic 0.5 / 簇 B 裁定）：纯信号
    // 分支在到达时刻成熟，复合分支在自身成熟时刻成熟（如 AND 取操作数
    // 的 max）。只有 READY 的分支才有资格竞争锚点；等待分支的陈旧锚点
    // 不得获胜——就绪胜者保留自己的计时。与核心求值器（uvp-hook-dsl
    // Expr::Or）及合约 _orValue 对齐。
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
    fn rejects_unknown_instruction() {
        // 未知指令一律 unsupported：求值器只认冻结指令集（SIGNAL/NOT/AND/OR/DELAY/MERGE）。
        let instructions = vec![
            json!({"op": "SIGNAL", "signalKey": "0xaa"}),
            json!({"op": "SIGNAL", "signalKey": "0xbb"}),
            json!({"op": "FANIN", "arity": 2}),
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
    fn merge_instruction_matches_contract_semantics() {
        // 合约 op=Merge 求值对齐：任一在场分支即就绪，锚点取在场分支最早
        // 到达；全部缺席则未就绪。k=1（arity<2）按合约编码门拒绝。
        let mut order = OracleOrderState::default();
        order.signals.insert(
            "0x50".to_string(),
            json!({"submittedAt": "2026-04-27T00:00:30.000Z"}),
        );
        order.signals.insert(
            "0x51".to_string(),
            json!({"submittedAt": "2026-04-27T00:00:10.000Z"}),
        );
        let instructions = vec![
            json!({"op": "SIGNAL", "signalKey": "0x50"}),
            json!({"op": "SIGNAL", "signalKey": "0x51"}),
            json!({"op": "MERGE", "arity": 2}),
        ];
        let merged = evaluate_instructions(&order, &instructions, "2026-04-27T00:01:00Z").unwrap();
        assert!(merged.value);
        assert_eq!(
            merged.anchor_at,
            seconds_from_iso("2026-04-27T00:00:10Z").unwrap()
        );

        let mut partial = OracleOrderState::default();
        partial.signals.insert(
            "0x50".to_string(),
            json!({"submittedAt": "2026-04-27T00:00:30.000Z"}),
        );
        let merged =
            evaluate_instructions(&partial, &instructions, "2026-04-27T00:01:00Z").unwrap();
        assert!(merged.value);
        assert_eq!(
            merged.anchor_at,
            seconds_from_iso("2026-04-27T00:00:30Z").unwrap()
        );

        let absent = evaluate_instructions(
            &OracleOrderState::default(),
            &instructions,
            "2026-04-27T00:01:00Z",
        )
        .unwrap();
        assert!(!absent.value && !absent.wait && !absent.cancel);

        let unary = vec![
            json!({"op": "SIGNAL", "signalKey": "0x50"}),
            json!({"op": "MERGE", "arity": 1}),
        ];
        let error = evaluate_instructions(&order, &unary, "2026-04-27T00:01:00Z").unwrap_err();
        assert!(
            error.to_string().contains("arity must be at least 2"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn merge_hook_replays_with_earliest_anchor_golden() {
        // golden：手工 plan 的 MERGE 出生 hook（官方编译器不产出），两路
        // 事实先后到达，回放观察到 HookReady 且链上期望逐一对齐。
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
                        "hookId": "match.exchange#PAIR",
                        "stageId": "match.exchange",
                        "stageIdentifier": "match.exchange",
                        "hookName": "PAIR",
                        "orderTriggerKind": "mint",
                        "emitReady": true,
                        "instructions": [
                            {"op": "SIGNAL", "signalKey": "0x50"},
                            {"op": "SIGNAL", "signalKey": "0x51"},
                            {"op": "MERGE", "arity": 2}
                        ]
                    }],
                    "dependencyIndex": { "0x50": ["match.exchange#PAIR"], "0x51": ["match.exchange#PAIR"] }
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
                "senderId": "seller",
                "submittedAt": "2026-04-27T00:00:30.000Z"
            }),
            json!({
                "eventName": "HookStatusChanged",
                "blockNumber": 3,
                "logIndex": 1,
                "transactionHash": "0x03",
                "planId": "0x01",
                "zhixuId": "demo",
                "orderId": "order-1",
                "hookId": "match.exchange#PAIR",
                "status": "ready"
            }),
            json!({
                "eventName": "HookReady",
                "blockNumber": 3,
                "logIndex": 2,
                "transactionHash": "0x03",
                "planId": "0x01",
                "zhixuId": "demo",
                "orderId": "order-1",
                "hookId": "match.exchange#PAIR",
                "stageIdentifier": "match.exchange",
                "hookName": "PAIR"
            }),
        ];
        let result = replay_chain_events(
            events,
            &ReplayOptions {
                sort: None,
                strict: Some(true),
            },
        )
        .unwrap();
        assert_eq!(
            result["mismatches"].as_array().map(Vec::len),
            Some(0),
            "merge golden must replay without mismatch"
        );
        assert_eq!(result["observed"].as_array().map(Vec::len), Some(1));
        assert_eq!(result["observed"][0]["eventName"], "HookReady");
        assert_eq!(
            result["state"]["orders"]["0x01::order-1"]["hookStatuses"]["match.exchange#PAIR"]
                ["status"],
            "ready"
        );
    }

    #[test]
    fn ready_status_changes_and_duplicates_are_absorbed() {
        // 真实事件流：合约对 →Ready 先发 HookStatusChanged(ready) 再发
        // HookReady；逐字重复的 wait 状态变更被吸收——两者都不得产生
        // missing-observed 假阳性。
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
                        "hookId": "flow.start#START",
                        "stageId": "flow.start",
                        "stageIdentifier": "flow.start",
                        "hookName": "START",
                        "orderTriggerKind": "mint",
                        "emitReady": true,
                        "instructions": [{"op": "SIGNAL", "signalKey": "0x50"}]
                    }],
                    "dependencyIndex": { "0x50": ["flow.start#START"] }
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
            json!({
                "eventName": "HookStatusChanged",
                "blockNumber": 3,
                "logIndex": 1,
                "transactionHash": "0x03",
                "planId": "0x01",
                "zhixuId": "demo",
                "orderId": "order-1",
                "hookId": "flow.start#START",
                "status": "ready"
            }),
            json!({
                "eventName": "HookReady",
                "blockNumber": 3,
                "logIndex": 2,
                "transactionHash": "0x03",
                "planId": "0x01",
                "zhixuId": "demo",
                "orderId": "order-1",
                "hookId": "flow.start#START",
                "stageIdentifier": "flow.start",
                "hookName": "START"
            }),
            json!({
                "eventName": "HookStatusChanged",
                "blockNumber": 4,
                "logIndex": 0,
                "transactionHash": "0x04",
                "planId": "0x01",
                "zhixuId": "demo",
                "orderId": "order-1",
                "hookId": "flow.start#START",
                "status": "wait",
                "dueAt": "2026-04-27T00:00:05.000Z"
            }),
            json!({
                "eventName": "HookStatusChanged",
                "blockNumber": 5,
                "logIndex": 0,
                "transactionHash": "0x05",
                "planId": "0x01",
                "zhixuId": "demo",
                "orderId": "order-1",
                "hookId": "flow.start#START",
                "status": "wait",
                "dueAt": "2026-04-27T00:00:05.000Z"
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
        // ready 状态变更被裁剪、重复 wait 被吸收：expected 只剩 HookReady 与
        // 首条 wait。
        let expected = result["expected"].as_array().unwrap();
        assert_eq!(expected.len(), 2, "expected: {expected:?}");
        assert_eq!(expected[0]["eventName"], "HookReady");
        assert_eq!(expected[1]["eventName"], "HookStatusChanged");
        assert_eq!(expected[1]["status"], "wait");
    }

    #[test]
    fn order_link_birth_is_derived_from_hook_ready() {
        // order-link 出生（triggerOrderFromSignalFromModule）：链上不
        // _recordSignal 但 emit HookReady。oracle 从 HookReady 推导出生
        // （runtime ready + 阶段物化），并接受链上断言的观察。
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
                        "hookId": "linked.entry#BIRTH",
                        "stageId": "linked.entry",
                        "stageIdentifier": "linked.entry",
                        "hookName": "BIRTH",
                        "orderTriggerKind": "mint",
                        "emitReady": true,
                        "instructions": [{"op": "SIGNAL", "signalKey": "0x50"}]
                    }],
                    "dependencyIndex": { "0x50": ["linked.entry#BIRTH"] }
                }
            }),
            json!({
                "eventName": "OrderRegistered",
                "blockNumber": 2,
                "logIndex": 0,
                "transactionHash": "0x02",
                "planId": "0x01",
                "zhixuId": "demo",
                "orderId": "order-7",
                "registeredAt": "2026-04-27T00:00:00.000Z"
            }),
            json!({
                "eventName": "OrderTriggered",
                "blockNumber": 3,
                "logIndex": 0,
                "transactionHash": "0x03",
                "planId": "0x01",
                "orderId": "order-7"
            }),
            json!({
                "eventName": "HookReady",
                "blockNumber": 3,
                "logIndex": 1,
                "transactionHash": "0x03",
                "planId": "0x01",
                "zhixuId": "demo",
                "orderId": "order-7",
                "hookId": "linked.entry#BIRTH",
                "stageIdentifier": "linked.entry",
                "hookName": "BIRTH"
            }),
        ];
        let result = replay_chain_events(
            events,
            &ReplayOptions {
                sort: None,
                strict: Some(true),
            },
        )
        .unwrap();
        assert_eq!(
            result["mismatches"].as_array().map(Vec::len),
            Some(0),
            "order-link birth must replay without mismatch"
        );
        let order = &result["state"]["orders"]["0x01::order-7"];
        assert_eq!(
            order["hookStatuses"]["linked.entry#BIRTH"]["status"],
            "ready"
        );
        assert_eq!(
            order["hookStatuses"]["linked.entry#BIRTH"]["readyEmitted"],
            true
        );
        assert_eq!(order["materializedStages"]["linked.entry"], true);
    }

    #[test]
    fn poke_before_due_or_not_waiting_is_skipped() {
        // 对齐合约 pokeTimer：非 wait / 未到期的 poke 直接跳过，不产生
        // unexpected-observed 假阳性；到期后重评照常发生。
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
                        "hookId": "flow.pay#TIMEOUT",
                        "stageId": "flow.pay",
                        "stageIdentifier": "flow.pay",
                        "hookName": "TIMEOUT",
                        "orderTriggerKind": "none",
                        "emitReady": true,
                        "instructions": [
                            {"op": "SIGNAL", "signalKey": "0x50"},
                            {"op": "DELAY", "delaySeconds": 10}
                        ]
                    }],
                    "dependencyIndex": { "0x50": ["flow.pay#TIMEOUT"] }
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
                "stageId": "flow.pay"
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
            json!({
                "eventName": "HookStatusChanged",
                "blockNumber": 4,
                "logIndex": 1,
                "transactionHash": "0x04",
                "planId": "0x01",
                "zhixuId": "demo",
                "orderId": "order-1",
                "hookId": "flow.pay#TIMEOUT",
                "status": "wait",
                "dueAt": "2026-04-27T00:00:10.000Z"
            }),
            json!({
                "eventName": "TimerPoked",
                "blockNumber": 5,
                "logIndex": 0,
                "transactionHash": "0x05",
                "planId": "0x01",
                "zhixuId": "demo",
                "orderId": "order-1",
                "hookId": "flow.pay#TIMEOUT",
                "pokedAt": "2026-04-27T00:00:05.000Z"
            }),
            json!({
                "eventName": "TimerPoked",
                "blockNumber": 6,
                "logIndex": 0,
                "transactionHash": "0x06",
                "planId": "0x01",
                "zhixuId": "demo",
                "orderId": "order-1",
                "hookId": "flow.pay#TIMEOUT",
                "pokedAt": "2026-04-27T00:00:11.000Z"
            }),
            json!({
                "eventName": "HookStatusChanged",
                "blockNumber": 6,
                "logIndex": 1,
                "transactionHash": "0x06",
                "planId": "0x01",
                "zhixuId": "demo",
                "orderId": "order-1",
                "hookId": "flow.pay#TIMEOUT",
                "status": "ready"
            }),
            json!({
                "eventName": "HookReady",
                "blockNumber": 6,
                "logIndex": 2,
                "transactionHash": "0x06",
                "planId": "0x01",
                "zhixuId": "demo",
                "orderId": "order-1",
                "hookId": "flow.pay#TIMEOUT",
                "stageIdentifier": "flow.pay",
                "hookName": "TIMEOUT"
            }),
        ];
        let result = replay_chain_events(
            events,
            &ReplayOptions {
                sort: None,
                strict: Some(true),
            },
        )
        .unwrap();
        let observed = result["observed"].as_array().unwrap();
        // 首次观察是 wait（信号到达），未到期 poke 不产生任何观察，到期
        // poke 产生 HookReady——共两条。
        assert_eq!(observed.len(), 2, "observed: {observed:?}");
        assert_eq!(observed[0]["eventName"], "HookStatusChanged");
        assert_eq!(observed[0]["status"], "wait");
        assert_eq!(observed[1]["eventName"], "HookReady");
        assert_eq!(result["mismatches"].as_array().map(Vec::len), Some(0));
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
        assert_eq!(order.hook_statuses["flow.start#SILENT"].status, "ready");
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
        assert_eq!(order.hook_statuses["flow.execute#WATCH"].status, "ready");
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
            "ready",
            "watcher must evaluate after the chain-emitted StageMaterialized"
        );
    }

    #[test]
    fn hooks_missing_required_v2_fields_are_rejected() {
        // fail-closed：缺失 v2 必需字段（orderTriggerKind / emitReady）即
        // 结构性错误，不做隐式回退。
        let mut order = OracleOrderState {
            zhixu_id: "demo".to_string(),
            order_id: "order-1".to_string(),
            ..OracleOrderState::default()
        };
        order.signals.insert(
            "0x50".to_string(),
            json!({"submittedAt": "2026-04-27T00:00:00.000Z"}),
        );
        let hook_missing_kind = json!({
            "hookId": "flow.start#BARE",
            "stageId": "flow.start",
            "stageIdentifier": "flow.start",
            "hookName": "BARE",
            "emitReady": true,
            "isTrigger": true,
            "instructions": [{"op": "SIGNAL", "signalKey": "0x50"}]
        });
        let error = evaluate_hook(&mut order, &hook_missing_kind, "2026-04-27T00:00:00.000Z")
            .expect_err("hook missing orderTriggerKind must be rejected");
        assert!(
            error.to_string().contains("orderTriggerKind"),
            "unexpected error: {error}"
        );

        let hook_missing_emit_ready = json!({
            "hookId": "flow.start#BARE",
            "stageId": "flow.start",
            "stageIdentifier": "flow.start",
            "hookName": "BARE",
            "orderTriggerKind": "mint",
            "isTrigger": true,
            "instructions": [{"op": "SIGNAL", "signalKey": "0x50"}]
        });
        let error = evaluate_hook(
            &mut order,
            &hook_missing_emit_ready,
            "2026-04-27T00:00:00.000Z",
        )
        .expect_err("hook missing emitReady must be rejected");
        assert!(
            error.to_string().contains("emitReady must be a boolean"),
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
