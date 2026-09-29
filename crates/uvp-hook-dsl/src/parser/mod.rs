//! 解析：递归下降解析器、词法闸与 parse 管线（请求/产物类型 + 入口）。
//! 解析行为与 profile 无关（profile 只影响归一化/兼容性输出）。

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::ast::{
    cloud_ast_for, compatibility_for, hook_mode, hook_to_value, is_plain_identifier,
    is_strict_signal_ref, normalize_condition, runtime_condition, validate_filter_hook,
    validate_hook, validate_subscription_position, Compatibility, Expr, HookExpr, HookMode,
    NormalizeStyle, SubscriptionTarget,
};
use crate::dependency::{extract_dependencies, Dependency};
use crate::lint::Span;
use crate::{
    Gate, HookError, Profile, Result, CORE_VERSION, RETIRED_KEYWORDS_HINT, SEMANTIC_VERSION,
};

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

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ParseHookRequest {
    #[serde(default)]
    pub profile: Profile,
    #[serde(default)]
    pub gate: Gate,
    #[serde(default)]
    pub hook_name: String,
    pub hook: String,
}

pub fn parse_hook(req: ParseHookRequest) -> Result<ParseHookOutput> {
    let profile = req.profile;
    let hook_name = req.hook_name;
    validate_hook_name(&hook_name)?;
    let (hook, _spans) = parse_hook_expr_with_spans(&req.hook)?;
    match req.gate {
        Gate::Hook => validate_hook(&hook.condition)?,
        Gate::Filter => validate_filter_hook(&hook.condition)?,
    }

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

pub(crate) const MAX_PARSE_DEPTH: usize = 120;

pub(crate) fn validate_hook_name(hook_name: &str) -> Result<()> {
    if hook_name.trim().is_empty() || hook_name.len() > 36 {
        return Err(HookError::Message(
            "hook_name must be 1-36 characters".to_string(),
        ));
    }
    if hook_name.contains('.') || hook_name.contains('#') {
        return Err(HookError::Message(
            "hook_name must not contain '.' or '#'".to_string(),
        ));
    }
    if hook_name.chars().any(char::is_whitespace) {
        return Err(HookError::Message(
            "hook_name must not contain whitespace".to_string(),
        ));
    }
    Ok(())
}

pub fn parse_hook_expr_with_spans(raw: &str) -> Result<(HookExpr, Vec<Span>)> {
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
    let spans = parser.spans;
    validate_subscription_position(&condition, true)?;
    if matches!(condition, Expr::Subscription { .. }) && !source.is_empty() {
        return Err(HookError::Message(
            "subscription entries must use an empty source header: ::ANCHOR(@source::task.stage.signal)"
                .to_string(),
        ));
    }
    Ok((
        HookExpr {
            raw: raw.to_string(),
            source,
            condition,
        },
        spans,
    ))
}

fn starts_cross_source(value: &str) -> bool {
    ["ANCHOR", "OUTSIDE", "OUTSOURCE"].iter().any(|keyword| {
        let Some(rest) = value.strip_prefix(keyword) else {
            return false;
        };
        rest.chars()
            .next()
            .is_none_or(|ch| !(ch.is_ascii_alphanumeric() || matches!(ch, '_' | '.' | '-')))
    })
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

struct Parser<'a> {
    input: &'a str,
    index: usize,
    depth: usize,
    spans: Vec<Span>,
}

impl<'a> Parser<'a> {
    fn new(input: &'a str) -> Self {
        Self {
            input,
            index: 0,
            depth: 0,
            spans: Vec::new(),
        }
    }

    fn span_end_here(&self) -> usize {
        let mut end = self.index.min(self.input.len());
        while end > 0 {
            match self.input[..end].chars().next_back() {
                Some(ch) if ch.is_whitespace() => end -= ch.len_utf8(),
                _ => break,
            }
        }
        end
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
        self.skip_ws();
        let start = self.index;
        let mut terms = vec![self.parse_and()?];
        while self.consume("|") {
            terms.push(self.parse_and()?);
        }
        Ok(if terms.len() == 1 {
            terms.remove(0)
        } else {
            self.spans.push(Span::new(start, self.span_end_here()));
            Expr::Or(terms)
        })
    }

    fn parse_and(&mut self) -> Result<Expr> {
        self.skip_ws();
        let start = self.index;
        let mut terms = vec![self.guard_depth(|parser| parser.parse_unary())?];
        while self.consume("&") {
            terms.push(self.guard_depth(|parser| parser.parse_unary())?);
        }
        Ok(if terms.len() == 1 {
            terms.remove(0)
        } else {
            self.spans.push(Span::new(start, self.span_end_here()));
            Expr::And(terms)
        })
    }

    fn parse_unary(&mut self) -> Result<Expr> {
        self.skip_ws();
        let start = self.index;
        if self.consume("~") {
            let inner = self.guard_depth(|parser| parser.parse_unary())?;
            self.spans.push(Span::new(start, self.span_end_here()));
            return Ok(Expr::Not(Box::new(inner)));
        }
        self.parse_postfix()
    }

    fn parse_postfix(&mut self) -> Result<Expr> {
        self.skip_ws();
        let start = self.index;
        let mut expr = self.parse_primary()?;
        self.skip_ws();
        if self.consume("+") {
            let raw_duration = self.read_duration()?;
            let duration_seconds = duration_to_seconds(&raw_duration)?;
            self.spans.push(Span::new(start, self.span_end_here()));
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

        let ident_start = self.index;
        let ident = self.read_identifier()?;
        match ident.as_str() {
            "ANCHOR" => self.parse_subscription(ident_start),
            "OUTSIDE" | "OUTSOURCE" => Err(HookError::Message(format!(
                "{ident}@ is not supported: {RETIRED_KEYWORDS_HINT}"
            ))),
            _ => {
                if !is_strict_signal_ref(&ident) {
                    return Err(HookError::Message(format!(
                        "signal reference must use task.stage.signal: {ident}"
                    )));
                }
                self.spans
                    .push(Span::new(ident_start, self.span_end_here()));
                Ok(Expr::Signal(ident))
            }
        }
    }

    fn parse_subscription(&mut self, anchor_start: usize) -> Result<Expr> {
        self.skip_ws();
        if self.peek() == '@' {
            return Err(HookError::Message(format!(
                "ANCHOR@ header form is not supported: {RETIRED_KEYWORDS_HINT}"
            )));
        }
        if !self.consume("(") {
            return Err(HookError::Message(format!(
                "expected '(' after ANCHOR at {}",
                self.index
            )));
        }
        let target_raw = self.read_balanced_target()?;
        let target = target_raw.strip_prefix('@').ok_or_else(|| {
            HookError::Message(format!(
                "subscription target must be @source::task.stage.signal: {target_raw:?}"
            ))
        })?;
        let (source, signal) = target.split_once("::").ok_or_else(|| {
            HookError::Message(format!(
                "subscription target must be @source::task.stage.signal: {target_raw:?}"
            ))
        })?;
        if !is_plain_identifier(source) {
            return Err(HookError::Message(format!(
                "subscription source must be a plain identifier: {source:?}"
            )));
        }
        if source.len() > 36 {
            return Err(HookError::Message(format!(
                "subscription source exceeds the maximum length of 36 characters: {source:?}"
            )));
        }
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
        self.spans
            .push(Span::new(anchor_start, self.span_end_here()));
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
                    return Ok(self.input[start..self.index - 1].to_string());
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

const MAX_DELAY_SECONDS: i64 = 30 * 24 * 60 * 60;

pub(crate) fn duration_to_seconds(duration: &str) -> Result<i64> {
    if duration.len() < 2 {
        return Err(HookError::Message(format!("invalid duration: {duration}")));
    }
    let (num, unit) = match duration.char_indices().next_back() {
        Some((index, unit)) if unit.is_ascii() => (&duration[..index], unit),
        _ => return Err(HookError::Message(format!("invalid duration: {duration}"))),
    };
    if num.is_empty() || !num.chars().all(|ch| ch.is_ascii_digit()) {
        return Err(HookError::Message(format!("invalid duration: {duration}")));
    }
    if num.starts_with('0') {
        return Err(HookError::Message(format!("invalid duration: {duration}")));
    }
    let value = num
        .parse::<i64>()
        .map_err(|err| HookError::Message(format!("invalid duration {duration}: {err}")))?;
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
