use chrono::{DateTime, SecondsFormat, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use thiserror::Error;

pub const CORE_VERSION: &str = env!("CARGO_PKG_VERSION");
pub const SEMANTIC_VERSION: &str = "uvp-semantic/0.1";

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
        target: Option<Box<HookExpr>>,
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
    #[serde(rename = "OUTSOURCE")]
    Outsource,
}

impl ExternalMode {
    fn as_str(self) -> &'static str {
        match self {
            Self::Outside => "OUTSIDE",
            Self::Outsource => "OUTSOURCE",
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
    pub has_outside: bool,
    pub has_outsource: bool,
    pub dependencies: Vec<Dependency>,
    pub ast: Value,
    pub cloud_ast: Value,
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
    Outside,
    OutsideSpawn,
    Outsource,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EvalHookOutput {
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
#[serde(rename_all = "camelCase")]
pub struct EvalHookRequest {
    #[serde(default)]
    pub profile: Profile,
    #[serde(default)]
    pub hook_name: String,
    pub hook: String,
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

pub fn eval_hook_json(input: &str) -> String {
    let result = serde_json::from_str::<EvalHookRequest>(input)
        .map_err(|err| HookError::Message(format!("invalid eval hook request: {err}")))
        .and_then(eval_hook);
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
    let dependencies = extract_dependencies(&hook, profile, &hook_name);
    let cloud_ast = cloud_ast_for(&hook, &hook_name, profile)?;

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
        has_outside: matches!(mode, HookMode::Outside | HookMode::OutsideSpawn),
        has_outsource: matches!(mode, HookMode::Outsource),
        dependencies,
        ast: hook_to_value(&hook),
        cloud_ast,
    })
}

pub fn eval_hook(req: EvalHookRequest) -> Result<EvalHookOutput> {
    let profile = req.profile;
    let hook = parse_hook_expr(&req.hook, profile)?;
    validate_hook(&hook.condition, profile)?;
    let now = parse_time(&req.now, profile)?;
    let mut signals = BTreeMap::new();
    for signal in req.signals {
        let received_at = parse_time(&signal.received_at, profile)?;
        signals.insert(
            signal_key(&signal.source, &signal.signal_name),
            SignalEntry {
                source: signal.source.clone(),
                signal_name: signal.signal_name.clone(),
                received_at,
            },
        );
    }

    let result = eval_expr(&hook.condition, &hook.source, &signals, now, profile)?;
    Ok(EvalHookOutput {
        uvp_core_version: CORE_VERSION,
        semantic_version: SEMANTIC_VERSION,
        profile,
        state: result.state,
        ready_at: result
            .ready_at
            .map(|ts| ts.to_rfc3339_opts(SecondsFormat::Millis, true)),
        reason: result.reason,
    })
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
            "empty source is only allowed for OUTSIDE or OUTSOURCE hooks".to_string(),
        ));
    }
    reject_unsupported_legacy_syntax(condition_raw)?;
    let mut parser = Parser::new(condition_raw, profile);
    let condition = parser.parse()?;
    Ok(HookExpr {
        raw: raw.to_string(),
        source,
        condition,
    })
}

fn starts_external(value: &str) -> bool {
    value.starts_with("OUTSIDE") || value.starts_with("OUTSOURCE")
}

fn reject_unsupported_legacy_syntax(condition: &str) -> Result<()> {
    if condition.contains("+T") {
        return Err(HookError::Message(format!(
            "legacy +T syntax is not supported: {condition:?}"
        )));
    }
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
        Expr::Signal(_) | Expr::External { .. } => Ok(true),
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
        Expr::Signal(_) | Expr::External { .. } => true,
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
        Expr::External { mode, target } => match target {
            Some(target) => format!("{}@({})", mode.as_str(), normalize_hook_tight(target)),
            None => mode.as_str().to_string(),
        },
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
        Expr::Signal(_) | Expr::External { .. } => normalize_tight(expr),
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
        Expr::External { mode, target } => match target {
            Some(target) => format!("{}@({})", mode.as_str(), normalize_hook_tight(target)),
            None => mode.as_str().to_string(),
        },
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
        Expr::Signal(_) | Expr::External { .. } => 4,
    }
}

fn runtime_condition(hook: &HookExpr, hook_name: &str, profile: Profile) -> Result<String> {
    if profile == Profile::EvmStrict {
        return Ok(normalize_condition(&hook.condition, NormalizeStyle::Tight));
    }
    match &hook.condition {
        Expr::External {
            mode: ExternalMode::Outside,
            target: None,
        } => Ok(hook_name.to_string()),
        Expr::External {
            mode: ExternalMode::Outside | ExternalMode::Outsource,
            target: Some(target),
        } => Ok(normalize_condition(
            &target.condition,
            NormalizeStyle::Cloud,
        )),
        _ => Ok(normalize_condition(&hook.condition, NormalizeStyle::Cloud)),
    }
}

fn hook_mode(expr: &Expr) -> HookMode {
    match expr {
        Expr::External {
            mode: ExternalMode::Outside,
            target: None,
        } => HookMode::Outside,
        Expr::External {
            mode: ExternalMode::Outside,
            target: Some(_),
        } => HookMode::OutsideSpawn,
        Expr::External {
            mode: ExternalMode::Outsource,
            ..
        } => HookMode::Outsource,
        _ => HookMode::Normal,
    }
}

fn upstream_source(expr: &Expr) -> Option<String> {
    match expr {
        Expr::External {
            target: Some(target),
            ..
        } => Some(target.source.clone()),
        _ => None,
    }
}

fn compatibility_for(_hook: &HookExpr, profile: Profile) -> Compatibility {
    match profile {
        Profile::EvmStrict => Compatibility::Portable,
        Profile::CloudCompat => Compatibility::CloudOnly,
    }
}

fn extract_dependencies(hook: &HookExpr, profile: Profile, hook_name: &str) -> Vec<Dependency> {
    let mut deps = Vec::new();
    if profile == Profile::CloudCompat {
        if let Expr::External {
            mode: ExternalMode::Outside,
            target: None,
        } = &hook.condition
        {
            deps.push(Dependency {
                kind: DependencyKind::Positive,
                source: hook.source.clone(),
                signal_name: hook_name.to_string(),
                delay_seconds: None,
            });
            return deps;
        }
    }
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
        Expr::External { mode, target } => {
            if let Some(target) = target {
                collect_dependencies(&target.condition, &target.source, negated, out);
            } else {
                out.push(Dependency {
                    kind: if negated {
                        DependencyKind::Negative
                    } else {
                        DependencyKind::Positive
                    },
                    source: source.to_string(),
                    signal_name: mode.as_str().to_string(),
                    delay_seconds: None,
                });
            }
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
            if let Some(target) = target {
                json!({ "kind": "external", "mode": mode.as_str(), "target": hook_to_value(target) })
            } else {
                json!({ "kind": "external", "mode": mode.as_str() })
            }
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

fn cloud_ast_for(hook: &HookExpr, hook_name: &str, profile: Profile) -> Result<Value> {
    let expr = if profile == Profile::CloudCompat {
        match &hook.condition {
            Expr::External {
                mode: ExternalMode::Outside,
                target: None,
            } => Expr::Signal(hook_name.to_string()),
            Expr::External {
                mode: ExternalMode::Outside | ExternalMode::Outsource,
                target: Some(target),
            } => target.condition.clone(),
            _ => hook.condition.clone(),
        }
    } else {
        hook.condition.clone()
    };
    Ok(json!({ "root": expr_to_cloud_value(&expr) }))
}

fn expr_to_cloud_value(expr: &Expr) -> Value {
    match expr {
        Expr::Signal(signal) => json!({ "type": "signal", "signal": signal }),
        Expr::External { mode, target } => {
            if let Some(target) = target {
                expr_to_cloud_value(&target.condition)
            } else {
                json!({ "type": "signal", "signal": mode.as_str() })
            }
        }
        Expr::Not(inner) => json!({ "type": "neg", "expr": expr_to_cloud_value(inner) }),
        Expr::And(terms) => fold_cloud_terms("and", terms),
        Expr::Or(terms) => fold_cloud_terms("or", terms),
        Expr::Delay {
            expr, raw_duration, ..
        } => json!({ "type": "delay", "expr": expr_to_cloud_value(expr), "delay": raw_duration }),
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
    profile: Profile,
) -> Result<InternalEval> {
    match expr {
        Expr::Signal(signal) => eval_signal(source, signal, signals),
        Expr::External { mode, target } => {
            if let Some(target) = target {
                eval_expr(&target.condition, &target.source, signals, now, profile)
            } else {
                eval_signal(source, mode.as_str(), signals)
            }
        }
        Expr::Not(inner) => {
            let evaluated = eval_expr(inner, source, signals, now, profile)?;
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
            let evaluated = eval_expr(expr, source, signals, now, profile)?;
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
                    let ready_at = anchor + chrono::Duration::seconds(*duration_seconds);
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
                let evaluated = eval_expr(term, source, signals, now, profile)?;
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
                let evaluated = eval_expr(term, source, signals, now, profile)?;
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
}

impl<'a> Parser<'a> {
    fn new(input: &'a str, profile: Profile) -> Self {
        Self {
            input,
            index: 0,
            profile,
        }
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
            return Ok(Expr::Not(Box::new(self.parse_unary()?)));
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
            "OUTSOURCE" => self.parse_external(ExternalMode::Outsource),
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
            return Ok(Expr::External { mode, target: None });
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
                "nested OUTSIDE/OUTSOURCE is not allowed in {target_raw:?}"
            )));
        }
        Ok(Expr::External {
            mode,
            target: Some(Box::new(target)),
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
        Ok(self.input[start..self.index].to_string())
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
        Expr::External { .. } => true,
        Expr::Signal(_) => false,
        Expr::Not(inner) | Expr::Delay { expr: inner, .. } => contains_nested_external(inner),
        Expr::And(terms) | Expr::Or(terms) => terms.iter().any(contains_nested_external),
    }
}

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
    match unit {
        "s" => Ok(value),
        "m" => Ok(value * 60),
        "h" => Ok(value * 60 * 60),
        "d" => Ok(value * 60 * 60 * 24),
        _ => Err(HookError::Message(format!("invalid duration unit: {unit}"))),
    }
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

    #[test]
    fn evm_strict_parses_and_evaluates_positive_signal() {
        let out = parse_value("buyer::task.main.cmp", Profile::EvmStrict, "TRIGGER");
        assert_eq!(out["normalizedExpression"], "buyer::task.main.cmp");
        assert_eq!(
            out["dependencies"],
            json!([{ "kind": "positive", "source": "buyer", "signalName": "task.main.cmp" }])
        );

        let eval = eval_hook(EvalHookRequest {
            profile: Profile::EvmStrict,
            hook_name: "TRIGGER".to_string(),
            hook: "buyer::task.main.cmp".to_string(),
            signals: vec![SignalFact {
                source: "buyer".to_string(),
                signal_name: "task.main.cmp".to_string(),
                received_at: "2026-04-27T00:00:00.900Z".to_string(),
            }],
            now: "2026-04-27T00:00:00.999Z".to_string(),
        })
        .unwrap();
        assert_eq!(eval.state, EvalState::Ready);
        assert_eq!(eval.ready_at.as_deref(), Some("2026-04-27T00:00:00.000Z"));
    }

    #[test]
    fn evm_strict_handles_delay_and_negative_guard() {
        let eval = eval_hook(EvalHookRequest {
            profile: Profile::EvmStrict,
            hook_name: "TIMEOUT".to_string(),
            hook: "buyer::(task.pay.cmp +5s) & ~task.refund.cmp".to_string(),
            signals: vec![SignalFact {
                source: "buyer".to_string(),
                signal_name: "task.pay.cmp".to_string(),
                received_at: "2026-04-27T00:00:00.900Z".to_string(),
            }],
            now: "2026-04-27T00:00:04.999Z".to_string(),
        })
        .unwrap();
        assert_eq!(eval.state, EvalState::Wait);
        assert_eq!(eval.ready_at.as_deref(), Some("2026-04-27T00:00:05.000Z"));
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

        let outside = parse_value("::OUTSIDE", Profile::CloudCompat, "TRIGGER");
        assert_eq!(outside["runtimeCondition"], "TRIGGER");
        assert_eq!(
            outside["dependencies"],
            json!([{ "kind": "positive", "source": "", "signalName": "TRIGGER" }])
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
}
