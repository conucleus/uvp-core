//! 求值：cloud AST 解码（毒产物确定性拒绝）与事实集上的表达式求值。

use chrono::{DateTime, SecondsFormat, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

use crate::ast::{
    contains_nested_subscription, is_plain_identifier, normalize_tight, valid_signal_identity,
    validate_filter_hook, validate_hook, Expr, HookMode,
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
    #[serde(default)]
    pub gate: Gate,
    pub ast: Value,
    #[serde(default)]
    pub signals: Vec<SignalFact>,
    pub now: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SignalFact {
    #[serde(default)]
    pub source: String,
    pub signal_name: String,
    pub received_at: String,
}

#[derive(Debug, Clone)]
pub struct DecodedCompiledHook {
    pub mode: HookMode,
    pub source: String,
    pub expr: Expr,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HookEval {
    pub state: EvalState,
    pub ready_at: Option<DateTime<Utc>>,
}

impl DecodedCompiledHook {
    pub fn eval(
        &self,
        signals: &BTreeMap<String, DateTime<Utc>>,
        now: DateTime<Utc>,
    ) -> Result<HookEval> {
        let entries = signals
            .iter()
            .map(|(name, received_at)| {
                (
                    signal_key(&self.source, name),
                    SignalEntry {
                        source: self.source.clone(),
                        signal_name: name.clone(),
                        received_at: *received_at,
                    },
                )
            })
            .collect::<BTreeMap<_, _>>();
        let evaluated = eval_expr(&self.expr, &self.source, &entries, now)?;
        Ok(HookEval {
            state: evaluated.state,
            ready_at: evaluated.ready_at,
        })
    }
}

pub fn decode_compiled_hook(ast: &Value, gate: Gate) -> Result<DecodedCompiledHook> {
    let ast_object = ast
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
    let schema_version = ast
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
    let mode = ast
        .get("mode")
        .and_then(Value::as_str)
        .ok_or_else(|| HookError::Message("compiled hook AST is missing mode".to_string()))?;
    if !matches!(mode, "normal" | "subscription") {
        return Err(HookError::Message(format!(
            "unsupported compiled hook AST mode: {mode}"
        )));
    }
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
        let target = ast
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
        if !is_plain_identifier(source) || source.len() > 36 {
            return Err(HookError::Message(format!(
                    "compiled subscriptionTarget source must be a plain identifier of at most 36 characters: {source:?}"
                )));
        }
        if !valid_signal_identity(signal) {
            return Err(HookError::Message(format!(
                    "compiled subscriptionTarget signal must use stage.signal and be at most 100 characters: {signal:?}"
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
    let raw_source = optional_ast_str(ast_object, "source")?;
    let source = match mode {
        "subscription" => {
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
            if !is_plain_identifier(&source) || source.len() > 36 {
                return Err(HookError::Message(format!(
                    "compiled hook AST source must be a plain identifier of at most 36 characters: {source:?}"
                )));
            }
            source
        }
    };
    let root = ast
        .get("root")
        .ok_or_else(|| HookError::Message("compiled hook AST root is missing".to_string()))?;
    let expr = expr_from_cloud_value(root)?;
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
    match gate {
        Gate::Hook => validate_hook(&expr)?,
        Gate::Filter => validate_filter_hook(&expr)?,
    }
    Ok(DecodedCompiledHook {
        mode: if mode == "subscription" {
            HookMode::Subscription
        } else {
            HookMode::Normal
        },
        source,
        expr,
    })
}

pub fn eval_compiled_hook(req: EvalCompiledHookRequest) -> Result<EvalCompiledHookOutput> {
    let decoded = decode_compiled_hook(&req.ast, req.gate)?;
    let now = parse_time(&req.now, req.profile)?;
    let signals = signal_map(req.signals, req.profile)?;
    let result = eval_expr(&decoded.expr, &decoded.source, &signals, now)?;

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
                "signal fact name must use stage.signal and be at most 100 characters: {:?}",
                signal.signal_name
            )));
        }
        let received_at = parse_time(&signal.received_at, profile)?;
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
            if !valid_signal_identity(signal) {
                return Err(HookError::Message(format!(
                    "compiled signal AST node must use stage.signal and be at most 100 characters: {signal:?}"
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
            if !is_plain_identifier(source) || source.len() > 36 {
                return Err(HookError::Message(format!(
                    "compiled subscription AST node source must be a plain identifier of at most 36 characters: {source:?}"
                )));
            }
            if !valid_signal_identity(signal) {
                return Err(HookError::Message(format!(
                    "compiled subscription AST node signal must use stage.signal and be at most 100 characters: {signal:?}"
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
                EvalState::Wait => Err(HookError::Message(
                    "delay operand is in a wait state: nested delays are rejected by the grammar, this state must be unreachable"
                        .to_string(),
                )),
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
