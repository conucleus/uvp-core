//! 事实与状态模型：oracle 的 plan/order 登记簿、信号事实存储、hook 运行
//! 时状态与 plan 注册门。

use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet};

use crate::transition::hook_is_order_trigger;
use crate::{value_i64, value_str, ReplayError, Result};

#[derive(Default)]
pub(crate) struct OracleState {
    pub(crate) plans: BTreeMap<String, Value>,
    pub(crate) orders: BTreeMap<String, OracleOrderState>,
    /// OrderTriggered 事务哈希（order_key → tx）：出生事务标记。outside
    /// 出生（`triggerOrderFromOutsideFor`）的 OrderTriggered 与出生事实
    /// `_recordSignal` 同事务；order-link 出生
    /// （`triggerOrderFromSignalFromModule`）只发 OrderTriggered +
    /// HookReady、不 `_recordSignal`——本表是两者在事件流上的判别面。
    pub(crate) order_trigger_tx: BTreeMap<String, String>,
}

#[derive(Default)]
pub(crate) struct OracleOrderState {
    pub(crate) plan_id: String,
    pub(crate) zhixu_id: String,
    pub(crate) order_id: String,
    pub(crate) signals: BTreeMap<String, Value>,
    pub(crate) hook_statuses: BTreeMap<String, HookRuntime>,
    pub(crate) materialized_stages: BTreeMap<String, bool>,
}

#[derive(Clone)]
pub(crate) struct HookRuntime {
    pub(crate) status: String,
    pub(crate) due_at: Option<String>,
    pub(crate) ready_emitted: bool,
}

impl OracleState {
    pub(crate) fn to_json(&self) -> Value {
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
    pub(crate) fn init() -> Self {
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

/// 合约延时上限镜像：UVPStateMachine.MAX_HOOK_DELAY_SECONDS = 30 days
/// （2592000s）。链上注册边界对超限 revert HookDelayTooLong，Go 解码层
/// （hookdsl validateNode）同值拒绝——oracle 的注册门同口径，超限 plan
/// 是"合约不可能的 plan"，回放响亮失败而不是产出荒谬的远期 wait。
pub(crate) const MAX_DELAY_SECONDS: i64 = 30 * 24 * 60 * 60;

/// 指令嵌套深度上限：镜像 uvp-hook-dsl MAX_PARSE_DEPTH=120（解析/求值
/// 单一深度闸）与 Go hookdsl.MaxASTDepth=120——合法编译产物不可能更深，
/// 超深指令流是毒输入，注册门拒绝（同时兜住手工 plan 的无界嵌套）。
pub(crate) const MAX_INSTRUCTION_DEPTH: usize = 120;

/// 合约注册门镜像（UVPPlanRegistration._validateHook）：回放输入里的
/// plan 必须是链上可注册的形态，否则"合约不可能的 plan"会被 oracle 以
/// 空洞观察软化成假 PASS/假 mismatch——一律结构性响亮失败：
/// - 每个钩子必须携带非空 instructions（合约 InvalidHook；也封死
///   "Rust 编译产物（无指令轨）直连回放"的空洞 PASS 断层）；
/// - order-trigger（mint/dock）hook 禁 DELAY（出生事实与订单创建同笔
///   交易，anchorAt=now，Delay(SIGNAL) 必得 Wait，出生路径永久
///   InvalidTriggerHook）；
/// - DELAY 时长 ∈ (0, 30d]（合约对 0 revert InvalidInstruction、超 30d
///   revert HookDelayTooLong）；
/// - DELAY 操作数须含正向信号锚点（hasPosAnchor 栈标志）；
/// - NOT 操作数必须裸 SIGNAL（组合否定的取消/锚点语义与编译器产物形态
///   分叉，合约注册边界拒绝）；
/// - AND/OR arity ≥ 2、栈深充足；AND 取任一正锚、OR 需每分支都有；
/// - 指令嵌套深度 ≤ 120（逐槽计数：SIGNAL=0，一元/二元组合=操作数最大
///   深度+1）；
/// - 结束时栈上恰一个值，且根含正向锚点（纯否定条件的 anchorAt 无源）；
/// - 每 hook 的 SIGNAL 原子键集合与 dependencyIndex 反查该 hook 的键集合
///   逐点一致（合约 reverts HookDependencyKeyMismatch）：oracle 的求值
///   范围由 dependencyIndex 反查决定，指令轨合法而索引错位的 plan 会让
///   observed/mismatches 全 0 仍 ok:true——空洞假 PASS 在注册期对拍拒绝；
/// - order-trigger（mint/dock）hook 必须携带 emitReady（合约 reverts
///   SilentOrderTriggerHook）：沉默 trigger 物化阶段但不发 HookReady，
///   链上链下的发出口径会分叉。
pub(crate) fn validate_plan_registration_gates(plan: &Value) -> Result<()> {
    let hooks = plan
        .get("compiledHooks")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            ReplayError::Message("chain oracle plan missing compiledHooks".to_string())
        })?;
    // dependencyIndex 是合约注册的必然产物（_registerPlanHook 逐键写入），
    // 缺失即"合约不可能的 plan"：求值范围反查会恒为空，一切信号都产生
    // 零观察——按结构错误响亮失败，不做"缺失视为空索引"的静默回退。
    let dependency_index = plan
        .get("dependencyIndex")
        .and_then(Value::as_object)
        .ok_or_else(|| {
            ReplayError::Message("chain oracle plan missing dependencyIndex".to_string())
        })?;
    for hook in hooks {
        validate_hook_registration_shape(hook)?;
        validate_hook_dependency_index_mirror(hook, dependency_index)?;
    }
    Ok(())
}

/// 镜像合约 _validateHook 的 HookDependencyKeyMismatch 门：hook 指令集的
/// SIGNAL 原子键集合与 dependencyIndex 反查该 hook 的键集合必须逐点一致。
/// 索引侧多出的键只是死索引；指令侧多出的键不进索引——该事实到达永不
/// 触发求值，hook 永久 Init 且零告警。两种错位在回放里都表现为零观察的
/// 空洞 PASS，注册期对拍拒绝。
fn validate_hook_dependency_index_mirror(
    hook: &Value,
    dependency_index: &Map<String, Value>,
) -> Result<()> {
    let hook_id = value_str(hook, "hookId")?.to_string();
    let instructions = hook
        .get("instructions")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            ReplayError::Message(format!(
                "chain oracle hook {hook_id} missing instructions (the onchain instruction track is produced by the TS compiler; a plan compiled by the Rust core does not carry it and cannot be replayed)"
            ))
        })?;
    let mut signal_keys: BTreeSet<&str> = BTreeSet::new();
    for instruction in instructions {
        if value_str(instruction, "op")? == "SIGNAL" {
            signal_keys.insert(value_str(instruction, "signalKey")?);
        }
    }
    let mut indexed_keys: BTreeSet<&str> = BTreeSet::new();
    for (key, hook_ids) in dependency_index {
        let hook_ids = hook_ids.as_array().ok_or_else(|| {
            ReplayError::Message(format!(
                "chain oracle dependencyIndex[{key}] must map to an array of hook ids"
            ))
        })?;
        if hook_ids
            .iter()
            .any(|id| id.as_str() == Some(hook_id.as_str()))
        {
            indexed_keys.insert(key);
        }
    }
    if let Some(key) = indexed_keys.difference(&signal_keys).next() {
        return Err(ReplayError::Message(format!(
            "malformed instruction plan: dependencyIndex maps key {key} to hook {hook_id} but the key is not a SIGNAL atom of its instructions (a declared key that never participates in evaluation is a dead index; contract _validateHook reverts HookDependencyKeyMismatch)"
        )));
    }
    if let Some(key) = signal_keys.difference(&indexed_keys).next() {
        return Err(ReplayError::Message(format!(
            "malformed instruction plan: hook {hook_id} references SIGNAL key {key} that dependencyIndex does not map back to it (an unindexed key never triggers evaluation, so the hook would stay Init forever with no alarm; contract _validateHook reverts HookDependencyKeyMismatch)"
        )));
    }
    Ok(())
}

/// 栈槽的注册期分析标志：_validateHook 的 bareSignal/hasPosAnchor 数组
/// 在 oracle 侧的等价物，外加逐槽嵌套深度（深度闸）。
struct InstructionSlot {
    bare_signal: bool,
    has_pos_anchor: bool,
    depth: usize,
}

fn validate_hook_registration_shape(hook: &Value) -> Result<()> {
    let hook_id = value_str(hook, "hookId").unwrap_or("<unknown>").to_string();
    let instructions = hook
        .get("instructions")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            ReplayError::Message(format!(
                "chain oracle hook {hook_id} missing instructions (the onchain instruction track is produced by the TS compiler; a plan compiled by the Rust core does not carry it and cannot be replayed)"
            ))
        })?;
    if instructions.is_empty() {
        return Err(ReplayError::Message(format!(
            "malformed instruction plan: hook {hook_id} carries no instructions (contract commitPlan reverts InvalidHook)"
        )));
    }
    let order_trigger = hook_is_order_trigger(hook)?;
    // 镜像合约 SilentOrderTriggerHook 门：order-trigger hook 必须携带
    // EMIT_READY——沉默 trigger 物化阶段但不发 HookReady，该形态的观察
    // 口径在链上链下会分叉，注册边界直接拒绝（编译器产物恒为
    // trigger|EMIT_READY，这里是"合约不可能的 plan"的防御面）。
    if order_trigger {
        match hook.get("emitReady") {
            Some(Value::Bool(true)) => {}
            _ => {
                return Err(ReplayError::Message(format!(
                    "malformed instruction plan: order-trigger hook {hook_id} must carry emitReady=true (a silent trigger materializes its stage without emitting HookReady; contract commitPlan reverts SilentOrderTriggerHook)"
                )))
            }
        }
    }
    let mut stack: Vec<InstructionSlot> = Vec::new();
    for instruction in instructions {
        match value_str(instruction, "op")? {
            "SIGNAL" => {
                stack.push(InstructionSlot {
                    bare_signal: true,
                    has_pos_anchor: true,
                    depth: 0,
                });
            }
            "NOT" => {
                let Some(slot) = stack.pop() else {
                    return Err(ReplayError::Message(
                        "malformed instruction plan: NOT requires one operand on the stack"
                            .to_string(),
                    ));
                };
                if !slot.bare_signal {
                    return Err(ReplayError::Message(format!(
                        "malformed instruction plan: hook {hook_id} applies NOT to a non-bare-SIGNAL operand (composite negation diverges from compiler-produced shapes; contract _validateHook reverts InvalidInstruction)"
                    )));
                }
                stack.push(InstructionSlot {
                    bare_signal: false,
                    has_pos_anchor: false,
                    depth: slot.depth + 1,
                });
            }
            "DELAY" => {
                let Some(slot) = stack.pop() else {
                    return Err(ReplayError::Message(
                        "malformed instruction plan: DELAY requires one operand on the stack"
                            .to_string(),
                    ));
                };
                let delay_seconds = value_i64(instruction, "delaySeconds")?;
                if delay_seconds <= 0 {
                    return Err(ReplayError::Message(
                        "malformed instruction plan: DELAY delaySeconds must be positive"
                            .to_string(),
                    ));
                }
                if delay_seconds > MAX_DELAY_SECONDS {
                    return Err(ReplayError::Message(format!(
                        "malformed instruction plan: DELAY delaySeconds {delay_seconds} exceeds the maximum allowed delay of {MAX_DELAY_SECONDS}s (30d) (contract commitPlan reverts HookDelayTooLong)"
                    )));
                }
                if order_trigger {
                    return Err(ReplayError::Message(format!(
                        "malformed instruction plan: order-trigger hook {hook_id} must not contain DELAY (birth facts settle at order creation; contract _validateHook reverts InvalidInstruction)"
                    )));
                }
                if !slot.has_pos_anchor {
                    return Err(ReplayError::Message(
                        "malformed instruction plan: DELAY requires a positively anchored operand (no positive anchor)"
                            .to_string(),
                    ));
                }
                // Delay 结果的锚点口径 = 操作数口径（成熟时刻成为新锚点，
                // 正负性随操作数）。
                stack.push(InstructionSlot {
                    bare_signal: false,
                    has_pos_anchor: slot.has_pos_anchor,
                    depth: slot.depth + 1,
                });
            }
            "AND" | "OR" => {
                let op_name = value_str(instruction, "op")?;
                let is_and = op_name == "AND";
                let arity = value_i64(instruction, "arity")?;
                // 合约编码门（_validateHook）：AND/OR 的 arity ≥ 2。
                if arity < 2 {
                    return Err(ReplayError::Message(format!(
                        "malformed instruction plan: {op_name} arity must be at least 2"
                    )));
                }
                let arity = arity as usize;
                if stack.len() < arity {
                    return Err(ReplayError::Message(format!(
                        "malformed instruction plan: {op_name} requires {arity} operands but {} remain",
                        stack.len()
                    )));
                }
                let terms = stack.split_off(stack.len() - arity);
                // And 取任一正锚，Or 需每一分支都有（Or 的缺席分支可单独
                // 就绪且锚点无源）——与 _anyPosAnchor/_allPosAnchor 同口径。
                let anchored = if is_and {
                    terms.iter().any(|term| term.has_pos_anchor)
                } else {
                    terms.iter().all(|term| term.has_pos_anchor)
                };
                let depth = terms.iter().map(|term| term.depth).max().unwrap_or(0) + 1;
                stack.push(InstructionSlot {
                    bare_signal: false,
                    has_pos_anchor: anchored,
                    depth,
                });
            }
            // 求值器只认冻结指令集（SIGNAL/NOT/AND/OR/DELAY）；注册门与
            // 求值同口径拒绝词表外操作码（合约侧编码校验同样不为其发放
            // 合法生产者）。
            other => {
                return Err(ReplayError::Message(format!(
                    "unsupported chain-mode instruction {other}"
                )))
            }
        }
        if stack
            .last()
            .is_some_and(|slot| slot.depth > MAX_INSTRUCTION_DEPTH)
        {
            return Err(ReplayError::Message(format!(
                "malformed instruction plan: hook {hook_id} instruction nesting exceeds the maximum depth of {MAX_INSTRUCTION_DEPTH}"
            )));
        }
    }
    if stack.len() != 1 {
        return Err(ReplayError::Message(format!(
            "malformed instruction plan: expected exactly one result value, found {}",
            stack.len()
        )));
    }
    if !stack[0].has_pos_anchor {
        return Err(ReplayError::Message(format!(
            "malformed instruction plan: hook {hook_id} condition has no positive signal anchor at the root (a purely negative condition has no anchor source; contract _validateHook reverts InvalidInstruction)"
        )));
    }
    Ok(())
}

pub(crate) fn find_hook(plan: &Value, hook_id: &str) -> Result<Value> {
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
                .is_some_and(|candidate| candidate == hook_id)
        })
        .cloned()
        .ok_or_else(|| ReplayError::Message(format!("chain oracle missing hook {hook_id}")))
}

/// Plan identity is part of the chain order address. Keeping the canonical
/// `(planId, orderId)` pair in the serialized key prevents two plans for the
/// same Zhixu from overwriting or sharing signals when they reuse an order id.
pub(crate) fn order_key(plan_id: &str, order_id: &str) -> String {
    format!("{plan_id}::{order_id}")
}
