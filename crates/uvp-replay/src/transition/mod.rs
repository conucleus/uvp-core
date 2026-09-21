//! 状态迁移：链上事件吸收（观察裁剪/出生推导）、信号登记触发的求值与
//! 指令栈求值器。

use serde_json::{json, Map, Value};
use std::collections::BTreeMap;

use crate::facts::{find_hook, order_key, HookRuntime, OracleOrderState, OracleState};
use crate::snapshot::{base_hook_observation, chain_event_to_expected_observation, same_due_at};
use crate::{chain_event_id, seconds_from_iso, value_i64, value_str, ReplayError, Result};

/// 链上可观察事件（HookReady/HookStatusChanged）的吸收口。真实事件流与
/// oracle 模型存在两类系统性分叉，规则如下（与 crate README 同步）：
///
/// 裁剪（trim，不进入 expected）：
/// - `HookStatusChanged(status=ready)`：合约对 →Ready 先 emit 状态变更再
///   emit HookReady；oracle 只以 HookReady 观察就绪，ready 状态变更被裁剪
///   出 expected，不参与比对。
/// - `HookStatusChanged(status=init)`：合约不产出（Init 是隐含初值，
///   无观察语义）；适配层抬升的遗留形状被裁剪，原生输入契约据此免裁剪
///   直喂。
/// - 语义重复的 `HookStatusChanged`（同 hook、同 status、同 dueAt 时刻）：
///   投影重放/重排可能重复，重复被吸收，不产生 missing-observed 假阳性。
///   dueAt 按时刻归一化比较（毫秒位数/时区写法不是语义），不同渲染的
///   同一时刻视为重复。
///
/// 推导（derive）：
/// - 合约出生路径三种：outside mint（`triggerOrderFromOutsideFor`）与 dock
///   （`createDockedOrderFromModule`）出生把事实 `_recordSignal` 落在本订单，
///   裸 atom 出生 hook 由正常求值自然产生 HookReady，无需推导；order-link
///   mint 出生（`triggerOrderFromSignalFromModule`）不 `_recordSignal` 但
///   emit HookReady。oracle 据链上 HookReady 反推的范围只覆盖 mint 标记的
///   出生 hook：补 runtime ready/readyEmitted 并物化其阶段，同时把该观察
///   记入 observed（接受链上断言——order-link 出生的事实只存在于 origin
///   订单，本订单没有可求值的信号源，链是唯一推导来源）。dock 标记的
///   出生 hook 不接受断言：dock 出生事实恒先落本订单（SignalSubmitted
///   先行），求值路径已可推导其 Ready，链上出现 oracle 未推导的 dock
///   HookReady 只能是事实缺失的异常。dock/mint 之外的无信号 HookReady
///   同样不推导，保持 mismatch 暴露真实异常。
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

/// order-link 出生推导：三种出生形态中仅 order-link mint 无法由求值推导
/// （详见 `absorb_chain_observation` 的推导规则）。推导门是 mint 标记——
/// dock 出生的事实恒先落本订单，其 Ready 可由求值推导，接受无信号断言
/// 会把事实缺失的异常吞成配对成功；plan 的 v2 结构字段缺失/非法在此
/// 响亮失败，不做"缺 orderTriggerKind 即视为非 trigger"的静默回退——
/// 那是把结构错误吞成推导缺口。
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
    // 结构字段缺失即响亮失败（与 evaluate_hook 对 stageId 的 ? 门口径
    // 一致）：unwrap_or_default 会把缺失吞成空串并物化 "" 键——畸形 plan
    // 的链上断言被静默接受，异常吞成配对成功。
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
    /// 等待期限（Wait 状态的到期时刻）。`None` = 无期限（就绪/取消/缺席
    /// 归约的产物）。显式 Option 区分"无 due"与 epoch 0——用 0 兼作哨兵
    /// 会让 iso_from_seconds(0) 丢期限、poke 资格闸把 due 判永不合资格，
    /// 且与核心求值器 waits 的 Option 口径在 due==0 边界分叉。
    pub(crate) due_at: Option<i64>,
    /// 正向锚点（事实到达时刻 / 延时到期时刻）。`None` = 无锚（缺席信号
    /// 的否定就绪等伪就绪）。显式 Option 区分"无锚"与 epoch 0——用 0 兼作
    /// 哨兵会让 epoch 0 提交的事实被 DELAY 误报结构性错误。
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
    // 出生通道判别（镜像合约 _recordSignal 的 evaluateOrderTriggerHooks
    // 标志）：出生事实写入只发生在订单创建事务内——outside 出生
    // （OrderTriggered 与出生 _recordSignal 同事务）与 dock 出生（无
    // OrderTriggered，entrance 事实是本订单首条信号）；order-link 出生不
    // _recordSignal，其 OrderTriggered 事务内没有信号，订单后续收到的第
    // 一条普通信号不得误判为出生通道。普通信号提交（含 dock input 模块
    // 写/回写/派生写回）一律不是出生通道。
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
    // 与合约 `_evaluateAffectedHooks` 相同的两遍顺序：先 trigger 后普通
    // watcher，保证出生/物化边先于同键观察者求值。出生求值范围同口径
    // （evaluateOrderTriggerHooks）：order-trigger（mint/dock）hook 只在
    // 出生事务内求值——普通信号不再推进它们，否则订单 Y（由事实 K2 铸
    // 出）内普通提交另一出生线事实 K1 会被 oracle 推 Ready 并物化 Y 并
    // 未由此出生的阶段，而链上已不再这么做（凭空 observed）。watcher
    // 两条路径都照常求值。
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

/// hook-plan v2 的出生标记字段是 `orderTriggerKind`(mint|dock|none) 加
/// `emitReady`。oracle 只认 v2 字段，缺失即结构性错误
/// （fail-closed，不做隐式回退）。
pub(crate) fn hook_is_order_trigger(hook: &Value) -> Result<bool> {
    Ok(matches!(order_trigger_kind(hook)?, "mint" | "dock"))
}

/// 出生推导按 kind 精确分流（只有 mint 接受链上断言），两处共用同一
/// 提取入口，缺失/非法值都报结构错误。
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
    // 阶段物化三线统一：`orderTriggerKind` 与 `emitReady` hook 都
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
                // 合约编码门（_validateHook）：AND/OR 的 arity ≥ 2
                // （k=1 观察入口是 cloud 运行时投递形态，链上无对应物，
                // 编码层即拒绝）。解码层同口径拒绝 k=1。
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
            // 求值器只认冻结指令集（SIGNAL/NOT/AND/OR/DELAY）；其他
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
    if value.value || value.wait {
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
    // 回放解码镜像合约注册门（UVPPlanRegistration._validateHook 对
    // delaySeconds == 0 revert InvalidInstruction；链上 uint64 无负值，回放
    // 的手工 plan 是 i64，≤0 一并拒绝）：负值会把锚点回拨、0 值恒等，
    // 都是确定性非法输入，不做宽容解释。在线入口（uvp-hook-dsl 解码层
    // "duration must be positive"）同口径。
    if delay_seconds <= 0 {
        return Err(ReplayError::Message(
            "malformed instruction plan: DELAY delaySeconds must be positive".to_string(),
        ));
    }
    if value.cancel || !value.value {
        return Ok(value);
    }
    // 锚点不变量复验（对齐合约 _validateHook 注册门的 hasPosAnchor 检查
    // 与 DSL validate_anchors）：就绪但无锚（如 NOT(缺席信号) 产生的伪
    // 就绪）会把 due 计到 1970——手工 plan 的结构性错误在求值期响亮失败，
    // 而不是产出荒谬的 wait 观察。显式 Option 哨兵：epoch 0 提交的事实
    // 是真实锚点，不落在该拒绝面内。
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
        // 锚点推进（链式延时语义）：延时到期时刻本身成为新的锚点，
        // 使 `(A+5s)+10s` 的外层延时从 A+5s 起算，与生产求值器一致。
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
            due_at: None,
            anchor_at: max_anchor(left.anchor_at, right.anchor_at),
        };
    }
    if (left.wait && (right.value || right.wait)) || (right.wait && (left.value || left.wait)) {
        return EvalValue {
            value: false,
            wait: true,
            cancel: false,
            due_at: max_due(left.due_at, right.due_at),
            anchor_at: max_anchor(left.anchor_at, right.anchor_at),
        };
    }
    false_value()
}

pub(crate) fn or_value(left: EvalValue, right: EvalValue) -> EvalValue {
    // OR 的延时锚点取"最早成熟时刻"：纯信号
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
            due_at: None,
            anchor_at: anchor,
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

/// 可用锚点取较晚者；双侧无锚返回 None（AND 的就绪/等待归约口径）。
fn max_anchor(left: Option<i64>, right: Option<i64>) -> Option<i64> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.max(right)),
        (Some(only), None) | (None, Some(only)) => Some(only),
        (None, None) => None,
    }
}

/// 可用锚点取较早者；双侧无锚返回 None（OR 的"最早成熟时刻"口径）。
fn min_anchor(left: Option<i64>, right: Option<i64>) -> Option<i64> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.min(right)),
        (Some(only), None) | (None, Some(only)) => Some(only),
        (None, None) => None,
    }
}

/// 可用 due 取较晚者；无 due 侧（就绪/取消归约的产物）不参与（AND 等待
/// 的归约口径，与核心求值器 Expr::And 的 waits.max() 同形：waits 只收集
/// Some 的 ready_at，"无 due"不得折成 epoch 0 参与比较）。
fn max_due(left: Option<i64>, right: Option<i64>) -> Option<i64> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.max(right)),
        (Some(only), None) | (None, Some(only)) => Some(only),
        (None, None) => None,
    }
}

/// 可用 due 取较早者；无 due 侧让位（OR 等待的归约口径，与核心求值器
/// Expr::Or 的 waits.min() 同形：无 due 侧不做非零哨兵特判）。
fn min_due(left: Option<i64>, right: Option<i64>) -> Option<i64> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.min(right)),
        (Some(only), None) | (None, Some(only)) => Some(only),
        (None, None) => None,
    }
}
