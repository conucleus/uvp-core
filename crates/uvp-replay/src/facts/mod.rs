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

pub(crate) const MAX_DELAY_SECONDS: i64 = 30 * 24 * 60 * 60;

pub(crate) const MAX_INSTRUCTION_DEPTH: usize = 120;

pub(crate) fn validate_plan_registration_gates(plan: &Value) -> Result<()> {
    let hooks = plan
        .get("compiledHooks")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            ReplayError::Message("chain oracle plan missing compiledHooks".to_string())
        })?;
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
    let mut seen_hook_ids = std::collections::BTreeSet::new();
    for hook in hooks {
        let hook_id = hook.get("hookId").and_then(Value::as_str).ok_or_else(|| {
            ReplayError::Message("chain oracle plan hook missing hookId".to_string())
        })?;
        if !seen_hook_ids.insert(hook_id) {
            return Err(ReplayError::Message(format!(
                "chain oracle plan carries duplicate hookId {hook_id}: the contract reverts HookAlreadyRegistered, a plan with duplicate ids is not a contract-reachable state"
            )));
        }
    }
    if let Some(admissions) = plan.get("admissions") {
        let admissions = admissions.as_array().ok_or_else(|| {
            ReplayError::Message("chain oracle plan admissions must be an array".to_string())
        })?;
        for admission in admissions {
            validate_admission_registration_shape(admission)?;
        }
    }
    Ok(())
}

fn validate_admission_registration_shape(admission: &Value) -> Result<()> {
    let label = admission
        .get("admissionId")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| {
            format!(
                "{}.{}",
                admission
                    .get("stageIdentifier")
                    .and_then(Value::as_str)
                    .unwrap_or("<unknown-stage>"),
                admission
                    .get("signalName")
                    .and_then(Value::as_str)
                    .unwrap_or("<unknown-signal>")
            )
        });
    let instructions = admission
        .get("instructions")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            ReplayError::Message(format!(
                "chain oracle admission {label} missing instructions (the onchain admission track is produced by the TS compiler; a plan compiled by the Rust core does not carry it and cannot be replayed)"
            ))
        })?;
    if instructions.is_empty() {
        return Err(ReplayError::Message(format!(
            "malformed instruction plan: admission {label} carries no instructions (contract commitPlan reverts InvalidHook)"
        )));
    }
    struct AdmissionSlot {
        bare_signal: bool,
        delay_result: bool,
        depth: usize,
    }
    let mut stack: Vec<AdmissionSlot> = Vec::new();
    for instruction in instructions {
        match value_str(instruction, "op")? {
            "SIGNAL" => stack.push(AdmissionSlot {
                bare_signal: true,
                delay_result: false,
                depth: 0,
            }),
            "NOT" => {
                let Some(slot) = stack.pop() else {
                    return Err(ReplayError::Message(
                        "malformed instruction plan: NOT requires one operand on the stack"
                            .to_string(),
                    ));
                };
                if !slot.bare_signal && !slot.delay_result {
                    return Err(ReplayError::Message(format!(
                        "malformed instruction plan: admission {label} applies NOT to a non-bare-SIGNAL operand (composite negation diverges from compiler-produced shapes; contract _validateAdmission reverts InvalidInstruction)"
                    )));
                }
                stack.push(AdmissionSlot {
                    bare_signal: false,
                    delay_result: false,
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
                stack.push(AdmissionSlot {
                    bare_signal: false,
                    delay_result: true,
                    depth: slot.depth + 1,
                });
            }
            "AND" | "OR" => {
                let op_name = value_str(instruction, "op")?;
                let arity = value_i64(instruction, "arity")?;
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
                let depth = terms.iter().map(|term| term.depth).max().unwrap_or(0) + 1;
                stack.push(AdmissionSlot {
                    bare_signal: false,
                    delay_result: false,
                    depth,
                });
            }
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
                "malformed instruction plan: admission {label} instruction nesting exceeds the maximum depth of {MAX_INSTRUCTION_DEPTH}"
            )));
        }
    }
    if stack.len() != 1 {
        return Err(ReplayError::Message(format!(
            "malformed instruction plan: admission {label} expected exactly one result value, found {}",
            stack.len()
        )));
    }
    Ok(())
}

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

struct InstructionSlot {
    bare_signal: bool,
    delay_result: bool,
    veto_term: bool,
    veto_inside: bool,
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
                    delay_result: false,
                    veto_term: false,
                    veto_inside: false,
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
                if !slot.bare_signal && !slot.delay_result {
                    return Err(ReplayError::Message(format!(
                        "malformed instruction plan: hook {hook_id} applies NOT to a non-bare-SIGNAL operand (composite negation diverges from compiler-produced shapes; contract _validateHook reverts InvalidInstruction)"
                    )));
                }
                stack.push(InstructionSlot {
                    bare_signal: false,
                    delay_result: false,
                    veto_term: slot.delay_result,
                    veto_inside: slot.veto_inside || slot.delay_result,
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
                if slot.veto_term || slot.veto_inside {
                    return Err(ReplayError::Message(format!(
                        "malformed instruction plan: hook {hook_id} applies DELAY to an operand containing a decaying veto (a veto's readiness decays while delay maturity is permanent; an outer delay anchored on an expired veto would silently pass; contract _validateHook reverts InvalidInstruction)"
                    )));
                }
                stack.push(InstructionSlot {
                    bare_signal: false,
                    delay_result: true,
                    veto_term: false,
                    veto_inside: false,
                    has_pos_anchor: slot.has_pos_anchor,
                    depth: slot.depth + 1,
                });
            }
            "AND" | "OR" => {
                let op_name = value_str(instruction, "op")?;
                let is_and = op_name == "AND";
                let arity = value_i64(instruction, "arity")?;
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
                if !is_and && terms.iter().any(|term| term.veto_term) {
                    return Err(ReplayError::Message(format!(
                        "malformed instruction plan: hook {hook_id} places a decaying veto under an OR branch (a veto is only legal as a direct conjunction operand; contract _validateHook reverts InvalidInstruction)"
                    )));
                }
                let anchored = if is_and {
                    terms.iter().any(|term| term.has_pos_anchor)
                } else {
                    terms.iter().all(|term| term.has_pos_anchor)
                };
                let veto_inside_result = terms.iter().any(|term| term.veto_inside);
                let depth = terms.iter().map(|term| term.depth).max().unwrap_or(0) + 1;
                stack.push(InstructionSlot {
                    bare_signal: false,
                    delay_result: false,
                    veto_term: false,
                    veto_inside: veto_inside_result,
                    has_pos_anchor: anchored,
                    depth,
                });
            }
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
    if stack[0].veto_term {
        return Err(ReplayError::Message(format!(
            "malformed instruction plan: hook {hook_id} places a decaying veto at the root (a veto is only legal as a direct conjunction operand; contract _validateHook reverts InvalidInstruction)"
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

pub(crate) fn order_key(plan_id: &str, order_id: &str) -> String {
    format!("{plan_id}::{order_id}")
}
