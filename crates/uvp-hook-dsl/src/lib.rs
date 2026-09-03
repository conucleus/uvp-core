use chrono::{DateTime, SecondsFormat, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use thiserror::Error;

pub const CORE_VERSION: &str = env!("CARGO_PKG_VERSION");
pub const SEMANTIC_VERSION: &str = "uvp.semantic.v1";
pub const CLOUD_AST_SCHEMA_VERSION: &str = "uvp.cloudAst.v1";

/// uvp-semantic/0.6 的跨秩序四典型（::OUTSIDE@ / ::MERGE@ / 旧 ::ANCHOR@ /
/// OUTSOURCE）已在 uvp.semantic.v1 统一退役为「订阅 + 铸单」模型；解析器保留关键字识别，
/// 以给出精确的迁移报错而不是笼统的语法错误。
pub const RETIRED_KEYWORDS_HINT: &str = "cross-source entries retired in uvp.semantic.v1; use ::ANCHOR(@source::task.stage.signal) as the unified subscription entry (see subscription-mint-spec.md)";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Profile {
    #[default]
    EvmStrict,
    CloudCompat,
}

#[derive(Debug, Error)]
pub enum HookError {
    #[error("{0}")]
    Message(String),
}

type Result<T> = std::result::Result<T, HookError>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Expr {
    Signal(String),
    /// 跨源订阅通道：`ANCHOR(@source::task.stage.signal)`。
    /// 按类寻址（source 为 zhixu 局部因果身份类），逐事件投递并携带溯源，
    /// 无表达式裁决。路由（按单 / 扇入）由接收方锚定状态决定，聚合判定
    /// 归订阅方执行器；per-fact 代铸由阶段级 `mint` 声明表达，不属于 hook。
    Subscription {
        source: String,
        target: String,
    },
    Not(Box<Expr>),
    And(Vec<Expr>),
    Or(Vec<Expr>),
    Delay {
        expr: Box<Expr>,
        raw_duration: String,
        duration_seconds: i64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookExpr {
    pub raw: String,
    pub source: String,
    pub condition: Expr,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Dependency {
    pub kind: DependencyKind,
    pub source: String,
    pub signal_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delay_seconds: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DependencyKind {
    Positive,
    Negative,
    Timer,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ParseHookOutput {
    pub uvp_core_version: &'static str,
    pub semantic_version: &'static str,
    pub profile: Profile,
    pub compatibility: Compatibility,
    pub hook_name: String,
    pub source: String,
    pub mode: HookMode,
    pub raw_hook: String,
    pub raw_condition: String,
    pub runtime_condition: String,
    pub normalized_expression: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subscription_target: Option<SubscriptionTarget>,
    pub dependencies: Vec<Dependency>,
    pub ast: Value,
    pub cloud_ast: Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SubscriptionTarget {
    pub source: String,
    pub signal_name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Compatibility {
    Portable,
    CloudOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HookMode {
    Normal,
    Subscription,
}

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
#[serde(rename_all = "camelCase")]
pub struct ParseHookRequest {
    #[serde(default)]
    pub profile: Profile,
    #[serde(default)]
    pub hook_name: String,
    pub hook: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
#[serde(rename_all = "camelCase")]
pub struct EvalCompiledHookRequest {
    #[serde(default)]
    pub profile: Profile,
    pub ast: Value,
    #[serde(default)]
    pub signals: Vec<SignalFact>,
    pub now: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SignalFact {
    #[serde(default)]
    pub source: String,
    pub signal_name: String,
    pub received_at: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Envelope<T: Serialize> {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<T>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diagnostics: Option<Vec<Diagnostic>>,
}

#[derive(Debug, Serialize)]
pub struct Diagnostic {
    pub message: String,
}

pub fn parse_hook_json(input: &str) -> String {
    let result = serde_json::from_str::<ParseHookRequest>(input)
        .map_err(|err| HookError::Message(format!("invalid parse hook request: {err}")))
        .and_then(parse_hook);
    envelope_json(result)
}

pub fn eval_compiled_hook_json(input: &str) -> String {
    let result = serde_json::from_str::<EvalCompiledHookRequest>(input)
        .map_err(|err| HookError::Message(format!("invalid eval compiled hook request: {err}")))
        .and_then(eval_compiled_hook);
    envelope_json(result)
}

fn envelope_json<T: Serialize>(result: Result<T>) -> String {
    let envelope = match result {
        Ok(value) => Envelope {
            ok: true,
            value: Some(value),
            diagnostics: None,
        },
        Err(err) => Envelope::<T> {
            ok: false,
            value: None,
            diagnostics: Some(vec![Diagnostic {
                message: err.to_string(),
            }]),
        },
    };
    serde_json::to_string(&envelope).expect("envelope serialization should not fail")
}

pub fn parse_hook(req: ParseHookRequest) -> Result<ParseHookOutput> {
    let profile = req.profile;
    let hook_name = req.hook_name;
    // 长度上限对齐 DDL 列宽（hook_name VARCHAR(36)、signal_name VARCHAR(100)）：
    // 超长定义在解析期即拒绝，不再拖到落库才以 value too long 失败。
    if hook_name.trim().is_empty() || hook_name.len() > 36 {
        return Err(HookError::Message(
            "hook_name must be 1-36 characters".to_string(),
        ));
    }
    let hook = parse_hook_expr(&req.hook, profile)?;
    validate_hook(&hook.condition, profile)?;

    let raw_condition = req
        .hook
        .split_once("::")
        .map(|(_, cond)| cond.trim().to_string())
        .unwrap_or_default();
    let mode = hook_mode(&hook.condition);
    let compatibility = compatibility_for(&hook, profile);
    let runtime_condition = runtime_condition(&hook, &hook_name, profile)?;
    let normalized_expression = format!(
        "{}::{}",
        hook.source,
        normalize_condition(&hook.condition, NormalizeStyle::Tight)
    );
    let dependencies = extract_dependencies(&hook, profile);
    let cloud_ast = cloud_ast_for(&hook, &hook_name, profile)?;
    let subscription_target = match &hook.condition {
        Expr::Subscription { source, target } => Some(SubscriptionTarget {
            source: source.clone(),
            signal_name: target.clone(),
        }),
        _ => None,
    };

    Ok(ParseHookOutput {
        uvp_core_version: CORE_VERSION,
        semantic_version: SEMANTIC_VERSION,
        profile,
        compatibility,
        hook_name,
        source: hook.source.clone(),
        mode,
        raw_hook: req.hook,
        raw_condition,
        runtime_condition,
        normalized_expression,
        subscription_target,
        dependencies,
        ast: hook_to_value(&hook),
        cloud_ast,
    })
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
            "unsupported compiled hook AST mode: {mode}; outside_spawn/merge/anchor were retired in uvp.semantic.v1"
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
    let (target_source, target_signal) =
        if mode == "subscription" {
            let target = req.ast.get("subscriptionTarget");
            let source = target
                .and_then(|target| target.get("source"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty());
            let signal = target
                .and_then(|target| target.get("signal"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty());
            match (source, signal) {
                (Some(source), Some(signal)) => (source.to_string(), signal.to_string()),
                _ => return Err(HookError::Message(
                    "compiled subscription hook AST is missing subscriptionTarget.source/.signal"
                        .to_string(),
                )),
            }
        } else {
            (String::new(), String::new())
        };
    let now = parse_time(&req.now, req.profile)?;
    // 订阅钩子标头恒为空：投递目标由阶段静态执行器决定，路由由接收方锚定
    // 状态与对接记录裁决，因此仅 subscription 模式允许空 source。
    let raw_source = req.ast.get("source").and_then(Value::as_str).map(str::trim);
    let source = match mode {
        "subscription" => {
            if !raw_source.unwrap_or_default().is_empty() {
                return Err(HookError::Message(
                    "compiled subscription hook AST source must be empty".to_string(),
                ));
            }
            String::new()
        }
        _ => raw_source
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
            .ok_or_else(|| HookError::Message("compiled hook AST is missing source".to_string()))?,
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
    // positive-anchor invariant as a parsed expression before it may drive
    // hook status transitions.
    validate_hook(&expr, req.profile)?;
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
        reason: result.reason,
    })
}

fn signal_map(signals: Vec<SignalFact>, profile: Profile) -> Result<BTreeMap<String, SignalEntry>> {
    let mut result = BTreeMap::new();
    for signal in signals {
        let received_at = parse_time(&signal.received_at, profile)?;
        // First-writer-wins matches the replay oracle and the documented
        // runtime contract: a repeated source::signalName fact never replaces
        // the first received instance.
        result
            .entry(signal_key(&signal.source, &signal.signal_name))
            .and_modify(|existing: &mut SignalEntry| {
                // first-RECEIVED-wins 必须与输入数组顺序无关：同一事实键的
                // 重复到达取时间戳最早者，语义成为事实集的纯函数。
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

const MAX_PARSE_DEPTH: usize = 256;

fn parse_hook_expr(raw: &str, profile: Profile) -> Result<HookExpr> {
    parse_hook_expr_at_depth(raw, profile)
}

fn parse_hook_expr_at_depth(raw: &str, _profile: Profile) -> Result<HookExpr> {
    let (source, condition_raw) = raw
        .trim()
        .split_once("::")
        .ok_or_else(|| HookError::Message("hook expression must contain \"::\"".to_string()))?;
    let source = source.trim().to_string();
    let condition_raw = condition_raw.trim();
    if condition_raw.is_empty() {
        return Err(HookError::Message(
            "hook condition cannot be empty".to_string(),
        ));
    }
    if source.is_empty() && !starts_cross_source(condition_raw) {
        return Err(HookError::Message(
            "empty source is only allowed for ANCHOR(@…) subscription hooks".to_string(),
        ));
    }
    if !source.is_empty() {
        // 标头 source 类是落库列（VARCHAR(36)）与路由键：解析期钉死长度与
        // 字符集（对齐 Go 镜像 zhixu_schema.go 的 ≤36 与 plain-identifier
        // 规则）。订阅形态（::ANCHOR(@…)）标头恒为空，不受此限——订阅目标
        // source 另有 is_plain_identifier 与 100 长度上限校验，保持不变。
        if source.len() > 36 {
            return Err(HookError::Message(format!(
                "hook source class exceeds the maximum length of 36 characters: {source}"
            )));
        }
        if !is_plain_identifier(&source) {
            return Err(HookError::Message(format!(
                "hook source must be a plain identifier: {source}"
            )));
        }
    }
    reject_unsupported_operators(condition_raw)?;
    let mut parser = Parser::new(condition_raw);
    let condition = parser.parse()?;
    validate_subscription_position(&condition, true)?;
    if matches!(condition, Expr::Subscription { .. }) && !source.is_empty() {
        return Err(HookError::Message(
            "subscription entries must use an empty source header: ::ANCHOR(@source::task.stage.signal)"
                .to_string(),
        ));
    }
    Ok(HookExpr {
        raw: raw.to_string(),
        source,
        condition,
    })
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
            value
                .get("signal")
                .and_then(Value::as_str)
                .filter(|signal| !signal.trim().is_empty())
                .map(|signal| Expr::Signal(signal.to_string()))
                .ok_or_else(|| {
                    HookError::Message("compiled signal AST node is missing signal".to_string())
                })
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

// Subscription operators are cross-source delivery channels, not
// backend/executor input declarations. Backend/executor external inputs are
// sent to UVP only when the executor explicitly chooses to do so; there is no
// externalSignals declaration any more.
fn validate_subscription_position(expr: &Expr, root: bool) -> Result<()> {
    match expr {
        Expr::Subscription { .. } => {
            if !root {
                return Err(HookError::Message(format!(
                    "{RETIRED_KEYWORDS_HINT}; a subscription must be the complete hook condition"
                )));
            }
            Ok(())
        }
        Expr::Signal(_) => Ok(()),
        Expr::Not(inner) => validate_subscription_position(inner, false),
        Expr::And(terms) | Expr::Or(terms) => {
            for term in terms {
                validate_subscription_position(term, false)?;
            }
            Ok(())
        }
        Expr::Delay { expr, .. } => validate_subscription_position(expr, false),
    }
}

fn starts_cross_source(value: &str) -> bool {
    // 旧关键字仍放行进解析器，以便命中精确的退役提示而非笼统的空标头报错。
    value.starts_with("ANCHOR")
        || value.starts_with("OUTSIDE")
        || value.starts_with("MERGE")
        || value.starts_with("OUTSOURCE")
}

fn reject_unsupported_operators(condition: &str) -> Result<()> {
    if condition.contains("&&") {
        return Err(HookError::Message(format!(
            "unsupported operator && in {condition:?}"
        )));
    }
    if condition.contains("%%") {
        return Err(HookError::Message(format!(
            "unsupported operator %% in {condition:?}"
        )));
    }
    Ok(())
}

fn validate_hook(expr: &Expr, profile: Profile) -> Result<()> {
    let anchored = validate_anchors(expr, profile)?;
    if !anchored {
        return Err(HookError::Message(
            "hook condition must contain at least one positive signal anchor".to_string(),
        ));
    }
    if let Expr::Or(terms) = expr {
        for term in terms {
            if !has_positive_anchor(term) {
                return Err(HookError::Message(
                    "each OR branch must contain a positive signal anchor".to_string(),
                ));
            }
        }
    }
    Ok(())
}

fn validate_anchors(expr: &Expr, profile: Profile) -> Result<bool> {
    match expr {
        Expr::Signal(_) | Expr::Subscription { .. } => Ok(true),
        Expr::Not(inner) => {
            if !matches!(inner.as_ref(), Expr::Signal(_)) {
                return Err(HookError::Message(
                    "negation only supports direct signal references".to_string(),
                ));
            }
            Ok(false)
        }
        Expr::Delay {
            expr,
            duration_seconds,
            ..
        } => {
            if *duration_seconds <= 0 {
                return Err(HookError::Message("delay must be positive".to_string()));
            }
            let anchored = validate_anchors(expr, profile)?;
            if !anchored {
                return Err(HookError::Message(
                    "delay requires a positive signal anchor".to_string(),
                ));
            }
            Ok(true)
        }
        Expr::And(terms) => {
            let mut anchored = false;
            for term in terms {
                anchored |= validate_anchors(term, profile)?;
            }
            Ok(anchored)
        }
        Expr::Or(terms) => {
            let mut anchored = false;
            for term in terms {
                let term_anchored = validate_anchors(term, profile)?;
                if !term_anchored {
                    return Err(HookError::Message(
                        "each OR branch must contain a positive signal anchor".to_string(),
                    ));
                }
                anchored = true;
            }
            Ok(anchored)
        }
    }
}

fn has_positive_anchor(expr: &Expr) -> bool {
    match expr {
        Expr::Signal(_) | Expr::Subscription { .. } => true,
        Expr::Not(_) => false,
        Expr::Delay { expr, .. } => has_positive_anchor(expr),
        Expr::And(terms) | Expr::Or(terms) => terms.iter().any(has_positive_anchor),
    }
}

#[derive(Debug, Clone, Copy)]
enum NormalizeStyle {
    Tight,
    Cloud,
}

fn normalize_condition(expr: &Expr, style: NormalizeStyle) -> String {
    match style {
        NormalizeStyle::Tight => normalize_tight(expr),
        NormalizeStyle::Cloud => normalize_cloud(expr, 0),
    }
}

fn normalize_tight(expr: &Expr) -> String {
    match expr {
        Expr::Signal(signal) => signal.clone(),
        Expr::Subscription { source, target } => {
            format!("ANCHOR(@{source}::{target})")
        }
        Expr::Not(inner) => format!("~{}", normalize_for_unary_tight(inner)),
        Expr::Delay {
            expr, raw_duration, ..
        } => {
            format!("{}+{}", normalize_for_unary_tight(expr), raw_duration)
        }
        Expr::And(terms) => terms
            .iter()
            .map(normalize_for_join_tight)
            .collect::<Vec<_>>()
            .join("&"),
        Expr::Or(terms) => terms
            .iter()
            .map(normalize_for_join_tight)
            .collect::<Vec<_>>()
            .join("|"),
    }
}

fn normalize_for_unary_tight(expr: &Expr) -> String {
    match expr {
        Expr::Signal(_) | Expr::Subscription { .. } => normalize_tight(expr),
        _ => format!("({})", normalize_tight(expr)),
    }
}

fn normalize_for_join_tight(expr: &Expr) -> String {
    match expr {
        Expr::And(_) | Expr::Or(_) => format!("({})", normalize_tight(expr)),
        _ => normalize_tight(expr),
    }
}

fn normalize_cloud(expr: &Expr, parent_precedence: u8) -> String {
    let precedence = precedence(expr);
    let body = match expr {
        Expr::Signal(signal) => signal.clone(),
        Expr::Subscription { source, target } => {
            format!("ANCHOR(@{source}::{target})")
        }
        Expr::Not(inner) => {
            let mut child = normalize_cloud(inner, precedence);
            if matches!(inner.as_ref(), Expr::And(_) | Expr::Or(_)) {
                child = format!("({child})");
            }
            format!("~{child}")
        }
        Expr::Delay {
            expr, raw_duration, ..
        } => {
            let mut child = normalize_cloud(expr, precedence);
            if matches!(expr.as_ref(), Expr::And(_) | Expr::Or(_)) {
                child = format!("({child})");
            }
            format!("{child} + {raw_duration}")
        }
        Expr::And(terms) => terms
            .iter()
            .map(|term| normalize_cloud(term, precedence))
            .collect::<Vec<_>>()
            .join(" & "),
        Expr::Or(terms) => terms
            .iter()
            .map(|term| normalize_cloud(term, precedence))
            .collect::<Vec<_>>()
            .join(" | "),
    };

    if precedence < parent_precedence && matches!(expr, Expr::And(_) | Expr::Or(_)) {
        return format!("({body})");
    }
    if matches!(expr, Expr::Delay { .. }) && parent_precedence > 0 {
        return format!("({body})");
    }
    body
}

fn precedence(expr: &Expr) -> u8 {
    match expr {
        Expr::Or(_) => 1,
        Expr::And(_) => 2,
        Expr::Not(_) | Expr::Delay { .. } => 3,
        Expr::Signal(_) | Expr::Subscription { .. } => 4,
    }
}

fn runtime_condition(hook: &HookExpr, _hook_name: &str, profile: Profile) -> Result<String> {
    if profile == Profile::EvmStrict {
        return Ok(normalize_condition(&hook.condition, NormalizeStyle::Tight));
    }
    match &hook.condition {
        Expr::Subscription { .. } => {
            Ok(normalize_condition(&hook.condition, NormalizeStyle::Tight))
        }
        _ => Ok(normalize_condition(&hook.condition, NormalizeStyle::Cloud)),
    }
}

fn hook_mode(expr: &Expr) -> HookMode {
    match expr {
        Expr::Subscription { .. } => HookMode::Subscription,
        _ => HookMode::Normal,
    }
}

fn compatibility_for(_hook: &HookExpr, profile: Profile) -> Compatibility {
    match profile {
        Profile::EvmStrict => Compatibility::Portable,
        Profile::CloudCompat => Compatibility::CloudOnly,
    }
}

fn extract_dependencies(hook: &HookExpr, _profile: Profile) -> Vec<Dependency> {
    let mut deps = Vec::new();
    collect_dependencies(&hook.condition, &hook.source, false, &mut deps);
    dedupe_dependencies(deps)
}

fn collect_dependencies(expr: &Expr, source: &str, negated: bool, out: &mut Vec<Dependency>) {
    match expr {
        Expr::Signal(signal) => out.push(Dependency {
            kind: if negated {
                DependencyKind::Negative
            } else {
                DependencyKind::Positive
            },
            source: source.to_string(),
            signal_name: signal.clone(),
            delay_seconds: None,
        }),
        Expr::Subscription { source, target } => {
            out.push(Dependency {
                kind: DependencyKind::Positive,
                source: source.clone(),
                signal_name: target.clone(),
                delay_seconds: None,
            });
        }
        Expr::Not(inner) => collect_dependencies(inner, source, !negated, out),
        Expr::Delay {
            expr,
            duration_seconds,
            ..
        } => {
            collect_dependencies(expr, source, negated, out);
            if !negated {
                let mut inner = Vec::new();
                collect_dependencies(expr, source, false, &mut inner);
                for dep in inner
                    .into_iter()
                    .filter(|dep| dep.kind == DependencyKind::Positive)
                {
                    out.push(Dependency {
                        kind: DependencyKind::Timer,
                        source: dep.source,
                        signal_name: dep.signal_name,
                        delay_seconds: Some(*duration_seconds),
                    });
                }
            }
        }
        Expr::And(terms) | Expr::Or(terms) => {
            for term in terms {
                collect_dependencies(term, source, negated, out);
            }
        }
    }
}

fn dedupe_dependencies(deps: Vec<Dependency>) -> Vec<Dependency> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for dep in deps {
        let key = (
            dep.kind,
            dep.source.clone(),
            dep.signal_name.clone(),
            dep.delay_seconds.unwrap_or_default(),
        );
        if seen.insert(key) {
            out.push(dep);
        }
    }
    out.sort_by(|left, right| {
        dependency_kind_name(left.kind)
            .cmp(dependency_kind_name(right.kind))
            .then(left.source.cmp(&right.source))
            .then(left.signal_name.cmp(&right.signal_name))
            .then(left.delay_seconds.cmp(&right.delay_seconds))
    });
    out
}

fn dependency_kind_name(kind: DependencyKind) -> &'static str {
    match kind {
        DependencyKind::Negative => "negative",
        DependencyKind::Positive => "positive",
        DependencyKind::Timer => "timer",
    }
}

fn hook_to_value(hook: &HookExpr) -> Value {
    json!({
        "raw": hook.raw,
        "source": hook.source,
        "condition": expr_to_ts_value(&hook.condition),
    })
}

fn expr_to_ts_value(expr: &Expr) -> Value {
    match expr {
        Expr::Signal(signal) => json!({ "kind": "signal", "signalName": signal }),
        Expr::Subscription { source, target } => json!({
            "kind": "subscription",
            "source": source,
            "signal": target
        }),
        Expr::Not(inner) => json!({ "kind": "not", "expr": expr_to_ts_value(inner) }),
        Expr::And(terms) => {
            json!({ "kind": "and", "terms": terms.iter().map(expr_to_ts_value).collect::<Vec<_>>() })
        }
        Expr::Or(terms) => {
            json!({ "kind": "or", "terms": terms.iter().map(expr_to_ts_value).collect::<Vec<_>>() })
        }
        Expr::Delay {
            expr,
            raw_duration,
            duration_seconds,
        } => json!({
            "kind": "delay",
            "expr": expr_to_ts_value(expr),
            "durationSeconds": duration_seconds,
            "rawDuration": raw_duration,
        }),
    }
}

fn cloud_ast_for(hook: &HookExpr, _hook_name: &str, _profile: Profile) -> Result<Value> {
    match &hook.condition {
        Expr::Subscription { source, target } => Ok(json!({
            "schemaVersion": CLOUD_AST_SCHEMA_VERSION,
            // 订阅钩子按事件逐次由状态机扇入或按对接记录路由投递；路由由接收
            // 方锚定状态裁决，聚合判定归订阅方执行器，per-fact 代铸归阶段
            // mint 声明。
            "source": "",
            "mode": HookMode::Subscription,
            "subscriptionTarget": {
                "source": source,
                "signal": target,
            },
            "root": expr_to_cloud_value(&hook.condition)
        })),
        _ => Ok(json!({
            "schemaVersion": CLOUD_AST_SCHEMA_VERSION,
            "source": hook.source.clone(),
            "mode": HookMode::Normal,
            "root": expr_to_cloud_value(&hook.condition)
        })),
    }
}

fn expr_to_cloud_value(expr: &Expr) -> Value {
    match expr {
        Expr::Signal(signal) => json!({ "type": "signal", "signal": signal }),
        Expr::Subscription { source, target } => json!({
            "type": "subscription",
            "source": source,
            "signal": target
        }),
        Expr::Not(inner) => json!({ "type": "neg", "expr": expr_to_cloud_value(inner) }),
        Expr::And(terms) => fold_cloud_terms("and", terms),
        Expr::Or(terms) => fold_cloud_terms("or", terms),
        Expr::Delay {
            expr,
            raw_duration,
            duration_seconds,
        } => json!({
            "type": "delay",
            "expr": expr_to_cloud_value(expr),
            "rawDuration": raw_duration,
            "durationSeconds": duration_seconds,
        }),
    }
}

fn fold_cloud_terms(kind: &str, terms: &[Expr]) -> Value {
    // 分治平衡折叠：AST 会被 serde_json 以递归下降反序列化（默认 128 层），
    // 左斜折叠把 n 项链折成深度 n-1 的树，126 项即编译合法、求值必败的
    // 毒钩子。平衡树深度 O(log n)，任意合法项数都在反序列化限界内。
    fn fold_balanced(kind: &str, terms: &[Expr]) -> Value {
        match terms.len() {
            0 => Value::Null,
            1 => expr_to_cloud_value(&terms[0]),
            _ => {
                let mid = terms.len() / 2;
                let (left, right) = terms.split_at(mid);
                json!({
                    "type": kind,
                    "left": fold_balanced(kind, left),
                    "right": fold_balanced(kind, right),
                })
            }
        }
    }
    fold_balanced(kind, terms)
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
            reason: Some(
                "subscription hooks are delivered per contributing event by the state machine"
                    .to_string(),
            ),
        }),
        Expr::Not(inner) => {
            let evaluated = eval_expr(inner, source, signals, now)?;
            match evaluated.state {
                EvalState::Ready | EvalState::Wait => Ok(InternalEval {
                    state: EvalState::Impossible,
                    anchors: Vec::new(),
                    ready_at: None,
                    reason: Some(format!(
                        "negated condition exists: {}",
                        normalize_tight(inner)
                    )),
                }),
                EvalState::Impossible | EvalState::NeedsMore => Ok(InternalEval {
                    state: EvalState::Ready,
                    anchors: Vec::new(),
                    ready_at: None,
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
                    reason: None,
                }),
                EvalState::Ready => {
                    let Some(anchor) = evaluated.anchors.iter().max().copied() else {
                        return Ok(InternalEval {
                            state: EvalState::NeedsMore,
                            anchors: Vec::new(),
                            ready_at: None,
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
                        Ok(InternalEval {
                            state: EvalState::Ready,
                            anchors: vec![ready_at],
                            ready_at: Some(ready_at),
                            reason: None,
                        })
                    } else {
                        Ok(InternalEval {
                            state: EvalState::Wait,
                            anchors: Vec::new(),
                            ready_at: Some(ready_at),
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
                    EvalState::Ready => anchors.extend(evaluated.anchors),
                }
            }
            if needs_more {
                return Ok(InternalEval {
                    state: EvalState::NeedsMore,
                    anchors: Vec::new(),
                    ready_at: None,
                    reason: None,
                });
            }
            if let Some(ready_at) = waits.into_iter().max() {
                return Ok(InternalEval {
                    state: EvalState::Wait,
                    anchors: Vec::new(),
                    ready_at: Some(ready_at),
                    reason: None,
                });
            }
            Ok(InternalEval {
                state: EvalState::Ready,
                ready_at: anchors.iter().max().copied(),
                anchors,
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
                        // Arrival-time causality: when several branches have
                        // already fired, the earliest RECEIVED signal is the
                        // cause; the winning branch keeps its own timer.
                        let better = ready.as_ref().is_none_or(|current| {
                            branch_anchor(&evaluated) < branch_anchor(current)
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
                    reason: None,
                });
            }
            if all_impossible && !has_open {
                return Ok(InternalEval {
                    state: EvalState::Impossible,
                    anchors: Vec::new(),
                    ready_at: None,
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
                reason: None,
            })
        }
    }
}

/// Earliest received time of an evaluated branch; the arrival-time tiebreak
/// for OR branches that have already fired.
fn branch_anchor(evaluated: &InternalEval) -> DateTime<Utc> {
    evaluated
        .anchors
        .iter()
        .copied()
        .min()
        .unwrap_or_else(|| evaluated.ready_at.unwrap_or(DateTime::<Utc>::MAX_UTC))
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
            reason: None,
        });
    }
    Ok(InternalEval {
        state: EvalState::NeedsMore,
        anchors: Vec::new(),
        ready_at: None,
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

struct Parser<'a> {
    input: &'a str,
    index: usize,
    depth: usize,
}

/// 递归下降深度上限。hook 表达式来自外部可填写的模板定义，无界嵌套
/// （深层括号或连续 `~`）会打满调用栈直接 abort 宿主进程——栈溢出不可被
/// catch_unwind 捕获，必须在解析期以普通错误拒绝。
impl<'a> Parser<'a> {
    fn new(input: &'a str) -> Self {
        Self {
            input,
            index: 0,
            depth: 0,
        }
    }

    fn guard_depth<T>(&mut self, parse: impl FnOnce(&mut Self) -> Result<T>) -> Result<T> {
        self.depth += 1;
        if self.depth > MAX_PARSE_DEPTH {
            self.depth -= 1;
            return Err(HookError::Message(format!(
                "hook expression nesting exceeds the maximum depth of {MAX_PARSE_DEPTH}"
            )));
        }
        let result = parse(self);
        self.depth -= 1;
        result
    }

    fn parse(&mut self) -> Result<Expr> {
        let expr = self.guard_depth(|parser| parser.parse_or())?;
        self.skip_ws();
        if !self.at_end() {
            return Err(HookError::Message(format!(
                "unexpected token at {}: {}",
                self.index,
                &self.input[self.index..]
            )));
        }
        Ok(expr)
    }

    fn parse_or(&mut self) -> Result<Expr> {
        self.guard_depth(|parser| parser.parse_or_inner())
    }

    fn parse_or_inner(&mut self) -> Result<Expr> {
        let mut terms = vec![self.parse_and()?];
        while self.consume("|") {
            terms.push(self.parse_and()?);
        }
        Ok(if terms.len() == 1 {
            terms.remove(0)
        } else {
            Expr::Or(terms)
        })
    }

    fn parse_and(&mut self) -> Result<Expr> {
        let mut terms = vec![self.guard_depth(|parser| parser.parse_unary())?];
        while self.consume("&") {
            terms.push(self.guard_depth(|parser| parser.parse_unary())?);
        }
        Ok(if terms.len() == 1 {
            terms.remove(0)
        } else {
            Expr::And(terms)
        })
    }

    fn parse_unary(&mut self) -> Result<Expr> {
        self.skip_ws();
        if self.consume("~") {
            return Ok(Expr::Not(Box::new(
                self.guard_depth(|parser| parser.parse_unary())?,
            )));
        }
        self.parse_postfix()
    }

    fn parse_postfix(&mut self) -> Result<Expr> {
        let mut expr = self.parse_primary()?;
        self.skip_ws();
        if self.consume("+") {
            let raw_duration = self.read_duration()?;
            let duration_seconds = duration_to_seconds(&raw_duration)?;
            expr = Expr::Delay {
                expr: Box::new(expr),
                raw_duration,
                duration_seconds,
            };
        }
        Ok(expr)
    }

    fn parse_primary(&mut self) -> Result<Expr> {
        self.skip_ws();
        if self.consume("(") {
            let expr = self.parse_or()?;
            if !self.consume(")") {
                return Err(HookError::Message(format!(
                    "expected ')' at {}",
                    self.index
                )));
            }
            return Ok(expr);
        }

        let ident = self.read_identifier()?;
        match ident.as_str() {
            "ANCHOR" => self.parse_subscription(),
            "OUTSIDE" | "MERGE" | "OUTSOURCE" => Err(HookError::Message(format!(
                "{ident}@ has been retired: {RETIRED_KEYWORDS_HINT}"
            ))),
            _ => {
                if !is_strict_signal_ref(&ident) {
                    return Err(HookError::Message(format!(
                        "signal reference must use task.stage.signal: {ident}"
                    )));
                }
                Ok(Expr::Signal(ident))
            }
        }
    }

    /// 订阅通道：`ANCHOR(@source::task.stage.signal)`。旧 `ANCHOR@(裸三段)`
    /// 标头形态已退役；目标必须携带 @ 前缀的 source 类名空间。
    fn parse_subscription(&mut self) -> Result<Expr> {
        self.skip_ws();
        if self.peek() == '@' {
            return Err(HookError::Message(format!(
                "ANCHOR@ header form has been retired: {RETIRED_KEYWORDS_HINT}"
            )));
        }
        if !self.consume("(") {
            return Err(HookError::Message(format!(
                "expected '(' after ANCHOR at {}",
                self.index
            )));
        }
        let target_raw = self.read_balanced_target()?;
        let target = target_raw.strip_prefix('@').map(str::trim).ok_or_else(|| {
            HookError::Message(format!(
                "subscription target must be @source::task.stage.signal: {target_raw:?}"
            ))
        })?;
        let (source, signal) = target.split_once("::").ok_or_else(|| {
            HookError::Message(format!(
                "subscription target must be @source::task.stage.signal: {target_raw:?}"
            ))
        })?;
        let source = source.trim();
        let signal = signal.trim();
        // source 类命名空间复用普通标识符扫描规则：字符集 [A-Za-z0-9_-]，
        // 拒绝空格、括号、额外 :: 分隔与非 ASCII 字符。
        if !is_plain_identifier(source) {
            return Err(HookError::Message(format!(
                "subscription source must be a plain identifier: {source:?}"
            )));
        }
        // signal 全名落 signal_name 列（VARCHAR(100)），与普通标识符扫描同限。
        if signal.len() > 100 {
            return Err(HookError::Message(format!(
                "subscription target signal exceeds the maximum length of 100 characters: {signal:?}"
            )));
        }
        let segments = signal.split('.').collect::<Vec<_>>();
        if segments.len() != 3 || !segments.iter().all(|part| is_plain_identifier(part)) {
            return Err(HookError::Message(format!(
                "subscription target must use task.stage.signal: {signal:?}"
            )));
        }
        Ok(Expr::Subscription {
            source: source.to_string(),
            target: signal.to_string(),
        })
    }

    fn read_balanced_target(&mut self) -> Result<String> {
        let mut depth = 1;
        let start = self.index;
        while !self.at_end() {
            let ch = self.peek();
            self.index += ch.len_utf8();
            if ch == '(' {
                depth += 1;
            } else if ch == ')' {
                depth -= 1;
                if depth == 0 {
                    return Ok(self.input[start..self.index - 1].trim().to_string());
                }
            }
        }
        Err(HookError::Message("unterminated @() target".to_string()))
    }

    fn read_duration(&mut self) -> Result<String> {
        self.skip_ws();
        let start = self.index;
        while !self.at_end() {
            let ch = self.peek();
            if ch.is_ascii_digit() || matches!(ch, 's' | 'm' | 'h' | 'd') {
                self.index += ch.len_utf8();
            } else {
                break;
            }
        }
        let duration = self.input[start..self.index].to_string();
        if duration.is_empty() {
            return Err(HookError::Message("invalid duration: <empty>".to_string()));
        }
        duration_to_seconds(&duration)?;
        Ok(duration)
    }

    fn read_identifier(&mut self) -> Result<String> {
        self.skip_ws();
        let start = self.index;
        while !self.at_end() {
            let ch = self.peek();
            if ch.is_ascii_alphanumeric() || matches!(ch, '_' | '.' | '-') {
                self.index += ch.len_utf8();
            } else {
                break;
            }
        }
        if start == self.index {
            return Err(HookError::Message(format!(
                "expected identifier at {}",
                self.index
            )));
        }
        let ident = &self.input[start..self.index];
        // 标识符整体落 signal_name 列（task.stage.signal 全名，VARCHAR(100)）。
        if ident.len() > 100 {
            return Err(HookError::Message(format!(
                "identifier exceeds the maximum length of 100 characters: {}…",
                &ident[..32]
            )));
        }
        Ok(ident.to_string())
    }

    fn consume(&mut self, value: &str) -> bool {
        self.skip_ws();
        if self.input[self.index..].starts_with(value) {
            self.index += value.len();
            true
        } else {
            false
        }
    }

    fn skip_ws(&mut self) {
        while !self.at_end() {
            let ch = self.peek();
            if ch.is_whitespace() {
                self.index += ch.len_utf8();
            } else {
                break;
            }
        }
    }

    fn peek(&self) -> char {
        self.input[self.index..].chars().next().unwrap_or('\0')
    }

    fn at_end(&self) -> bool {
        self.index >= self.input.len()
    }
}

fn contains_nested_subscription(expr: &Expr) -> bool {
    match expr {
        Expr::Subscription { .. } => true,
        Expr::Signal(_) => false,
        Expr::Not(inner) | Expr::Delay { expr: inner, .. } => contains_nested_subscription(inner),
        Expr::And(terms) | Expr::Or(terms) => terms.iter().any(contains_nested_subscription),
    }
}

/// 延时操作数上限：30 天。超限在编译期直接拒绝，防止毒定义持久化。
const MAX_DELAY_SECONDS: i64 = 30 * 24 * 60 * 60;

fn duration_to_seconds(duration: &str) -> Result<i64> {
    if duration.len() < 2 {
        return Err(HookError::Message(format!("invalid duration: {duration}")));
    }
    // 末位单位必须按字符边界截取：毒 AST/毒输入可能携带多字节 UTF-8 结尾
    // （如 "1ü"，编译 cloud AST 时 rawDuration 来自外部 JSON），按字节
    // split_at 会在非边界处 panic——这里取最后一个 char，非 ASCII 单位字母
    // 一律返回确定性错误（有界失败，绝不 panic）。
    let (num, unit) = match duration.char_indices().next_back() {
        Some((index, unit)) if unit.is_ascii() => (&duration[..index], unit),
        _ => return Err(HookError::Message(format!("invalid duration: {duration}"))),
    };
    if num.starts_with('0') {
        return Err(HookError::Message(format!("invalid duration: {duration}")));
    }
    let value = num
        .parse::<i64>()
        .map_err(|err| HookError::Message(format!("invalid duration {duration}: {err}")))?;
    if value <= 0 {
        return Err(HookError::Message(format!("invalid duration: {duration}")));
    }
    let multiplier = match unit {
        's' => 1,
        'm' => 60,
        'h' => 60 * 60,
        'd' => 60 * 60 * 24,
        _ => return Err(HookError::Message(format!("invalid duration unit: {unit}"))),
    };
    let seconds = value
        .checked_mul(multiplier)
        .ok_or_else(|| HookError::Message(format!("duration is too large: {duration}")))?;
    if seconds > MAX_DELAY_SECONDS {
        return Err(HookError::Message(format!(
            "duration {duration} exceeds the maximum allowed delay of {MAX_DELAY_SECONDS}s (30d)"
        )));
    }
    Ok(seconds)
}

fn is_strict_signal_ref(value: &str) -> bool {
    let parts = value.split('.').collect::<Vec<_>>();
    parts.len() == 3 && parts.iter().all(|part| !part.is_empty())
}

/// 普通标识符扫描规则：非空，且仅 ASCII 字母/数字/下划线/中划线。
/// 订阅 target 的 source 类与 signal 各段均按此规则扫描。
fn is_plain_identifier(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn parse_value(raw: &str, profile: Profile, hook_name: &str) -> Value {
        let out = parse_hook(ParseHookRequest {
            profile,
            hook_name: hook_name.to_string(),
            hook: raw.to_string(),
        })
        .unwrap();
        serde_json::to_value(out).unwrap()
    }

    fn evaluate_compiled(
        hook_name: &str,
        hook: &str,
        profile: Profile,
        signals: Vec<SignalFact>,
        now: &str,
    ) -> EvalCompiledHookOutput {
        let parsed = parse_hook(ParseHookRequest {
            profile,
            hook_name: hook_name.to_string(),
            hook: hook.to_string(),
        })
        .unwrap();
        eval_compiled_hook(EvalCompiledHookRequest {
            profile,
            ast: parsed.cloud_ast,
            signals,
            now: now.to_string(),
        })
        .unwrap()
    }

    #[test]
    fn evm_strict_parses_and_evaluates_positive_signal() {
        let out = parse_value("buyer::task.main.cmp", Profile::EvmStrict, "TRIGGER");
        assert_eq!(out["normalizedExpression"], "buyer::task.main.cmp");
        assert_eq!(
            out["dependencies"],
            json!([{ "kind": "positive", "source": "buyer", "signalName": "task.main.cmp" }])
        );

        let eval = evaluate_compiled(
            "TRIGGER",
            "buyer::task.main.cmp",
            Profile::EvmStrict,
            vec![SignalFact {
                source: "buyer".to_string(),
                signal_name: "task.main.cmp".to_string(),
                received_at: "2026-04-27T00:00:00.900Z".to_string(),
            }],
            "2026-04-27T00:00:00.999Z",
        );
        assert_eq!(eval.state, EvalState::Ready);
        assert_eq!(eval.ready_at.as_deref(), Some("2026-04-27T00:00:00.000Z"));
    }

    #[test]
    fn rejects_deeply_nested_expressions_instead_of_overflowing() {
        let deep = format!("buyer::{}a{}", "(".repeat(50_000), ")".repeat(50_000));
        let err = parse_hook(ParseHookRequest {
            profile: Profile::EvmStrict,
            hook_name: "HOOK".to_string(),
            hook: deep,
        })
        .unwrap_err();
        assert!(err.to_string().contains("maximum depth of 256"));
    }

    #[test]
    fn retired_cross_source_keywords_fail_fast() {
        // 嵌套构造在最外层即命中退役报错：uvp.semantic.v1 不再解析四典型形态。
        let mut expression = "peer::task.main.cmp".to_string();
        for _ in 0..2_000 {
            expression = format!("::OUTSIDE@({expression})");
        }
        let err = parse_hook(ParseHookRequest {
            profile: Profile::EvmStrict,
            hook_name: "HOOK".to_string(),
            hook: expression,
        })
        .unwrap_err();
        assert!(err.to_string().contains("retired"), "unexpected: {err}");
    }

    #[test]
    fn rejects_deeply_nested_cloud_ast() {
        let mut root = json!({ "type": "signal", "signal": "task.main.cmp" });
        // Just past the guard threshold: deep enough to trip the depth cap,
        // shallow enough that serde_json's recursive Drop stays safe.
        for _ in 0..300 {
            root = json!({ "type": "neg", "expr": root });
        }
        let ast = json!({
            "schemaVersion": CLOUD_AST_SCHEMA_VERSION,
            "source": "buyer",
            "mode": "normal",
            "root": root
        });
        let err = eval_compiled_hook(EvalCompiledHookRequest {
            profile: Profile::EvmStrict,
            ast,
            signals: vec![],
            now: "2026-04-27T00:00:00.000Z".to_string(),
        })
        .unwrap_err();
        assert!(err.to_string().contains("maximum depth of 256"));
    }

    #[test]
    fn rejects_pure_negative_compiled_root() {
        let ast = json!({
            "schemaVersion": CLOUD_AST_SCHEMA_VERSION,
            "source": "buyer",
            "mode": "normal",
            "root": { "type": "neg", "expr": { "type": "signal", "signal": "task.cancel.cmp" } }
        });
        let err = eval_compiled_hook(EvalCompiledHookRequest {
            profile: Profile::EvmStrict,
            ast,
            signals: vec![],
            now: "2026-04-27T00:00:00.000Z".to_string(),
        })
        .unwrap_err();
        assert!(err
            .to_string()
            .contains("must contain at least one positive signal anchor"));
    }

    #[test]
    fn repeated_signals_keep_first_received_fact() {
        let parsed = parse_hook(ParseHookRequest {
            profile: Profile::EvmStrict,
            hook_name: "TRIGGER".to_string(),
            hook: "buyer::(task.pay.cmp +5s)".to_string(),
        })
        .unwrap();
        let eval = eval_compiled_hook(EvalCompiledHookRequest {
            profile: Profile::EvmStrict,
            ast: parsed.cloud_ast,
            signals: vec![
                SignalFact {
                    source: "buyer".to_string(),
                    signal_name: "task.pay.cmp".to_string(),
                    received_at: "2026-04-27T00:00:01.000Z".to_string(),
                },
                SignalFact {
                    source: "buyer".to_string(),
                    signal_name: "task.pay.cmp".to_string(),
                    received_at: "2026-04-27T00:00:10.000Z".to_string(),
                },
            ],
            now: "2026-04-27T00:00:06.000Z".to_string(),
        })
        .unwrap();
        // First received fact (00:00:01 + 5s) is already due at 00:00:06; a
        // last-writer-wins map would anchor at 00:00:10 and report wait.
        assert_eq!(eval.state, EvalState::Ready);
    }

    #[test]
    fn evm_strict_handles_delay_and_negative_guard() {
        let eval = evaluate_compiled(
            "TIMEOUT",
            "buyer::(task.pay.cmp +5s) & ~task.refund.cmp",
            Profile::EvmStrict,
            vec![SignalFact {
                source: "buyer".to_string(),
                signal_name: "task.pay.cmp".to_string(),
                received_at: "2026-04-27T00:00:00.900Z".to_string(),
            }],
            "2026-04-27T00:00:04.999Z",
        );
        assert_eq!(eval.state, EvalState::Wait);
        assert_eq!(eval.ready_at.as_deref(), Some("2026-04-27T00:00:05.000Z"));
    }

    #[test]
    fn cloud_ast_preserves_delay_operand_and_source() {
        let out = parse_hook(ParseHookRequest {
            profile: Profile::CloudCompat,
            hook_name: "TIMEOUT".to_string(),
            hook: "buyer::task.receive.cmp +14d".to_string(),
        })
        .unwrap();

        assert_eq!(out.cloud_ast["source"], json!("buyer"));
        assert_eq!(
            out.cloud_ast["schemaVersion"],
            json!(CLOUD_AST_SCHEMA_VERSION)
        );
        assert_eq!(out.cloud_ast["mode"], json!("normal"));
        assert_eq!(out.cloud_ast["root"]["type"], json!("delay"));
        assert_eq!(out.cloud_ast["root"].get("delay"), None);
        assert_eq!(out.cloud_ast["root"]["rawDuration"], json!("14d"));
        assert_eq!(
            out.cloud_ast["root"]["durationSeconds"],
            json!(14 * 24 * 60 * 60)
        );
    }

    #[test]
    fn delay_duration_above_30d_cap_is_rejected_at_parse_time() {
        let err = parse_hook(ParseHookRequest {
            profile: Profile::CloudCompat,
            hook_name: "TIMEOUT".to_string(),
            hook: "buyer::task.receive.cmp +31d".to_string(),
        })
        .unwrap_err();
        assert!(err.to_string().contains("30d"), "unexpected error: {err}");

        let boundary = parse_hook(ParseHookRequest {
            profile: Profile::CloudCompat,
            hook_name: "TIMEOUT".to_string(),
            hook: "buyer::task.receive.cmp +2592000s".to_string(),
        });
        assert!(boundary.is_ok(), "30d must stay accepted: {boundary:?}");
    }

    #[test]
    fn delay_ready_at_overflow_evaluates_to_error_instead_of_panic() {
        // 直接构造绕过编译期的毒 AST（历史持久化产物的形态：超大秒数与
        // 原始字面量自洽）。求值必须在解码期确定性拒绝并走有界失败路径，
        // 而不是 panic 跨 FFI 边界 abort 进程。
        let poisoned = json!({
            "schemaVersion": CLOUD_AST_SCHEMA_VERSION,
            "source": "buyer",
            "mode": "normal",
            "root": {
                "type": "delay",
                "expr": { "type": "signal", "signal": "task.receive.cmp" },
                "rawDuration": "9223372036854775807s",
                "durationSeconds": i64::MAX
            }
        });

        let err = eval_compiled_hook(EvalCompiledHookRequest {
            profile: Profile::CloudCompat,
            ast: poisoned,
            signals: vec![SignalFact {
                source: "buyer".to_string(),
                signal_name: "task.receive.cmp".to_string(),
                received_at: "2026-04-27T00:00:00.900Z".to_string(),
            }],
            now: "2026-04-27T00:00:01.000Z".to_string(),
        })
        .unwrap_err();
        assert!(err.to_string().contains("30d"), "unexpected error: {err}");
    }

    #[test]
    fn compiled_cloud_ast_evaluates_without_reparsing_source_expression() {
        let parsed = parse_hook(ParseHookRequest {
            profile: Profile::CloudCompat,
            hook_name: "TIMEOUT".to_string(),
            hook: "buyer::task.receive.cmp +14d".to_string(),
        })
        .unwrap();

        let eval = eval_compiled_hook(EvalCompiledHookRequest {
            profile: Profile::CloudCompat,
            ast: parsed.cloud_ast,
            signals: vec![SignalFact {
                source: "buyer".to_string(),
                signal_name: "task.receive.cmp".to_string(),
                received_at: "2026-04-01T00:00:00Z".to_string(),
            }],
            now: "2026-04-15T00:00:00Z".to_string(),
        })
        .unwrap();

        assert_eq!(eval.state, EvalState::Ready);
        assert_eq!(eval.ready_at.as_deref(), Some("2026-04-15T00:00:00.000Z"));
    }

    #[test]
    fn compiled_hook_evaluation_rejects_legacy_ast_shape() {
        let err = eval_compiled_hook(EvalCompiledHookRequest {
            profile: Profile::CloudCompat,
            ast: json!({
                "source": "buyer",
                "root": {
                    "type": "delay",
                    "expr": {"type": "signal", "signal": "task.receive.cmp"},
                    "delay": "14d"
                }
            }),
            signals: Vec::new(),
            now: "2026-04-15T00:00:00Z".to_string(),
        })
        .expect_err("legacy AST must not be evaluated");
        assert!(err.to_string().contains("schemaVersion"));
    }

    #[test]
    fn compiled_hook_evaluation_rejects_legacy_node_fields() {
        let err = eval_compiled_hook(EvalCompiledHookRequest {
            profile: Profile::CloudCompat,
            ast: json!({
                "schemaVersion": CLOUD_AST_SCHEMA_VERSION,
                "source": "buyer",
                "mode": "normal",
                "root": {
                    "type": "delay",
                    "expr": {"type": "signal", "signal": "task.receive.cmp"},
                    "delay": "14d"
                }
            }),
            signals: Vec::new(),
            now: "2026-04-15T00:00:00Z".to_string(),
        })
        .expect_err("legacy node fields must not be evaluated");
        assert!(err.to_string().contains("unsupported field: delay"));
    }

    #[test]
    fn cloud_compat_requires_full_signal_names() {
        let out = parse_value(
            "buyer::task.pay.cmp & ~task.refund.cmp",
            Profile::CloudCompat,
            "EXECUTE",
        );
        assert_eq!(out["runtimeCondition"], "task.pay.cmp & ~task.refund.cmp");
        assert_eq!(
            out["dependencies"],
            json!([
                { "kind": "negative", "source": "buyer", "signalName": "task.refund.cmp" },
                { "kind": "positive", "source": "buyer", "signalName": "task.pay.cmp" }
            ])
        );

        let err = parse_hook(ParseHookRequest {
            profile: Profile::CloudCompat,
            hook_name: "EXECUTE".to_string(),
            hook: "buyer::pay.cmp".to_string(),
        })
        .unwrap_err();
        assert!(err.to_string().contains("task.stage.signal"));

        let err = parse_hook(ParseHookRequest {
            profile: Profile::CloudCompat,
            hook_name: "TRIGGER".to_string(),
            hook: "::OUTSIDE".to_string(),
        })
        .unwrap_err();
        assert!(err.to_string().contains("retired"));
    }

    #[test]
    fn rejects_subscription_inside_composite_condition() {
        for hook in [
            "buyer::OUTSIDE & task.main.cmp",
            "buyer::task.main.cmp | OUTSIDE",
            "buyer::~OUTSIDE",
            "::MERGE & task.main.cmp",
        ] {
            let err = parse_hook(ParseHookRequest {
                profile: Profile::CloudCompat,
                hook_name: "HOOK".to_string(),
                hook: hook.to_string(),
            })
            .unwrap_err();
            assert!(
                err.to_string().contains("retired"),
                "unexpected error for {hook}: {err}"
            );
        }

        let err = parse_hook(ParseHookRequest {
            profile: Profile::CloudCompat,
            hook_name: "HOOK".to_string(),
            hook: "::ANCHOR(@seller::task.main.cmp) & task.other.cmp".to_string(),
        })
        .unwrap_err();
        assert!(
            err.to_string()
                .contains("must be the complete hook condition"),
            "unexpected error for a composite subscription: {err}"
        );

        let err = parse_hook(ParseHookRequest {
            profile: Profile::CloudCompat,
            hook_name: "HOOK".to_string(),
            hook: "buyer::ANCHOR(@seller::task.main.cmp)".to_string(),
        })
        .unwrap_err();
        assert!(
            err.to_string().contains("empty source header"),
            "expected headed subscription rejection: {err}"
        );
    }

    #[test]
    fn parser_rejects_unbounded_nesting_and_duplicate_merge_targets() {
        // 深度上限：深层括号与连续 ~ 都必须以普通错误拒绝，而不是打满
        // 调用栈 abort 宿主进程（栈溢出不可被 catch_unwind 捕获）。
        for poisoned in [
            format!("buyer::{}task.main.cmp{}", "(".repeat(256), ")".repeat(256)),
            format!("buyer::{}task.main.cmp", "~".repeat(256)),
        ] {
            let err = parse_hook(ParseHookRequest {
                profile: Profile::CloudCompat,
                hook_name: "HOOK".to_string(),
                hook: poisoned,
            })
            .unwrap_err();
            assert!(
                err.to_string().contains("maximum depth"),
                "expected depth-limit rejection: {err}"
            );
        }

        let err = parse_hook(ParseHookRequest {
            profile: Profile::CloudCompat,
            hook_name: "HOOK".to_string(),
            hook: "::ANCHOR(@seller::listing.cmp)".to_string(),
        })
        .unwrap_err();
        assert!(
            err.to_string().contains("task.stage.signal"),
            "unexpected error for short subscription target: {err}"
        );
    }

    #[test]
    fn outsource_is_retired_with_migration_hint() {
        for hook in [
            "::OUTSOURCE@(seller::task.main.cmp)",
            "buyer::OUTSOURCE@(seller::task.main.cmp)",
            "buyer::OUTSOURCE",
        ] {
            let err = parse_hook(ParseHookRequest {
                profile: Profile::CloudCompat,
                hook_name: "HOOK".to_string(),
                hook: hook.to_string(),
            })
            .unwrap_err();
            assert!(
                err.to_string().contains("retired"),
                "unexpected error for {hook}: {err}"
            );
        }
    }

    #[test]
    fn subscription_entry_parses_target_and_dependencies() {
        let out = parse_value(
            "::ANCHOR(@seller::trade.listing.cmp)",
            Profile::CloudCompat,
            "SUBSCRIBE",
        );
        assert_eq!(out["mode"], "subscription");
        assert_eq!(
            out["subscriptionTarget"],
            json!({ "source": "seller", "signalName": "trade.listing.cmp" })
        );
        assert_eq!(
            out["dependencies"],
            json!([
                { "kind": "positive", "source": "seller", "signalName": "trade.listing.cmp" }
            ])
        );
        assert_eq!(
            out["normalizedExpression"],
            "::ANCHOR(@seller::trade.listing.cmp)"
        );

        let cloud_ast = out["cloudAst"].clone();
        assert_eq!(cloud_ast["mode"], "subscription");
        assert_eq!(cloud_ast["source"], "");
        assert_eq!(
            cloud_ast["subscriptionTarget"],
            json!({ "source": "seller", "signal": "trade.listing.cmp" })
        );

        // 订阅钩子不经表达式裁决：求值器恒返回 NeedsMore，投递由状态机
        // 按接收方锚定状态路由（按单经对接记录，无锚按类扇入）。
        let eval = eval_compiled_hook(EvalCompiledHookRequest {
            profile: Profile::CloudCompat,
            ast: cloud_ast,
            signals: vec![],
            now: "2026-04-27T00:00:00Z".to_string(),
        })
        .unwrap();
        assert_eq!(eval.state, EvalState::NeedsMore);
        assert!(eval.reason.unwrap_or_default().contains("delivered per"));
    }

    #[test]
    fn subscription_entry_rejects_degenerate_shapes() {
        for hook in [
            "::ANCHOR()",
            "::ANCHOR(@)",
            "::ANCHOR(seller::trade.listing.cmp)",
            "::ANCHOR(@seller::trade.listing.cmp & buyer::trade.intent.cmp)",
            "::ANCHOR(@seller::listing.cmp)",
            "::ANCHOR(@::trade.listing.cmp)",
            "::ANCHOR(@seller names::trade.listing.cmp)",
            "wholesaler::ANCHOR(@seller::trade.listing.cmp)",
            "::ANCHOR@(farmer.main.settle)",
        ] {
            let err = parse_hook(ParseHookRequest {
                profile: Profile::CloudCompat,
                hook_name: "HOOK".to_string(),
                hook: hook.to_string(),
            })
            .unwrap_err();
            assert!(!err.to_string().is_empty(), "expected rejection for {hook}");
        }
    }

    #[test]
    fn retired_hook_modes_are_rejected_at_decode() {
        // 旧语义线的编译产物（outside_spawn/merge/anchor）在 uvp.semantic.v1 解码期确定性拒绝，
        // 不做兼容解释。
        for mode in ["outside_spawn", "merge", "anchor"] {
            let err = eval_compiled_hook(EvalCompiledHookRequest {
                profile: Profile::CloudCompat,
                ast: json!({
                    "schemaVersion": CLOUD_AST_SCHEMA_VERSION,
                    "source": "",
                    "mode": mode,
                    "root": { "type": "signal", "signal": "task.main.cmp" }
                }),
                signals: vec![],
                now: "2026-04-27T00:00:00Z".to_string(),
            })
            .expect_err("retired mode must not decode");
            assert!(
                err.to_string().contains("retired in uvp.semantic.v1"),
                "unexpected error for {mode}: {err}"
            );
        }
    }

    #[test]
    fn subscription_ast_without_target_is_rejected_at_decode() {
        let cloud_ast = json!({
            "schemaVersion": CLOUD_AST_SCHEMA_VERSION,
            "source": "",
            "mode": "subscription",
            "root": { "type": "subscription", "source": "seller", "signal": "trade.listing.cmp" }
        });
        let err = eval_compiled_hook(EvalCompiledHookRequest {
            profile: Profile::CloudCompat,
            ast: cloud_ast,
            signals: vec![],
            now: "2026-04-27T00:00:00Z".to_string(),
        })
        .expect_err("subscription AST without target must not be evaluated");
        assert!(
            err.to_string().contains("subscriptionTarget"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn mint_and_route_validation_matches_go_decode() {
        // normal 模式携带 mint/route：对齐 Go DecodeCompiledHook 一律拒绝。
        for (field, value) in [("mint", "per-fact"), ("route", "order")] {
            let mut ast = json!({
                "schemaVersion": CLOUD_AST_SCHEMA_VERSION,
                "source": "buyer",
                "mode": "normal",
                "root": { "type": "signal", "signal": "task.main.cmp" }
            });
            ast[field] = json!(value);
            let err = eval_compiled_hook(EvalCompiledHookRequest {
                profile: Profile::CloudCompat,
                ast,
                signals: vec![],
                now: "2026-04-27T00:00:00Z".to_string(),
            })
            .expect_err("normal-mode mint/route must not decode");
            assert!(
                err.to_string()
                    .contains("mint/route is only allowed on subscription mode"),
                "unexpected error for {field}: {err}"
            );
        }
        // subscription 模式：合法组合放行，非法取值确定性拒绝。
        let legal = json!({
            "schemaVersion": CLOUD_AST_SCHEMA_VERSION,
            "source": "",
            "mode": "subscription",
            "subscriptionTarget": { "source": "seller", "signal": "trade.listing.cmp" },
            "mint": "per-fact",
            "route": "fanin",
            "root": { "type": "subscription", "source": "seller", "signal": "trade.listing.cmp" }
        });
        let eval = eval_compiled_hook(EvalCompiledHookRequest {
            profile: Profile::CloudCompat,
            ast: legal,
            signals: vec![],
            now: "2026-04-27T00:00:00Z".to_string(),
        })
        .unwrap();
        assert_eq!(eval.state, EvalState::NeedsMore);

        let poisoned_mint = json!({
            "schemaVersion": CLOUD_AST_SCHEMA_VERSION,
            "source": "",
            "mode": "subscription",
            "subscriptionTarget": { "source": "seller", "signal": "trade.listing.cmp" },
            "mint": "bulk",
            "root": { "type": "subscription", "source": "seller", "signal": "trade.listing.cmp" }
        });
        let err = eval_compiled_hook(EvalCompiledHookRequest {
            profile: Profile::CloudCompat,
            ast: poisoned_mint,
            signals: vec![],
            now: "2026-04-27T00:00:00Z".to_string(),
        })
        .expect_err("non per-fact mint must not decode");
        assert!(
            err.to_string().contains("mint only supports per-fact"),
            "unexpected error: {err}"
        );

        let poisoned_route = json!({
            "schemaVersion": CLOUD_AST_SCHEMA_VERSION,
            "source": "",
            "mode": "subscription",
            "subscriptionTarget": { "source": "seller", "signal": "trade.listing.cmp" },
            "route": "broadcast",
            "root": { "type": "subscription", "source": "seller", "signal": "trade.listing.cmp" }
        });
        let err = eval_compiled_hook(EvalCompiledHookRequest {
            profile: Profile::CloudCompat,
            ast: poisoned_route,
            signals: vec![],
            now: "2026-04-27T00:00:00Z".to_string(),
        })
        .expect_err("unknown route must not decode");
        assert!(
            err.to_string().contains("route is invalid"),
            "unexpected error: {err}"
        );

        // 非字符串 mint/route 与 Go 的类型解码一致地拒绝。
        let poisoned_type = json!({
            "schemaVersion": CLOUD_AST_SCHEMA_VERSION,
            "source": "",
            "mode": "subscription",
            "subscriptionTarget": { "source": "seller", "signal": "trade.listing.cmp" },
            "mint": 5,
            "root": { "type": "subscription", "source": "seller", "signal": "trade.listing.cmp" }
        });
        let err = eval_compiled_hook(EvalCompiledHookRequest {
            profile: Profile::CloudCompat,
            ast: poisoned_type,
            signals: vec![],
            now: "2026-04-27T00:00:00Z".to_string(),
        })
        .expect_err("non-string mint must not decode");
        assert!(
            err.to_string().contains("mint must be a string"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn subscription_ast_with_nonempty_source_is_rejected_at_decode() {
        let cloud_ast = json!({
            "schemaVersion": CLOUD_AST_SCHEMA_VERSION,
            "source": "buyer",
            "mode": "subscription",
            "subscriptionTarget": { "source": "seller", "signal": "trade.listing.cmp" },
            "root": { "type": "subscription", "source": "seller", "signal": "trade.listing.cmp" }
        });
        let err = eval_compiled_hook(EvalCompiledHookRequest {
            profile: Profile::CloudCompat,
            ast: cloud_ast,
            signals: vec![],
            now: "2026-04-27T00:00:00Z".to_string(),
        })
        .expect_err("subscription AST with headed source must not decode");
        assert!(
            err.to_string().contains("source must be empty"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn subscription_ast_target_root_mismatch_is_rejected_at_decode() {
        let cloud_ast = json!({
            "schemaVersion": CLOUD_AST_SCHEMA_VERSION,
            "source": "",
            "mode": "subscription",
            "subscriptionTarget": { "source": "seller", "signal": "trade.listing.cmp" },
            "root": { "type": "subscription", "source": "seller", "signal": "trade.intent.cmp" }
        });
        let err = eval_compiled_hook(EvalCompiledHookRequest {
            profile: Profile::CloudCompat,
            ast: cloud_ast,
            signals: vec![],
            now: "2026-04-27T00:00:00Z".to_string(),
        })
        .expect_err("mismatched subscriptionTarget/root must not decode");
        assert!(
            err.to_string()
                .contains("subscriptionTarget does not match the root subscription node"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn rejects_invalid_strict_signal_names() {
        for profile in [Profile::CloudCompat, Profile::EvmStrict] {
            for hook in [
                "buyer::cmp",
                "buyer::main.cmp",
                "buyer::task.stage.signal.extra",
                "buyer::task..cmp",
            ] {
                let err = parse_hook(ParseHookRequest {
                    profile,
                    hook_name: "HOOK".to_string(),
                    hook: hook.to_string(),
                })
                .unwrap_err();
                assert!(
                    err.to_string().contains("task.stage.signal"),
                    "unexpected error for {hook}: {err}"
                );
            }
        }
    }

    #[test]
    fn rejects_duration_overflow() {
        let err = parse_hook(ParseHookRequest {
            profile: Profile::CloudCompat,
            hook_name: "TIMEOUT".to_string(),
            hook: "buyer::task.receive.cmp +9223372036854775807d".to_string(),
        })
        .expect_err("duration overflow must be rejected");
        assert!(err.to_string().contains("duration is too large"));
    }

    #[test]
    fn or_branches_resolve_by_earliest_received_signal() {
        let parsed = parse_hook(ParseHookRequest {
            profile: Profile::EvmStrict,
            hook_name: "TRIGGER".to_string(),
            hook: "buyer::(task.pay.cmp | task.ship.cmp) +5s".to_string(),
        })
        .unwrap();
        let eval = eval_compiled_hook(EvalCompiledHookRequest {
            profile: Profile::EvmStrict,
            ast: parsed.cloud_ast,
            signals: vec![
                SignalFact {
                    source: "buyer".to_string(),
                    signal_name: "task.pay.cmp".to_string(),
                    received_at: "2026-04-27T00:01:40.000Z".to_string(),
                },
                SignalFact {
                    source: "buyer".to_string(),
                    signal_name: "task.ship.cmp".to_string(),
                    received_at: "2026-04-27T00:00:01.000Z".to_string(),
                },
            ],
            now: "2026-04-27T00:00:06.000Z".to_string(),
        })
        .unwrap();
        assert_eq!(eval.state, EvalState::Ready);
        assert_eq!(eval.ready_at.as_deref(), Some("2026-04-27T00:00:06.000Z"));
    }

    #[test]
    fn or_anchor_uses_arrival_not_expression_order_without_delay() {
        let parsed = parse_hook(ParseHookRequest {
            profile: Profile::EvmStrict,
            hook_name: "TRIGGER".to_string(),
            hook: "buyer::(task.pay.cmp | task.ship.cmp)".to_string(),
        })
        .unwrap();
        let eval = eval_compiled_hook(EvalCompiledHookRequest {
            profile: Profile::EvmStrict,
            ast: parsed.cloud_ast,
            signals: vec![
                SignalFact {
                    source: "buyer".to_string(),
                    signal_name: "task.pay.cmp".to_string(),
                    received_at: "2026-04-27T00:09:00.000Z".to_string(),
                },
                SignalFact {
                    source: "buyer".to_string(),
                    signal_name: "task.ship.cmp".to_string(),
                    received_at: "2026-04-27T00:00:30.000Z".to_string(),
                },
            ],
            now: "2026-04-27T00:09:01.000Z".to_string(),
        })
        .unwrap();
        assert_eq!(eval.state, EvalState::Ready);
        assert_eq!(eval.ready_at.as_deref(), Some("2026-04-27T00:00:30.000Z"));
    }

    #[test]
    fn duration_with_multibyte_tail_fails_bounded_instead_of_panicking() {
        // 毒输入回归：多字节 UTF-8 结尾曾按字节 split_at 在非字符边界 panic
        // （有界失败纪律：毒 AST/毒输入必须返回确定性错误，绝不 panic）。
        for raw in ["ü", "1ü", "10ü"] {
            let err = duration_to_seconds(raw).unwrap_err();
            assert!(
                err.to_string().contains("invalid duration"),
                "unexpected error for {raw:?}: {err}"
            );
        }
        // 非 ASCII 单位字母（非 s/m/h/d）同样是确定性错误。
        assert!(duration_to_seconds("10x").is_err());
        // 正常单位不受影响。
        assert_eq!(duration_to_seconds("48h").unwrap(), 48 * 60 * 60);
        assert_eq!(duration_to_seconds("30d").unwrap(), 30 * 24 * 60 * 60);
    }

    #[test]
    fn non_subscription_source_header_requires_plain_identifier_of_at_most_36() {
        // 标头 source 类落 VARCHAR(36) 且是路由键：超长/非法字符集在解析期
        // 拒绝（对齐 Go 镜像 zhixu_schema.go 的 ≤36 与标识符规则）。
        let overlong = "s".repeat(37);
        for raw in [
            format!("{overlong}::task.main.cmp"),
            "has space::task.main.cmp".to_string(),
            "buyer.ü::task.main.cmp".to_string(),
        ] {
            let err = parse_hook(ParseHookRequest {
                profile: Profile::EvmStrict,
                hook_name: "HOOK".to_string(),
                hook: raw.clone(),
            })
            .unwrap_err();
            assert!(
                err.to_string().contains("source"),
                "unexpected error for {raw:?}: {err}"
            );
        }
        // 36 字节边界恰好放行。
        let boundary = "s".repeat(36);
        parse_hook(ParseHookRequest {
            profile: Profile::EvmStrict,
            hook_name: "HOOK".to_string(),
            hook: format!("{boundary}::task.main.cmp"),
        })
        .unwrap();
    }

    #[test]
    fn subscription_header_form_is_unaffected_by_source_header_cap() {
        // 订阅形态标头恒为空：不受 ≤36/plain-identifier 标头校验影响，
        // 订阅目标 source 仍走 is_plain_identifier/100 既有校验。
        parse_hook(ParseHookRequest {
            profile: Profile::EvmStrict,
            hook_name: "HOOK".to_string(),
            hook: "::ANCHOR(@seller::task.main.cmp)".to_string(),
        })
        .unwrap();
        // 订阅条目带非空标头仍按既有口径拒绝（而非新的标头错误）。
        let err = parse_hook(ParseHookRequest {
            profile: Profile::EvmStrict,
            hook_name: "HOOK".to_string(),
            hook: "buyer::ANCHOR(@seller::task.main.cmp)".to_string(),
        })
        .unwrap_err();
        assert!(
            err.to_string()
                .contains("subscription entries must use an empty source header"),
            "unexpected: {err}"
        );
    }
}
