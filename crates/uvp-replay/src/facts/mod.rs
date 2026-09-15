//! 事实与状态模型：oracle 的 plan/order 登记簿、信号事实存储、hook 运行
//! 时状态与 plan 注册门。

use serde_json::{json, Map, Value};
use std::collections::BTreeMap;

use crate::transition::hook_is_order_trigger;
use crate::{value_str, ReplayError, Result};

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

/// 合约注册门镜像（UVPStateMachine._validateHook）：order-trigger
/// （mint/dock）hook 禁 DELAY——出生事实与订单创建同笔交易（anchorAt=
/// now），Delay(SIGNAL) 必得 Wait，出生路径永久 InvalidTriggerHook；dock
/// entrance 由模块直接标记 Ready，DELAY 只是死代码。该形态在合约
/// commitPlan 边界 revert InvalidInstruction（TS 编译器产出侧同口径
/// 拒绝），链上不可注册——回放输入携带即结构性错误，不产"部分观察 +
/// mismatch"的软化报告。
pub(crate) fn validate_plan_registration_gates(plan: &Value) -> Result<()> {
    let hooks = plan
        .get("compiledHooks")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            ReplayError::Message("chain oracle plan missing compiledHooks".to_string())
        })?;
    for hook in hooks {
        if !hook_is_order_trigger(hook)? {
            continue;
        }
        let instructions = hook
            .get("instructions")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                ReplayError::Message(format!(
                    "chain oracle hook {} missing instructions",
                    value_str(hook, "hookId").unwrap_or("<unknown>")
                ))
            })?;
        let carries_delay = instructions
            .iter()
            .any(|instruction| instruction.get("op").and_then(Value::as_str) == Some("DELAY"));
        if carries_delay {
            return Err(ReplayError::Message(format!(
                "malformed instruction plan: order-trigger hook {} must not contain DELAY (birth facts settle at order creation; contract _validateHook reverts InvalidInstruction)",
                value_str(hook, "hookId").unwrap_or("<unknown>")
            )));
        }
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
