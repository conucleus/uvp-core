use chrono::{DateTime, SecondsFormat, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use thiserror::Error;

pub const CORE_VERSION: &str = env!("CARGO_PKG_VERSION");
pub const SEMANTIC_VERSION: &str = "uvp-semantic/0.4";
pub const CLOUD_AST_SCHEMA_VERSION: &str = "uvp/cloud-ast/v1";

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
    External {
        mode: ExternalMode,
        target: Box<HookExpr>,
    },
    /// 撮合入口：k≥1 路独立上游信号逐事件携带溯源转交静态执行器，判定权归执行器。
    /// k≥2 含跨源配对；k=1 退化为跨订单观察/聚合入口（单 source 的多张订单逐
    /// 事件转交，"收到几个、收齐没有"仍由执行器裁决）。分馏 OUTSIDE@ 是同一
    /// 管道上判定谓词退化为恒等、连建单都由引擎代行的极限形态。
    Merge {
        targets: Vec<HookExpr>,
    },
    /// 收购回流入口：当前订单的直接子订单发生目标信号时，逐事件投递给锚定阶段
    /// 静态执行器；血缘（rel_order_order）是构成性过滤，聚合裁决归锚定执行器。
    Anchor {
        target: Box<HookExpr>,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExternalMode {
    #[serde(rename = "OUTSIDE")]
    Outside,
}

impl ExternalMode {
    fn as_str(self) -> &'static str {
        match self {
            Self::Outside => "OUTSIDE",
        }
    }
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upstream_source: Option<String>,
    pub raw_hook: String,
    pub raw_condition: String,
    pub runtime_condition: String,
    pub normalized_expression: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub merge_targets: Option<Vec<MergeTarget>>,
    pub dependencies: Vec<Dependency>,
    pub ast: Value,
    pub cloud_ast: Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MergeTarget {
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
    OutsideSpawn,
    Merge,
    Anchor,
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

pub fn compile_json(input: &str) -> String {
    parse_hook_json(input)
}

pub fn replay_json(_input: &str) -> String {
    envelope_json::<Value>(Err(HookError::Message(
        "uvp-replay is not implemented in this initial core cut".to_string(),
    )))
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
    let upstream_source = upstream_source(&hook.condition);
    let compatibility = compatibility_for(&hook, profile);
    let runtime_condition = runtime_condition(&hook, &hook_name, profile)?;
    let normalized_expression = format!(
        "{}::{}",
        hook.source,
        normalize_condition(&hook.condition, NormalizeStyle::Tight)
    );
    let dependencies = extract_dependencies(&hook, profile);
    let cloud_ast = cloud_ast_for(&hook, &hook_name, profile)?;
    let merge_targets = match &hook.condition {
        Expr::Merge { targets } => Some(
            targets
                .iter()
                .map(|target| MergeTarget {
                    source: target.source.clone(),
                    signal_name: merge_target_signal(target),
                })
                .collect::<Vec<_>>(),
        ),
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
        upstream_source,
        raw_hook: req.hook,
        raw_condition,
        runtime_condition,
        normalized_expression,
        merge_targets,
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
            "upstreamSource",
            "mergeTargets",
            "anchorTarget",
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
    if !matches!(mode, "normal" | "outside_spawn" | "merge" | "anchor") {
        return Err(HookError::Message(format!(
            "unsupported compiled hook AST mode: {mode}"
        )));
    }
    if mode == "outside_spawn"
        && req
            .ast
            .get("upstreamSource")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .is_none()
    {
        return Err(HookError::Message(
            "compiled external hook AST is missing upstreamSource".to_string(),
        ));
    }
    if mode == "anchor" {
        let has_target = req
            .ast
            .get("anchorTarget")
            .and_then(|target| target.get("signal"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .is_some();
        if !has_target {
            return Err(HookError::Message(
                "compiled anchor hook AST is missing anchorTarget.signal".to_string(),
            ));
        }
    }
    let now = parse_time(&req.now, req.profile)?;
    // 收购回流钩子标头恒为空：投递目标由阶段静态执行器决定，事件来源由
    // 状态机按血缘过滤，因此仅 anchor 模式允许空 source。
    let source = req
        .ast
        .get("source")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty() || mode == "anchor")
        .map(ToOwned::to_owned)
        .ok_or_else(|| HookError::Message("compiled hook AST is missing source".to_string()))?;
    let root = req
        .ast
        .get("root")
        .ok_or_else(|| HookError::Message("compiled hook AST root is missing".to_string()))?;
    let expr = expr_from_cloud_value(root)?;
    // 求值器是解码层最后一道防线：root 形态必须与 mode 一致，布尔树内部
    // 不得再嵌套跨源节点——两者都只能由手写毒 AST 构造，解析器产不出
    // （解析期位置约束见 validate_external_position）。
    match mode {
        "merge" if !matches!(expr, Expr::Merge { .. }) => {
            return Err(HookError::Message(
                "compiled merge hook AST root must be a merge node".to_string(),
            ));
        }
        "anchor" if !matches!(expr, Expr::Anchor { .. }) => {
            return Err(HookError::Message(
                "compiled anchor hook AST root must be an anchor node".to_string(),
            ));
        }
        "outside_spawn" if !matches!(expr, Expr::External { .. }) => {
            return Err(HookError::Message(
                "compiled external hook AST root must be an external node".to_string(),
            ));
        }
        "normal" if contains_nested_external(&expr) => {
            return Err(HookError::Message(
                "compiled normal hook AST must not contain cross-source nodes".to_string(),
            ));
        }
        _ => {}
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
        reason: result.reason,
    })
}

fn signal_map(signals: Vec<SignalFact>, profile: Profile) -> Result<BTreeMap<String, SignalEntry>> {
    let mut result = BTreeMap::new();
    for signal in signals {
        let received_at = parse_time(&signal.received_at, profile)?;
        result.insert(
            signal_key(&signal.source, &signal.signal_name),
            SignalEntry {
                source: signal.source,
                signal_name: signal.signal_name,
                received_at,
            },
        );
    }
    Ok(result)
}

fn parse_hook_expr(raw: &str, profile: Profile) -> Result<HookExpr> {
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
    if source.is_empty() && !starts_external(condition_raw) {
        return Err(HookError::Message(
            "empty source is only allowed for OUTSIDE@(…), MERGE@(…) or ANCHOR@(…) hooks"
                .to_string(),
        ));
    }    reject_unsupported_operators(condition_raw)?;
    let mut parser = Parser::new(condition_raw, profile);
    let condition = parser.parse()?;
    validate_external_position(&condition, true)?;
    if matches!(
        condition,
        Expr::Merge { .. } | Expr::Anchor { .. } | Expr::External { .. }
    ) && !source.is_empty()
    {
        return Err(HookError::Message(
            "MERGE@/ANCHOR@/OUTSIDE@ entries must use an empty source header: ::MERGE@(a::task.stage.signal, ...), ::ANCHOR@(task.stage.signal) or ::OUTSIDE@(source::task.stage.signal)".to_string(),
        ));
    }
    Ok(HookExpr {
        raw: raw.to_string(),
        source,
        condition,
    })
}

fn expr_from_cloud_value(value: &Value) -> Result<Expr> {
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
        "merge" => {
            reject_unknown_keys(
                value.as_object().ok_or_else(|| {
                    HookError::Message("compiled merge AST node must be an object".to_string())
                })?,
                &["type", "targets"],
                "compiled merge AST node",
            )?;
            let targets = value
                .get("targets")
                .and_then(Value::as_array)
                .ok_or_else(|| {
                    HookError::Message("compiled merge AST node is missing targets".to_string())
                })?;
            // 求值器只做表达式裁决，从不执行投递：作为"汇合表达式"，少于两路
            // 操作数没有求值意义。k=1 的运行时投递形态（跨订单观察入口）由
            // 状态机在 decode 层短路处理，不经过这里。
            if targets.len() < 2 {
                return Err(HookError::Message(
                    "compiled merge AST node requires at least two targets".to_string(),
                ));
            }
            let mut parsed = Vec::new();
            for target in targets {
                let source = target
                    .get("source")
                    .and_then(Value::as_str)
                    .filter(|source| !source.trim().is_empty())
                    .ok_or_else(|| {
                        HookError::Message(
                            "compiled merge AST target is missing source".to_string(),
                        )
                    })?;
                let signal = target
                    .get("signal")
                    .and_then(Value::as_str)
                    .filter(|signal| !signal.trim().is_empty())
                    .ok_or_else(|| {
                        HookError::Message(
                            "compiled merge AST target is missing signal".to_string(),
                        )
                    })?;
                parsed.push(HookExpr {
                    raw: format!("{source}::{signal}"),
                    source: source.to_string(),
                    condition: Expr::Signal(signal.to_string()),
                });
            }
            Ok(Expr::Merge { targets: parsed })
        }
        "anchor" => {
            reject_unknown_keys(
                value.as_object().ok_or_else(|| {
                    HookError::Message("compiled anchor AST node must be an object".to_string())
                })?,
                &["type", "signal"],
                "compiled anchor AST node",
            )?;
            let signal = value
                .get("signal")
                .and_then(Value::as_str)
                .filter(|signal| !signal.trim().is_empty())
                .ok_or_else(|| {
                    HookError::Message("compiled anchor AST node is missing signal".to_string())
                })?;
            Ok(Expr::Anchor {
                target: Box::new(HookExpr {
                    raw: signal.to_string(),
                    source: String::new(),
                    condition: Expr::Signal(signal.to_string()),
                }),
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
            Ok(Expr::Not(Box::new(expr_from_cloud_value(
                value.get("expr").ok_or_else(|| {
                    HookError::Message("compiled neg AST node is missing expr".to_string())
                })?,
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
            let left = expr_from_cloud_value(value.get("left").ok_or_else(|| {
                HookError::Message("compiled boolean AST node is missing left".to_string())
            })?)?;
            let right = expr_from_cloud_value(value.get("right").ok_or_else(|| {
                HookError::Message("compiled boolean AST node is missing right".to_string())
            })?)?;
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
            let expr = expr_from_cloud_value(value.get("expr").ok_or_else(|| {
                HookError::Message("compiled delay AST node is missing expr".to_string())
            })?)?;
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

// External operators are cross-source wrappers, not backend/executor input
// declarations. Backend/executor external inputs are declared by the
// surrounding Stage contract and are sent to UVP only when backend explicitly
// chooses to do so.
fn validate_external_position(expr: &Expr, root: bool) -> Result<()> {
    match expr {
        Expr::External { .. } | Expr::Merge { .. } | Expr::Anchor { .. } => {
            if !root {
                return Err(HookError::Message(
                    "OUTSIDE/MERGE/ANCHOR must be the complete hook condition; declare backend external inputs in externalSignals and reference the canonical signal in a normal expression".to_string(),
                ));
            }
            Ok(())
        }
        Expr::Signal(_) => Ok(()),
        Expr::Not(inner) => validate_external_position(inner, false),
        Expr::And(terms) | Expr::Or(terms) => {
            for term in terms {
                validate_external_position(term, false)?;
            }
            Ok(())
        }
        Expr::Delay { expr, .. } => validate_external_position(expr, false),
    }
}

fn starts_external(value: &str) -> bool {
    // OUTSOURCE 仍放行进解析器，以便空标头形态也能命中退役提示而非笼统的
    // 空标头报错。
    value.starts_with("OUTSIDE")
        || value.starts_with("MERGE")
        || value.starts_with("ANCHOR")
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
        Expr::Signal(_)
        | Expr::External { .. }
        | Expr::Merge { .. }
        | Expr::Anchor { .. } => Ok(true),
        Expr::Not(inner) => {
            if profile == Profile::CloudCompat && !matches!(inner.as_ref(), Expr::Signal(_)) {
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
        Expr::Signal(_)
        | Expr::External { .. }
        | Expr::Merge { .. }
        | Expr::Anchor { .. } => true,
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
        Expr::External { mode, target } => {
            format!("{}@({})", mode.as_str(), normalize_hook_tight(target))
        }
        Expr::Merge { targets } => format!(
            "MERGE@({})",
            targets
                .iter()
                .map(normalize_hook_tight)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Expr::Anchor { target } => {
            format!("ANCHOR@({})", normalize_tight(&target.condition))
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

fn normalize_hook_tight(hook: &HookExpr) -> String {
    format!("{}::{}", hook.source, normalize_tight(&hook.condition))
}

fn normalize_for_unary_tight(expr: &Expr) -> String {
    match expr {
        Expr::Signal(_) | Expr::External { .. } | Expr::Merge { .. } => normalize_tight(expr),
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
        Expr::External { mode, target } => {
            format!("{}@({})", mode.as_str(), normalize_hook_tight(target))
        }
        Expr::Merge { targets } => format!(
            "MERGE@({})",
            targets
                .iter()
                .map(normalize_hook_tight)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Expr::Anchor { target } => {
            format!("ANCHOR@({})", normalize_tight(&target.condition))
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
        Expr::Signal(_)
        | Expr::External { .. }
        | Expr::Merge { .. }
        | Expr::Anchor { .. } => 4,
    }
}

fn runtime_condition(hook: &HookExpr, _hook_name: &str, profile: Profile) -> Result<String> {
    if profile == Profile::EvmStrict {
        return Ok(normalize_condition(&hook.condition, NormalizeStyle::Tight));
    }
    match &hook.condition {
        Expr::External { target, .. } => Ok(normalize_condition(
            &target.condition,
            NormalizeStyle::Cloud,
        )),
        Expr::Merge { .. } | Expr::Anchor { .. } => {
            Ok(normalize_condition(&hook.condition, NormalizeStyle::Tight))
        }
        _ => Ok(normalize_condition(&hook.condition, NormalizeStyle::Cloud)),
    }
}

fn hook_mode(expr: &Expr) -> HookMode {
    match expr {
        Expr::External {
            mode: ExternalMode::Outside,
            ..
        } => HookMode::OutsideSpawn,
        Expr::Merge { .. } => HookMode::Merge,
        Expr::Anchor { .. } => HookMode::Anchor,
        _ => HookMode::Normal,
    }
}

fn upstream_source(expr: &Expr) -> Option<String> {
    match expr {
        Expr::External { target, .. } => Some(target.source.clone()),
        _ => None,
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
        Expr::External { target, .. } => {
            collect_dependencies(&target.condition, &target.source, negated, out);
        }
        Expr::Merge { targets } => {
            for target in targets {
                collect_dependencies(&target.condition, &target.source, negated, out);
            }
        }
        Expr::Anchor { target } => {
            collect_dependencies(&target.condition, &target.source, negated, out);
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
        Expr::External { mode, target } => {
            json!({ "kind": "external", "mode": mode.as_str(), "target": hook_to_value(target) })
        }
        Expr::Merge { targets } => json!({
            "kind": "merge",
            "targets": targets.iter().map(hook_to_value).collect::<Vec<_>>()
        }),
        Expr::Anchor { target } => {
            json!({ "kind": "anchor", "target": hook_to_value(target) })
        }
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
        Expr::External { target, .. } => Ok(json!({
            "schemaVersion": CLOUD_AST_SCHEMA_VERSION,
            "source": target.source.clone(),
            "mode": HookMode::OutsideSpawn,
            "upstreamSource": target.source,
            "root": expr_to_cloud_value(&target.condition)
        })),
        Expr::Merge { targets } => Ok(json!({
            "schemaVersion": CLOUD_AST_SCHEMA_VERSION,
            // 撮合钩子按事件逐次由状态机直接判定就绪，root 仅保留首个目标的
            // 可求值形态以维持 cloud-ast 结构完整；配对语义归撮合执行器。
            "source": targets.first().map(|t| t.source.clone()).unwrap_or_default(),
            "mode": HookMode::Merge,
            "mergeTargets": targets
                .iter()
                .map(|target| json!({
                    "source": target.source,
                    "signal": merge_target_signal(target),
                }))
                .collect::<Vec<_>>(),
            "root": expr_to_cloud_value(&hook.condition)
        })),
        Expr::Anchor { target } => Ok(json!({
            "schemaVersion": CLOUD_AST_SCHEMA_VERSION,
            // 收购回流钩子按子订单事件逐次由状态机判定投递；血缘过滤在状态机
            // 完成，聚合裁决归锚定执行器。
            "source": "",
            "mode": HookMode::Anchor,
            "anchorTarget": {
                "signal": merge_target_signal(target),
            },
            "root": expr_to_cloud_value(&hook.condition)
        })),
        _ => Ok(json!({
            "schemaVersion": CLOUD_AST_SCHEMA_VERSION,
            "source": hook.source.clone(),
            "mode": HookMode::Normal,
            "upstreamSource": Value::Null,
            "root": expr_to_cloud_value(&hook.condition)
        })),
    }
}

fn merge_target_signal(target: &HookExpr) -> String {
    match &target.condition {
        Expr::Signal(signal) => signal.clone(),
        other => normalize_tight(other),
    }
}

fn expr_to_cloud_value(expr: &Expr) -> Value {
    match expr {
        Expr::Signal(signal) => json!({ "type": "signal", "signal": signal }),
        Expr::External { target, .. } => expr_to_cloud_value(&target.condition),
        Expr::Merge { targets } => json!({
            "type": "merge",
            "targets": targets
                .iter()
                .map(|target| json!({
                    "source": target.source,
                    "signal": merge_target_signal(target),
                }))
                .collect::<Vec<_>>()
        }),
        Expr::Anchor { target } => json!({
            "type": "anchor",
            "signal": merge_target_signal(target),
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
    let mut iter = terms.iter();
    let Some(first) = iter.next() else {
        return Value::Null;
    };
    iter.fold(
        expr_to_cloud_value(first),
        |left, term| json!({ "type": kind, "left": left, "right": expr_to_cloud_value(term) }),
    )
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
        Expr::External { target, .. } => eval_expr(&target.condition, &target.source, signals, now),
        Expr::Merge { .. } => Ok(InternalEval {
            state: EvalState::NeedsMore,
            anchors: Vec::new(),
            ready_at: None,
            reason: Some(
                "merge hooks are delivered per contributing event by the state machine".to_string(),
            ),
        }),
        Expr::Anchor { .. } => Ok(InternalEval {
            state: EvalState::NeedsMore,
            anchors: Vec::new(),
            ready_at: None,
            reason: Some(
                "anchor hooks are delivered per child-order event by the state machine".to_string(),
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
                EvalState::Ready | EvalState::Wait => {
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
                    let delta = chrono::Duration::try_seconds(*duration_seconds).ok_or_else(|| {
                        HookError::Message(format!(
                            "delay duration seconds out of range: {duration_seconds}"
                        ))
                    })?;
                    let ready_at = anchor
                        .checked_add_signed(delta)
                        .ok_or_else(|| {
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
            for term in terms {
                let evaluated = eval_expr(term, source, signals, now)?;
                match evaluated.state {
                    EvalState::Ready => return Ok(evaluated),
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
    profile: Profile,
    depth: usize,
}

/// 递归下降深度上限。hook 表达式来自外部可填写的模板定义，无界嵌套
/// （深层括号或连续 `~`）会打满调用栈直接 abort 宿主进程——栈溢出不可被
/// catch_unwind 捕获，必须在解析期以普通错误拒绝。
const MAX_EXPRESSION_DEPTH: usize = 128;

impl<'a> Parser<'a> {
    fn new(input: &'a str, profile: Profile) -> Self {
        Self {
            input,
            index: 0,
            profile,
            depth: 0,
        }
    }

    fn enter(&mut self) -> Result<()> {
        if self.depth >= MAX_EXPRESSION_DEPTH {
            return Err(HookError::Message(format!(
                "hook expression nesting exceeds the maximum depth of {MAX_EXPRESSION_DEPTH}"
            )));
        }
        self.depth += 1;
        Ok(())
    }

    fn parse(&mut self) -> Result<Expr> {
        let expr = self.parse_or()?;
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
        self.enter()?;
        let result = self.parse_or_inner();
        self.depth -= 1;
        result
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
        let mut terms = vec![self.parse_unary()?];
        while self.consume("&") {
            terms.push(self.parse_unary()?);
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
            self.enter()?;
            let result = self.parse_unary();
            self.depth -= 1;
            return result.map(|expr| Expr::Not(Box::new(expr)));
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
            "OUTSIDE" => self.parse_external(ExternalMode::Outside),
            "MERGE" => self.parse_merge(),
            "ANCHOR" => self.parse_anchor(),
            "OUTSOURCE" => Err(HookError::Message(
                "OUTSOURCE has been retired; use ::OUTSIDE@(...) to fork an independent order, ::MERGE@(...) to fan cross-source events into a match executor, or ::ANCHOR@(task.stage.signal) to reflux child-order events into an anchor executor".to_string(),
            )),
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

    fn parse_external(&mut self, mode: ExternalMode) -> Result<Expr> {
        self.skip_ws();
        if !self.consume("@") {
            return Err(HookError::Message(format!(
                "bare {} is no longer supported; declare external inputs in externalSignals or use {}@(source::task.stage.signal) for a cross-source wrapper",
                mode.as_str(),
                mode.as_str(),
            )));
        }
        if !self.consume("(") {
            return Err(HookError::Message(format!(
                "expected '@' target at {}",
                self.index
            )));
        }
        let target_raw = self.read_balanced_target()?;
        let target = parse_hook_expr(&target_raw, self.profile)?;
        if contains_nested_external(&target.condition) {
            return Err(HookError::Message(format!(
                "nested OUTSIDE/MERGE is not allowed in {target_raw:?}"
            )));
        }
        Ok(Expr::External {
            mode,
            target: Box::new(target),
        })
    }

    fn parse_merge(&mut self) -> Result<Expr> {
        self.skip_ws();
        if !self.consume("@") {
            return Err(HookError::Message(
                "bare MERGE is no longer supported; use MERGE@(a::task.stage.signal, b::task.stage.signal) for a merge entry".to_string(),
            ));
        }
        if !self.consume("(") {
            return Err(HookError::Message(format!(
                "expected '@' target at {}",
                self.index
            )));
        }
        let targets_raw = self.read_balanced_target()?;
        let mut targets = Vec::new();
        let mut seen_targets = std::collections::BTreeSet::new();
        for part in targets_raw.split(',') {
            let raw = part.trim();
            if raw.is_empty() {
                return Err(HookError::Message(format!(
                    "MERGE@ targets must be non-empty source::task.stage.signal entries in {targets_raw:?}"
                )));
            }
            let target = parse_hook_expr(raw, self.profile)?;
            if !matches!(target.condition, Expr::Signal(_)) {
                return Err(HookError::Message(format!(
                    "MERGE@ target must be a plain source::task.stage.signal reference: {raw:?}"
                )));
            }
            if !seen_targets.insert(raw.to_string()) {
                return Err(HookError::Message(format!(
                    "MERGE@ targets must be distinct: duplicate entry {raw:?}"
                )));
            }
            targets.push(target);
        }
        // k≥1 而非 k≥2：k=1 是合法的跨订单观察/聚合入口（单 source 的多张订单
        // 逐事件转交静态执行器，判定权仍在执行器）；求值器层的 ≥2 下限只约束
        // 表达式形态，见 expr_from_cloud_value。
        if targets.is_empty() {
            return Err(HookError::Message(
                "MERGE@ requires at least one upstream signal target".to_string(),
            ));
        }
        Ok(Expr::Merge { targets })
    }

    fn parse_anchor(&mut self) -> Result<Expr> {
        self.skip_ws();
        if !self.consume("@") {
            return Err(HookError::Message(
                "bare ANCHOR is no longer supported; use ::ANCHOR@(task.stage.signal) for an acquisition reflux entry".to_string(),
            ));
        }
        if !self.consume("(") {
            return Err(HookError::Message(format!(
                "expected '@' target at {}",
                self.index
            )));
        }
        // 收购回流目标必须是裸 task.stage.signal：子订单事件来自任意农户秩序，
        // 不允许 source 名空间，血缘过滤由状态机按 rel_order_order 裁决。
        let signal = self.read_balanced_target()?;
        let bare_ref = !signal.is_empty()
            && signal
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '.' | '-'))
            && is_strict_signal_ref(&signal);
        if !bare_ref {
            return Err(HookError::Message(format!(
                "ANCHOR@ target must be a bare task.stage.signal reference without a source namespace: {signal:?}"
            )));
        }
        Ok(Expr::Anchor {
            target: Box::new(HookExpr {
                raw: signal.clone(),
                source: String::new(),
                condition: Expr::Signal(signal),
            }),
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

fn contains_nested_external(expr: &Expr) -> bool {
    match expr {
        Expr::External { .. } | Expr::Merge { .. } | Expr::Anchor { .. } => true,
        Expr::Signal(_) => false,
        Expr::Not(inner) | Expr::Delay { expr: inner, .. } => contains_nested_external(inner),
        Expr::And(terms) | Expr::Or(terms) => terms.iter().any(contains_nested_external),
    }
}

/// 延时操作数上限：30 天。超限在编译期直接拒绝，防止毒定义持久化。
const MAX_DELAY_SECONDS: i64 = 30 * 24 * 60 * 60;

fn duration_to_seconds(duration: &str) -> Result<i64> {
    if duration.len() < 2 {
        return Err(HookError::Message(format!("invalid duration: {duration}")));
    }
    let (num, unit) = duration.split_at(duration.len() - 1);
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
        "s" => 1,
        "m" => 60,
        "h" => 60 * 60,
        "d" => 60 * 60 * 24,
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
        assert!(
            err.to_string().contains("30d"),
            "unexpected error: {err}"
        );

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
        let mut poisoned = json!({
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
        if poisoned.get("upstreamSource").is_none() {
            poisoned["upstreamSource"] = serde_json::Value::Null;
        }

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
        assert!(
            err.to_string().contains("30d"),
            "unexpected error: {err}"
        );
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
        assert!(err.to_string().contains("no longer supported"));
    }

    #[test]
    fn rejects_external_operator_inside_composite_condition() {
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
                err.to_string().contains("no longer supported"),
                "unexpected error for {hook}: {err}"
            );
        }

        let err = parse_hook(ParseHookRequest {
            profile: Profile::CloudCompat,
            hook_name: "HOOK".to_string(),
            hook: "::OUTSIDE@(seller::task.main.cmp) & task.other.cmp".to_string(),
        })
        .unwrap_err();
        assert!(
            err.to_string()
                .contains("must be the complete hook condition"),
            "unexpected error for a composite cross-source wrapper: {err}"
        );

        let err = parse_hook(ParseHookRequest {
            profile: Profile::CloudCompat,
            hook_name: "HOOK".to_string(),
            hook: "buyer::OUTSIDE@(seller::task.main.cmp)".to_string(),
        })
        .unwrap_err();
        assert!(
            err.to_string().contains("empty source header"),
            "expected headed OUTSIDE rejection: {err}"
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
            hook: "::MERGE@(a::t.s.x, a::t.s.x)".to_string(),
        })
        .unwrap_err();
        assert!(
            err.to_string().contains("duplicate entry"),
            "unexpected error for duplicate merge targets: {err}"
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
                err.to_string().contains("OUTSOURCE has been retired"),
                "unexpected error for {hook}: {err}"
            );
        }
    }

    #[test]
    fn merge_entry_parses_targets_and_dependencies() {        let out = parse_value(
            "::MERGE@(seller::trade.listing.cmp, buyer::trade.intent.cmp)",
            Profile::CloudCompat,
            "MATCH",
        );
        assert_eq!(out["mode"], "merge");
        assert_eq!(out["mergeTargets"], json!([
            { "source": "seller", "signalName": "trade.listing.cmp" },
            { "source": "buyer", "signalName": "trade.intent.cmp" }
        ]));
        assert_eq!(
            out["dependencies"],
            json!([
                { "kind": "positive", "source": "buyer", "signalName": "trade.intent.cmp" },
                { "kind": "positive", "source": "seller", "signalName": "trade.listing.cmp" }
            ])
        );
        assert_eq!(
            out["normalizedExpression"],
            "::MERGE@(seller::trade.listing.cmp, buyer::trade.intent.cmp)"
        );

        let cloud_ast = out["cloudAst"].clone();
        assert_eq!(cloud_ast["mode"], "merge");
        assert_eq!(cloud_ast["source"], "seller");
        assert_eq!(cloud_ast["mergeTargets"].as_array().map(Vec::len), Some(2));

        let eval = eval_compiled_hook(EvalCompiledHookRequest {
            profile: Profile::CloudCompat,
            ast: cloud_ast,
            signals: vec![],
            now: "2026-04-27T00:00:00Z".to_string(),
        })
        .unwrap();
        assert_eq!(eval.state, EvalState::NeedsMore);
    }

    #[test]
    fn merge_entry_rejects_degenerate_shapes() {
        for hook in [
            "::MERGE@(seller::trade.listing.cmp,)",
            "::MERGE@()",
            "::MERGE@(seller::trade.listing.cmp & buyer::trade.intent.cmp)",
            "::MERGE@(seller::trade.listing.cmp, seller::(trade.a & trade.b))",
            "wholesaler::MERGE@(seller::trade.listing.cmp, buyer::trade.intent.cmp)",
        ] {
            let err = parse_hook(ParseHookRequest {
                profile: Profile::CloudCompat,
                hook_name: "HOOK".to_string(),
                hook: hook.to_string(),
            })
            .unwrap_err();
            assert!(
                !err.to_string().is_empty(),
                "expected rejection for {hook}"
            );
        }
    }

    #[test]
    fn merge_entry_with_single_upstream_is_observation_entry() {
        // k=1 是合法的运行时投递形态：单 source 的多张订单逐事件转交静态执行
        // 器（跨订单观察/聚合入口），判定权仍在执行器。钉住 parse 层 k≥1 下限，
        // 防止被误"收紧"到 ≥2。
        let out = parse_value(
            "::MERGE@(child_ci::child.test.cmp)",
            Profile::CloudCompat,
            "OBSERVE",
        );
        assert_eq!(out["mode"], "merge");
        assert_eq!(
            out["mergeTargets"],
            json!([{ "source": "child_ci", "signalName": "child.test.cmp" }])
        );

        // 求值器只做表达式裁决、从不执行投递：作为汇合表达式，k<2 在求值层
        // 仍无意义。运行时投递由状态机在 decode 层短路完成，不经过求值器。
        let err = eval_compiled_hook(EvalCompiledHookRequest {
            profile: Profile::CloudCompat,
            ast: out["cloudAst"].clone(),
            signals: vec![],
            now: "2026-04-27T00:00:00Z".to_string(),
        })
        .expect_err("merge expression with a single operand must not evaluate");
        assert!(
            err.to_string().contains("requires at least two targets"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn anchor_entry_parses_target_and_cloud_ast() {
        let out = parse_value(
            "::ANCHOR@(farmer.main.settle)",
            Profile::CloudCompat,
            "ANCHOR_SETTLE",
        );
        assert_eq!(out["mode"], "anchor");
        assert_eq!(
            out["dependencies"],
            json!([{ "kind": "positive", "source": "", "signalName": "farmer.main.settle" }])
        );
        assert_eq!(
            out["normalizedExpression"],
            "::ANCHOR@(farmer.main.settle)"
        );

        let cloud_ast = out["cloudAst"].clone();
        assert_eq!(cloud_ast["mode"], "anchor");
        assert_eq!(
            cloud_ast["anchorTarget"],
            json!({ "signal": "farmer.main.settle" })
        );

        let eval = eval_compiled_hook(EvalCompiledHookRequest {
            profile: Profile::CloudCompat,
            ast: cloud_ast.clone(),
            signals: vec![],
            now: "2026-04-27T00:00:00Z".to_string(),
        })
        .unwrap();
        assert_eq!(eval.state, EvalState::NeedsMore);
        assert!(eval.reason.unwrap_or_default().contains("delivered per"));

        // 直接构造缺失 anchorTarget 的毒 AST：求值必须在解码期确定性拒绝。
        let mut poisoned = cloud_ast;
        poisoned["anchorTarget"] = serde_json::Value::Null;
        let err = eval_compiled_hook(EvalCompiledHookRequest {
            profile: Profile::CloudCompat,
            ast: poisoned,
            signals: vec![],
            now: "2026-04-27T00:00:00Z".to_string(),
        })
        .expect_err("anchor AST without target must not be evaluated");
        assert!(
            err.to_string().contains("anchorTarget"),
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
}
