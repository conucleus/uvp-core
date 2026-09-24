//! 求值：cloud AST 解码（毒产物确定性拒绝）与事实集上的表达式求值。

use chrono::{DateTime, SecondsFormat, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

use crate::ast::{
    contains_nested_subscription, is_plain_identifier, normalize_tight, valid_signal_identity,
    validate_filter_hook, validate_hook, Expr,
};
use crate::parser::{duration_to_seconds, MAX_PARSE_DEPTH};
use crate::{
    Gate, HookError, Profile, Result, CLOUD_AST_SCHEMA_VERSION, CORE_VERSION, SEMANTIC_VERSION,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EvalCompiledHookOutput {
    pub uvp_core_version: &'static str,
    pub semantic_version: &'static str,
    pub profile: Profile,
    pub state: EvalState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ready_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EvalState {
    Ready,
    Wait,
    Impossible,
    NeedsMore,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
#[serde(rename_all = "camelCase")]
pub struct EvalCompiledHookRequest {
    #[serde(default)]
    pub profile: Profile,
    /// 校验档（默认 hook，与 ParseHookRequest.gate 同先例）：解码防御按
    /// 档运行对应校验——过滤档（发射适格面）放行其合法化的形态。
    #[serde(default)]
    pub gate: Gate,
    pub ast: Value,
    #[serde(default)]
    pub signals: Vec<SignalFact>,
    pub now: String,
}

#[derive(Debug, Deserialize)]
// 事实键未知字段确定性拒绝：拼错的 source（如 sourse）不得被静默吞成
// 空 source 的"无归属事实"（那会把不匹配伪装成 ok:true needs_more）。
// 缺失 source 仍合法——空 source 是语义语料钉住的负例形态。
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SignalFact {
    #[serde(default)]
    pub source: String,
    pub signal_name: String,
    pub received_at: String,
}

pub fn eval_compiled_hook(req: EvalCompiledHookRequest) -> Result<EvalCompiledHookOutput> {
    let ast_object = req
        .ast
        .as_object()
        .ok_or_else(|| HookError::Message("compiled hook AST must be an object".to_string()))?;
    reject_unknown_keys(
        ast_object,
        &[
            "schemaVersion",
            "source",
            "mode",
            "subscriptionTarget",
            "mint",
            "route",
            "root",
        ],
        "compiled hook AST",
    )?;
    let schema_version = req
        .ast
        .get("schemaVersion")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            HookError::Message("compiled hook AST is missing schemaVersion".to_string())
        })?;
    if schema_version != CLOUD_AST_SCHEMA_VERSION {
        return Err(HookError::Message(format!(
            "unsupported compiled hook AST schemaVersion: {schema_version}"
        )));
    }
    let mode = req
        .ast
        .get("mode")
        .and_then(Value::as_str)
        .ok_or_else(|| HookError::Message("compiled hook AST is missing mode".to_string()))?;
    if !matches!(mode, "normal" | "subscription") {
        return Err(HookError::Message(format!(
            "unsupported compiled hook AST mode: {mode}"
        )));
    }
    // mint/route 是云侧编译器注入订阅 AST 的铸单/路由标注；对齐 Go
    // DecodeCompiledHook：仅 subscription 模式允许携带，mint 仅 per-fact，
    // route 仅 order/fanin（空值视为未携带）。
    let mint = optional_ast_str(ast_object, "mint")?;
    let route = optional_ast_str(ast_object, "route")?;
    match mode {
        "subscription" => {
            if !mint.is_empty() && mint != "per-fact" {
                return Err(HookError::Message(format!(
                    "compiled hook AST mint only supports per-fact: {mint}"
                )));
            }
            if !route.is_empty() && route != "order" && route != "fanin" {
                return Err(HookError::Message(format!(
                    "compiled hook AST route is invalid: {route}"
                )));
            }
        }
        _ => {
            if !mint.is_empty() || !route.is_empty() {
                return Err(HookError::Message(
                    "compiled hook AST mint/route is only allowed on subscription mode".to_string(),
                ));
            }
        }
    }
    let (target_source, target_signal) = if mode == "subscription" {
        // normal 模式不得携带 subscriptionTarget：解析器只为订阅形态产出
        // 该字段，normal 产物上出现只能是手写毒 AST。
        let target = req
            .ast
            .get("subscriptionTarget")
            .ok_or_else(|| {
                HookError::Message(
                    "compiled subscription hook AST is missing subscriptionTarget".to_string(),
                )
            })?
            .clone();
        let target_object = target.as_object().ok_or_else(|| {
            HookError::Message("compiled subscriptionTarget must be an object".to_string())
        })?;
        // 键闭集与其他子对象闸口同口径：拼错的字段（如 singal）不得被
        // 静默忽略成缺省语义（Go DecodeCompiledHook 同款拒绝）。
        reject_unknown_keys(
            target_object,
            &["source", "signal"],
            "compiled subscriptionTarget",
        )?;
        let source = target_object
            .get("source")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty());
        let signal = target_object
            .get("signal")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty());
        let (source, signal) =
            match (source, signal) {
                (Some(source), Some(signal)) => (source, signal),
                _ => return Err(HookError::Message(
                    "compiled subscription hook AST is missing subscriptionTarget.source/.signal"
                        .to_string(),
                )),
            };
        // 与 root 订阅节点（expr_from_cloud_value）同口径按原文校验：
        // trim 只用于"缺失"判定，身份闸吃原文——先 trim 再校验会把
        // " seller" 洗白成 "seller"，架空身份闸并骗过下方与 root 的
        // 一致性比对（毒身份经归一后放行）。
        if !is_plain_identifier(source) || source.len() > 36 {
            return Err(HookError::Message(format!(
                    "compiled subscriptionTarget source must be a plain identifier of at most 36 characters: {source:?}"
                )));
        }
        if !valid_signal_identity(signal) {
            return Err(HookError::Message(format!(
                    "compiled subscriptionTarget signal must use task.stage.signal and be at most 100 characters: {signal:?}"
                )));
        }
        (source.to_string(), signal.to_string())
    } else {
        if ast_object.contains_key("subscriptionTarget") {
            return Err(HookError::Message(
                "compiled normal hook AST must not carry subscriptionTarget".to_string(),
            ));
        }
        (String::new(), String::new())
    };
    let now = parse_time(&req.now, req.profile)?;
    // 订阅钩子标头恒为空：投递目标由阶段静态执行器决定，路由由接收方锚定
    // 状态与对接记录裁决，因此仅 subscription 模式允许空 source。字段类型
    // 与 mint/route 同走 optional_ast_str 纪律：在场且非字符串（含布尔/
    // 数字/对象）确定性报错，None/null 视为空——订阅模式下非字符串 source
    // 被 `and_then(as_str)` 吞成 None 再折成 "" 放行，是毒 AST 的静默通道。
    let raw_source = optional_ast_str(ast_object, "source")?;
    let source = match mode {
        "subscription" => {
            // 按原文判空：纯空白串不是编译器产出的空 source，而是毒值，
            // 不做 trim 归一后放行。
            if !raw_source.is_empty() {
                return Err(HookError::Message(
                    "compiled subscription hook AST source must be empty".to_string(),
                ));
            }
            String::new()
        }
        _ => {
            if raw_source.trim().is_empty() {
                return Err(HookError::Message(
                    "compiled hook AST is missing source".to_string(),
                ));
            }
            let source = raw_source.to_string();
            // 与解析期标头校验同口径（plain identifier ≤36，编译期上限严于
            // 落库列宽 source_zhixu_id VARCHAR(64)），且与节点层身份闸同样
            // 吃原文（trim 只用于缺失判定）：毒 source（" buyer"）解码期
            // 确定性拒绝，而不是被 trim 洗白放行、或成为永不匹配任何事实
            // 键的 source 维度。
            if !is_plain_identifier(&source) || source.len() > 36 {
                return Err(HookError::Message(format!(
                    "compiled hook AST source must be a plain identifier of at most 36 characters: {source:?}"
                )));
            }
            source
        }
    };
    let root = req
        .ast
        .get("root")
        .ok_or_else(|| HookError::Message("compiled hook AST root is missing".to_string()))?;
    let expr = expr_from_cloud_value(root)?;
    // 求值器是解码层最后一道防线：root 形态必须与 mode 一致，布尔树内部
    // 不得再嵌套订阅节点——两者都只能由手写毒 AST 构造，解析器产不出
    // （解析期位置约束见 validate_subscription_position）。订阅模式下顶层
    // subscriptionTarget 还必须与 root 订阅节点指向同一 @source::signal。
    match mode {
        "subscription" => {
            let Expr::Subscription {
                source: node_source,
                target: node_target,
            } = &expr
            else {
                return Err(HookError::Message(
                    "compiled subscription hook AST root must be a subscription node".to_string(),
                ));
            };
            if node_source != &target_source || node_target != &target_signal {
                return Err(HookError::Message(
                    "compiled subscription hook AST subscriptionTarget does not match the root subscription node"
                        .to_string(),
                ));
            }
        }
        "normal" if contains_nested_subscription(&expr) => {
            return Err(HookError::Message(
                "compiled normal hook AST must not contain subscription nodes".to_string(),
            ));
        }
        _ => {}
    }
    // Defense in depth: a hand-crafted compiled AST must satisfy the same
    // invariants as a parsed expression before it may drive state
    // transitions — per gate (hook: positive-anchor invariant; filter:
    // admission vocabulary).
    match req.gate {
        Gate::Hook => validate_hook(&expr)?,
        Gate::Filter => validate_filter_hook(&expr)?,
    }
    let signals = signal_map(req.signals, req.profile)?;
    let result = eval_expr(&expr, &source, &signals, now)?;

    Ok(EvalCompiledHookOutput {
        uvp_core_version: CORE_VERSION,
        semantic_version: SEMANTIC_VERSION,
        profile: req.profile,
        state: result.state,
        ready_at: result
            .ready_at
            .map(|ts| ts.to_rfc3339_opts(SecondsFormat::Millis, true)),
        expires_at: result
            .expires_at
            .map(|ts| ts.to_rfc3339_opts(SecondsFormat::Millis, true)),
        reason: result.reason,
    })
}

fn signal_map(signals: Vec<SignalFact>, profile: Profile) -> Result<BTreeMap<String, SignalEntry>> {
    let mut result = BTreeMap::new();
    for signal in signals {
        // 解码层最后一道防线：事实身份必须与解析器对 hook 侧身份的口径
        // 一致——source 是 plain identifier 且 ≤36（编译期钉死的键上限，
        // 严于落库列 hook_dependency.source_zhixu_id VARCHAR(64)），signal_name 是三段式
        // task.stage.signal、每段 plain identifier、全名 ≤100
        // （individual_record.signal_name VARCHAR(100)）。空 source 是
        // 合法的"无归属事实"（语义语料的负例形态，永不匹配非空 hook
        // source），不是畸形身份。
        if !signal.source.is_empty()
            && (!is_plain_identifier(&signal.source) || signal.source.len() > 36)
        {
            return Err(HookError::Message(format!(
                "signal fact source must be a plain identifier of at most 36 characters: {:?}",
                signal.source
            )));
        }
        if !valid_signal_identity(&signal.signal_name) {
            return Err(HookError::Message(format!(
                "signal fact name must use task.stage.signal and be at most 100 characters: {:?}",
                signal.signal_name
            )));
        }
        let received_at = parse_time(&signal.received_at, profile)?;
        // 同一事实键（source::signalName）的重复事实取 received_at 最早者
        // 获胜：归约结果与输入数组顺序无关，求值语义是事实集的纯函数。
        // 键的存在性单调——一旦在场永不移除，仅锚点时间戳可前移。
        result
            .entry(signal_key(&signal.source, &signal.signal_name))
            .and_modify(|existing: &mut SignalEntry| {
                if received_at < existing.received_at {
                    existing.received_at = received_at;
                }
            })
            .or_insert(SignalEntry {
                source: signal.source,
                signal_name: signal.signal_name,
                received_at,
            });
    }
    Ok(result)
}

fn expr_from_cloud_value(value: &Value) -> Result<Expr> {
    expr_from_cloud_value_at_depth(value, 0)
}

fn expr_from_cloud_value_at_depth(value: &Value, depth: usize) -> Result<Expr> {
    if depth > MAX_PARSE_DEPTH {
        return Err(HookError::Message(format!(
            "compiled hook AST nesting exceeds the maximum depth of {MAX_PARSE_DEPTH}"
        )));
    }
    let kind = value
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| HookError::Message("compiled hook AST node is missing type".to_string()))?;

    match kind {
        "signal" => {
            reject_unknown_keys(
                value.as_object().ok_or_else(|| {
                    HookError::Message("compiled signal AST node must be an object".to_string())
                })?,
                &["type", "signal"],
                "compiled signal AST node",
            )?;
            let signal = value
                .get("signal")
                .and_then(Value::as_str)
                .filter(|signal| !signal.trim().is_empty())
                .ok_or_else(|| {
                    HookError::Message("compiled signal AST node is missing signal".to_string())
                })?;
            // 解码层身份闸与解析期 read_identifier + task.stage.signal 同口径：
            // 毒原子（拼错段数、超长、内嵌空格）确定性拒绝，而不是解码成
            // 永不匹配事实集的 Signal（那会把不匹配伪装成 ok:true needs_more）。
            if !valid_signal_identity(signal) {
                return Err(HookError::Message(format!(
                    "compiled signal AST node must use task.stage.signal and be at most 100 characters: {signal:?}"
                )));
            }
            Ok(Expr::Signal(signal.to_string()))
        }
        "subscription" => {
            reject_unknown_keys(
                value.as_object().ok_or_else(|| {
                    HookError::Message(
                        "compiled subscription AST node must be an object".to_string(),
                    )
                })?,
                &["type", "source", "signal"],
                "compiled subscription AST node",
            )?;
            let source = value
                .get("source")
                .and_then(Value::as_str)
                .filter(|source| !source.trim().is_empty())
                .ok_or_else(|| {
                    HookError::Message(
                        "compiled subscription AST node is missing source".to_string(),
                    )
                })?;
            let signal = value
                .get("signal")
                .and_then(Value::as_str)
                .filter(|signal| !signal.trim().is_empty())
                .ok_or_else(|| {
                    HookError::Message(
                        "compiled subscription AST node is missing signal".to_string(),
                    )
                })?;
            // 与解析期 parse_subscription 的目标校验同口径（source：plain
            // identifier ≤36；signal：三段式、每段 plain identifier、≤100）。
            if !is_plain_identifier(source) || source.len() > 36 {
                return Err(HookError::Message(format!(
                    "compiled subscription AST node source must be a plain identifier of at most 36 characters: {source:?}"
                )));
            }
            if !valid_signal_identity(signal) {
                return Err(HookError::Message(format!(
                    "compiled subscription AST node signal must use task.stage.signal and be at most 100 characters: {signal:?}"
                )));
            }
            Ok(Expr::Subscription {
                source: source.to_string(),
                target: signal.to_string(),
            })
        }
        "neg" => {
            reject_unknown_keys(
                value.as_object().ok_or_else(|| {
                    HookError::Message("compiled neg AST node must be an object".to_string())
                })?,
                &["type", "expr"],
                "compiled neg AST node",
            )?;
            Ok(Expr::Not(Box::new(expr_from_cloud_value_at_depth(
                value.get("expr").ok_or_else(|| {
                    HookError::Message("compiled neg AST node is missing expr".to_string())
                })?,
                depth + 1,
            )?)))
        }
        "and" | "or" => {
            reject_unknown_keys(
                value.as_object().ok_or_else(|| {
                    HookError::Message("compiled boolean AST node must be an object".to_string())
                })?,
                &["type", "left", "right"],
                "compiled boolean AST node",
            )?;
            let left = expr_from_cloud_value_at_depth(
                value.get("left").ok_or_else(|| {
                    HookError::Message("compiled boolean AST node is missing left".to_string())
                })?,
                depth + 1,
            )?;
            let right = expr_from_cloud_value_at_depth(
                value.get("right").ok_or_else(|| {
                    HookError::Message("compiled boolean AST node is missing right".to_string())
                })?,
                depth + 1,
            )?;
            Ok(if kind == "and" {
                Expr::And(vec![left, right])
            } else {
                Expr::Or(vec![left, right])
            })
        }
        "delay" => {
            reject_unknown_keys(
                value.as_object().ok_or_else(|| {
                    HookError::Message("compiled delay AST node must be an object".to_string())
                })?,
                &["type", "expr", "rawDuration", "durationSeconds"],
                "compiled delay AST node",
            )?;
            let expr = expr_from_cloud_value_at_depth(
                value.get("expr").ok_or_else(|| {
                    HookError::Message("compiled delay AST node is missing expr".to_string())
                })?,
                depth + 1,
            )?;
            let raw_duration = value
                .get("rawDuration")
                .and_then(Value::as_str)
                .filter(|duration| !duration.trim().is_empty())
                .ok_or_else(|| {
                    HookError::Message("compiled delay AST is missing rawDuration".to_string())
                })?
                .to_string();
            let duration_seconds = value
                .get("durationSeconds")
                .and_then(Value::as_i64)
                .ok_or_else(|| {
                    HookError::Message(format!(
                        "compiled delay AST has invalid duration: {raw_duration}"
                    ))
                })?;
            if duration_seconds <= 0 {
                return Err(HookError::Message(
                    "compiled delay AST duration must be positive".to_string(),
                ));
            }
            let parsed_seconds = duration_to_seconds(&raw_duration)?;
            if parsed_seconds != duration_seconds {
                return Err(HookError::Message(format!(
                    "compiled delay AST duration mismatch: rawDuration={raw_duration}, durationSeconds={duration_seconds}"
                )));
            }
            Ok(Expr::Delay {
                expr: Box::new(expr),
                raw_duration,
                duration_seconds,
            })
        }
        other => Err(HookError::Message(format!(
            "unsupported compiled hook AST node type: {other}"
        ))),
    }
}

fn reject_unknown_keys(
    object: &serde_json::Map<String, Value>,
    allowed: &[&str],
    label: &str,
) -> Result<()> {
    if let Some(key) = object.keys().find(|key| !allowed.contains(&key.as_str())) {
        return Err(HookError::Message(format!(
            "{label} contains unsupported field: {key}"
        )));
    }
    Ok(())
}

/// 顶层可选字符串字段：缺失或 null 视为空（对齐 Go 的零值解码语义），
/// 其余非字符串类型在解码期确定性拒绝（Go 侧由 JSON 类型解码拒绝）。
fn optional_ast_str<'a>(object: &'a serde_json::Map<String, Value>, key: &str) -> Result<&'a str> {
    match object.get(key) {
        None | Some(Value::Null) => Ok(""),
        Some(Value::String(value)) => Ok(value.as_str()),
        Some(_) => Err(HookError::Message(format!(
            "compiled hook AST {key} must be a string"
        ))),
    }
}

#[derive(Clone)]
struct SignalEntry {
    #[allow(dead_code)]
    source: String,
    #[allow(dead_code)]
    signal_name: String,
    received_at: DateTime<Utc>,
}

struct InternalEval {
    state: EvalState,
    anchors: Vec<DateTime<Utc>>,
    ready_at: Option<DateTime<Utc>>,
    /// 本 Ready 的有效期至（None = 无限期）。仅 Ready 态可携带，来自衰减
    /// 否决位 `~(A+duration)` 的内层成熟时刻。它是纯输出元数据：任何
    /// 调度不得依赖它——poke 只为"变真"设闹钟，衰减永不为"变假"唤醒。
    expires_at: Option<DateTime<Utc>>,
    reason: Option<String>,
}

fn eval_expr(
    expr: &Expr,
    source: &str,
    signals: &BTreeMap<String, SignalEntry>,
    now: DateTime<Utc>,
) -> Result<InternalEval> {
    match expr {
        Expr::Signal(signal) => eval_signal(source, signal, signals),
        Expr::Subscription { .. } => Ok(InternalEval {
            state: EvalState::NeedsMore,
            anchors: Vec::new(),
            ready_at: None,
            expires_at: None,
            reason: Some(
                "subscription hooks are delivered per contributing event by the state machine"
                    .to_string(),
            ),
        }),
        Expr::Not(inner) => {
            let evaluated = eval_expr(inner, source, signals, now)?;
            match evaluated.state {
                EvalState::Ready => Ok(InternalEval {
                    state: EvalState::Impossible,
                    anchors: Vec::new(),
                    ready_at: None,
                    expires_at: None,
                    reason: Some(format!(
                        "negated condition exists: {}",
                        normalize_tight(inner)
                    )),
                }),
                // 衰减否决位（合取直接子项上的 `~(A+duration)`，校验期
                // 位置闸保证唯一合法形态）：内层在案未熟 → 本项此刻
                // Ready，有效期至内层成熟时刻——此后内层翻 Ready、本项翻
                // Impossible（A 缺席/被否决的分支永不到达成熟，走下方
                // 无限期臂）。
                EvalState::Wait => Ok(InternalEval {
                    state: EvalState::Ready,
                    anchors: Vec::new(),
                    ready_at: None,
                    expires_at: evaluated.ready_at,
                    reason: None,
                }),
                EvalState::Impossible | EvalState::NeedsMore => Ok(InternalEval {
                    state: EvalState::Ready,
                    anchors: Vec::new(),
                    ready_at: None,
                    expires_at: None,
                    reason: None,
                }),
            }
        }
        Expr::Delay {
            expr,
            duration_seconds,
            ..
        } => {
            let evaluated = eval_expr(expr, source, signals, now)?;
            match evaluated.state {
                EvalState::Impossible | EvalState::NeedsMore => Ok(evaluated),
                // 内层已处于 Wait（嵌套延时如 (A +5s) +10s，或延时复合式中间
                // 态）：把内层的 due_at 原样上浮为本次等待期限。否则这里返回
                // NeedsMore（语义="缺正锚"）会让 adapter 不持久化任何定时，
                // 内层到期后不再有新事件触发重评，订单永久卡在中间态。到期后
                // poke 重评时内层锚点就位，本层再按自身时长推进（与回放
                // oracle 的 delay_value 语义一致）。
                EvalState::Wait => Ok(InternalEval {
                    state: EvalState::Wait,
                    anchors: Vec::new(),
                    ready_at: evaluated.ready_at,
                    expires_at: None,
                    reason: None,
                }),
                EvalState::Ready => {
                    let Some(anchor) = evaluated.anchors.iter().max().copied() else {
                        return Ok(InternalEval {
                            state: EvalState::NeedsMore,
                            anchors: Vec::new(),
                            ready_at: None,
                            expires_at: None,
                            reason: None,
                        });
                    };
                    // 溢出必须走错误返回而不是 panic：panic 跨 extern "C" 边界会 abort
                    // 整个宿主进程（statemachine），毒 hook 会杀死所有在途信号处理。
                    let delta =
                        chrono::Duration::try_seconds(*duration_seconds).ok_or_else(|| {
                            HookError::Message(format!(
                                "delay duration seconds out of range: {duration_seconds}"
                            ))
                        })?;
                    let ready_at = anchor.checked_add_signed(delta).ok_or_else(|| {
                        HookError::Message(format!(
                            "delay readyAt overflowed: anchor {anchor} plus {duration_seconds}s"
                        ))
                    })?;
                    if now >= ready_at {
                        // 成熟是永久的：延时 Ready 不携带有效期。
                        Ok(InternalEval {
                            state: EvalState::Ready,
                            anchors: vec![ready_at],
                            ready_at: Some(ready_at),
                            expires_at: None,
                            reason: None,
                        })
                    } else {
                        Ok(InternalEval {
                            state: EvalState::Wait,
                            anchors: Vec::new(),
                            ready_at: Some(ready_at),
                            expires_at: None,
                            reason: None,
                        })
                    }
                }
            }
        }
        Expr::And(terms) => {
            let mut anchors = Vec::new();
            let mut waits = Vec::new();
            let mut needs_more = false;
            let mut min_expires: Option<DateTime<Utc>> = None;
            for term in terms {
                let evaluated = eval_expr(term, source, signals, now)?;
                match evaluated.state {
                    EvalState::Impossible => return Ok(evaluated),
                    EvalState::NeedsMore => needs_more = true,
                    EvalState::Wait => {
                        if let Some(ready_at) = evaluated.ready_at {
                            waits.push(ready_at);
                        }
                    }
                    EvalState::Ready => {
                        // 防御语义（按构造不可达，求值器自洽）：衰减项的
                        // expires_at 只应指向未来（Not 构造时内层 Wait 保证
                        // 严格晚于 now）。若已到期，按 Impossible 处理
                        // （fail-closed），不得把过期否决当作仍然成立放行。
                        if evaluated
                            .expires_at
                            .is_some_and(|expires_at| expires_at <= now)
                        {
                            return Ok(InternalEval {
                                state: EvalState::Impossible,
                                anchors: Vec::new(),
                                ready_at: None,
                                expires_at: None,
                                reason: Some(format!(
                                    "decaying veto expired: {}",
                                    normalize_tight(term)
                                )),
                            });
                        }
                        anchors.extend(evaluated.anchors);
                        // And 的 Ready 有效期取成员最紧者（None = 无限，
                        // 不放宽任何有限期）：任一成员到期即整体不再成立。
                        min_expires = [min_expires, evaluated.expires_at]
                            .into_iter()
                            .flatten()
                            .min();
                    }
                }
            }
            if needs_more {
                return Ok(InternalEval {
                    state: EvalState::NeedsMore,
                    anchors: Vec::new(),
                    ready_at: None,
                    expires_at: None,
                    reason: None,
                });
            }
            if let Some(ready_at) = waits.into_iter().max() {
                return Ok(InternalEval {
                    state: EvalState::Wait,
                    anchors: Vec::new(),
                    ready_at: Some(ready_at),
                    expires_at: None,
                    reason: None,
                });
            }
            Ok(InternalEval {
                state: EvalState::Ready,
                ready_at: anchors.iter().max().copied(),
                anchors,
                expires_at: min_expires,
                reason: None,
            })
        }
        Expr::Or(terms) => {
            let mut waits = Vec::new();
            let mut has_open = false;
            let mut all_impossible = true;
            let mut ready: Option<InternalEval> = None;
            for term in terms {
                let evaluated = eval_expr(term, source, signals, now)?;
                match evaluated.state {
                    EvalState::Ready => {
                        // Maturity causality（OR 复合分支延时锚点裁决）：OR 取
                        // "最早成熟"的分支——分支成熟时刻 = 纯信号接收时刻、
                        // AND 分支内部取 max（最新锚点）、嵌套 OR 取其获胜分支
                        // 的成熟时刻。获胜分支原样上浮：其 anchors 的 max 就是
                        // 成熟时刻，外层 Delay 用获胜分支的成熟时刻计时（与
                        // 合约 _orValue / 回放 oracle 的 or_value 逐字节一致）。
                        // 无锚点的 Ready 分支（如 Not 就绪）永不获胜——链上其
                        // anchorAt=0，_minAnchor 同样让位于任何带锚分支。
                        // 获胜分支的 expires_at 原样上浮（衰减只收紧获胜
                        // 分支自身的有效期，不跨分支取 min）。
                        let better = ready.as_ref().is_none_or(|current| {
                            match (branch_maturity(&evaluated), branch_maturity(current)) {
                                (Some(candidate), Some(incumbent)) => candidate < incumbent,
                                (Some(_), None) => true,
                                (None, _) => false,
                            }
                        });
                        if better {
                            ready = Some(evaluated);
                        }
                    }
                    EvalState::Wait => {
                        has_open = true;
                        all_impossible = false;
                        if let Some(ready_at) = evaluated.ready_at {
                            waits.push(ready_at);
                        }
                    }
                    EvalState::NeedsMore => {
                        has_open = true;
                        all_impossible = false;
                    }
                    EvalState::Impossible => {}
                }
            }
            if let Some(evaluated) = ready {
                return Ok(evaluated);
            }
            if let Some(ready_at) = waits.into_iter().min() {
                return Ok(InternalEval {
                    state: EvalState::Wait,
                    anchors: Vec::new(),
                    ready_at: Some(ready_at),
                    expires_at: None,
                    reason: None,
                });
            }
            if all_impossible && !has_open {
                return Ok(InternalEval {
                    state: EvalState::Impossible,
                    anchors: Vec::new(),
                    ready_at: None,
                    expires_at: None,
                    reason: Some(format!(
                        "all OR branches are cancelled: {}",
                        normalize_tight(expr)
                    )),
                });
            }
            Ok(InternalEval {
                state: EvalState::NeedsMore,
                anchors: Vec::new(),
                ready_at: None,
                expires_at: None,
                reason: None,
            })
        }
    }
}

/// Maturity moment of an evaluated READY branch — the moment its causal
/// anchor settled: a pure signal matures at its receive time, an AND branch
/// at its latest anchor (max), a matured delay at its advanced due date.
/// The winner selection for OR branches and the outer `Delay` anchor share
/// this definition (both take `max(anchors)`), removing the previous
/// min-anchor / max-anchor split that diverged from the chain's `_orValue`.
/// `None` marks an anchor-less Ready branch (chain anchorAt == 0): it never
/// competes for the OR anchor.
fn branch_maturity(evaluated: &InternalEval) -> Option<DateTime<Utc>> {
    evaluated.anchors.iter().copied().max()
}

fn eval_signal(
    source: &str,
    signal: &str,
    signals: &BTreeMap<String, SignalEntry>,
) -> Result<InternalEval> {
    let entry = signals.get(&signal_key(source, signal));
    if let Some(entry) = entry {
        return Ok(InternalEval {
            state: EvalState::Ready,
            anchors: vec![entry.received_at],
            ready_at: Some(entry.received_at),
            expires_at: None,
            reason: None,
        });
    }
    Ok(InternalEval {
        state: EvalState::NeedsMore,
        anchors: Vec::new(),
        ready_at: None,
        expires_at: None,
        reason: None,
    })
}

fn signal_key(source: &str, signal: &str) -> String {
    format!("{source}::{signal}")
}

fn parse_time(value: &str, profile: Profile) -> Result<DateTime<Utc>> {
    let parsed = DateTime::parse_from_rfc3339(value)
        .map_err(|err| HookError::Message(format!("invalid date {value:?}: {err}")))?
        .with_timezone(&Utc);
    if profile == Profile::EvmStrict {
        let timestamp = parsed.timestamp();
        return Utc
            .timestamp_opt(timestamp, 0)
            .single()
            .ok_or_else(|| HookError::Message(format!("invalid date {value:?}")));
    }
    Ok(parsed)
}
