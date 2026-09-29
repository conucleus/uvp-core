//! UVP Core Lint v1（`docs/product/prd_109_core_lint.md`，PRD 109）。

use crate::{ast::normalize_tight, Expr, HookError, Profile, SEMANTIC_VERSION};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use thiserror::Error;

pub const MAX_LINT_BOOLEAN_DEPTH: usize = 8;
pub const MAX_LINT_BOOLEAN_OPERANDS: usize = 16;
pub const MAX_LINT_NODES: usize = 4096;
pub const MAX_PAIRWISE_HOOKS: usize = 64;
const IMPLICATION_BUDGET: usize = 10_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Span {
    pub start_byte: usize,
    pub end_byte: usize,
}

impl Span {
    pub fn new(start_byte: usize, end_byte: usize) -> Self {
        Self {
            start_byte,
            end_byte,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Error,
    Warning,
    Info,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    Correctness,
    Redundancy,
    Temporal,
    Relation,
    Maintainability,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RelatedSpan {
    pub span: Span,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LintProof {
    pub kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rule: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub premise: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conclusion: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LintDiagnostic {
    pub code: &'static str,
    pub severity: Severity,
    pub category: Category,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hook_name: Option<String>,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub explanation: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub primary_span: Option<Span>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub related_spans: Vec<RelatedSpan>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proof: Option<LintProof>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LintReport {
    pub semantic_version: String,
    pub diagnostics: Vec<LintDiagnostic>,
}

#[derive(Debug, Error)]
pub enum LintError {
    #[error("{0}")]
    Message(String),
}

impl From<HookError> for LintError {
    fn from(err: HookError) -> Self {
        LintError::Message(err.to_string())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProofResult {
    Proven(ProofReason),
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProofReason {
    Reflexive,
    AndMember,
    OrMember,
    DelayReady,
    DelayDominance,
    OrBranches,
}

impl ProofReason {
    pub fn as_rule(self) -> &'static str {
        match self {
            ProofReason::Reflexive => "reflexive",
            ProofReason::AndMember => "and_member",
            ProofReason::OrMember => "or_member",
            ProofReason::DelayReady => "delay_ready",
            ProofReason::DelayDominance => "delay_dominance",
            ProofReason::OrBranches => "or_branches",
        }
    }
}

pub fn semantic_fingerprint(expr: &Expr) -> String {
    normalize_tight(expr)
}

pub fn same_expr(a: &Expr, b: &Expr) -> bool {
    a == b
}

pub fn ready_implies(a: &Expr, b: &Expr) -> ProofResult {
    let mut budget = IMPLICATION_BUDGET;
    implies(a, b, &mut budget)
}

fn implies(a: &Expr, b: &Expr, budget: &mut usize) -> ProofResult {
    if *budget == 0 {
        return ProofResult::Unknown;
    }
    *budget -= 1;
    if same_expr(a, b) {
        return ProofResult::Proven(ProofReason::Reflexive);
    }
    match a {
        Expr::And(terms) => {
            for term in terms {
                if let ProofResult::Proven(reason) = implies(term, b, budget) {
                    return ProofResult::Proven(reason);
                }
            }
        }
        Expr::Or(terms) => {
            let mut all = !terms.is_empty();
            for term in terms {
                if implies(term, b, budget) == ProofResult::Unknown {
                    all = false;
                    break;
                }
            }
            if all {
                return ProofResult::Proven(ProofReason::OrBranches);
            }
        }
        Expr::Delay {
            expr: inner,
            duration_seconds,
            ..
        } => {
            if let Expr::Delay {
                expr: b_inner,
                duration_seconds: b_duration,
                ..
            } = b
            {
                if same_expr(inner, b_inner) && duration_seconds >= b_duration {
                    return ProofResult::Proven(ProofReason::DelayDominance);
                }
            }
            if let ProofResult::Proven(_) = implies(inner, b, budget) {
                return ProofResult::Proven(ProofReason::DelayReady);
            }
        }
        Expr::Signal(_) | Expr::Subscription { .. } | Expr::Not(_) => {}
    }
    match b {
        Expr::Or(b_terms) => {
            for term in b_terms {
                if let ProofResult::Proven(reason) = implies(a, term, budget) {
                    return ProofResult::Proven(reason);
                }
            }
        }
        Expr::And(b_terms) => {
            let mut all = !b_terms.is_empty();
            for term in b_terms {
                if implies(a, term, budget) == ProofResult::Unknown {
                    all = false;
                    break;
                }
            }
            if all {
                return ProofResult::Proven(ProofReason::AndMember);
            }
        }
        Expr::Signal(_) | Expr::Subscription { .. } | Expr::Not(_) | Expr::Delay { .. } => {}
    }
    ProofResult::Unknown
}

fn intersect_branch_sets(
    terms: &[Expr],
    collect: fn(&Expr, &mut BTreeSet<String>),
    out: &mut BTreeSet<String>,
) {
    let mut intersection: Option<BTreeSet<String>> = None;
    for term in terms {
        let mut term_set = BTreeSet::new();
        collect(term, &mut term_set);
        intersection = Some(match intersection {
            None => term_set,
            Some(current) => current.intersection(&term_set).cloned().collect(),
        });
    }
    for key in intersection.into_iter().flatten() {
        out.insert(key);
    }
}

fn forced_positive(expr: &Expr, out: &mut BTreeSet<String>) {
    match expr {
        Expr::Signal(signal) => {
            out.insert(signal.clone());
        }
        Expr::Subscription { .. } | Expr::Not(_) => {}
        Expr::Delay { expr: inner, .. } => forced_positive(inner, out),
        Expr::And(terms) => {
            for term in terms {
                forced_positive(term, out);
            }
        }
        Expr::Or(terms) => intersect_branch_sets(terms, forced_positive, out),
    }
}

fn forced_negative(expr: &Expr, out: &mut BTreeSet<String>) {
    match expr {
        Expr::Not(inner) => {
            if let Expr::Signal(signal) = inner.as_ref() {
                out.insert(signal.clone());
            }
        }
        Expr::Signal(_) | Expr::Subscription { .. } => {}
        Expr::Delay { expr: inner, .. } => forced_negative(inner, out),
        Expr::And(terms) => {
            for term in terms {
                forced_negative(term, out);
            }
        }
        Expr::Or(terms) => intersect_branch_sets(terms, forced_negative, out),
    }
}

pub fn contradicts(a: &Expr, b: &Expr) -> Option<String> {
    let mut a_pos = BTreeSet::new();
    forced_positive(a, &mut a_pos);
    let mut b_neg = BTreeSet::new();
    forced_negative(b, &mut b_neg);
    if let Some(signal) = a_pos.intersection(&b_neg).next() {
        return Some(signal.clone());
    }
    let mut a_neg = BTreeSet::new();
    forced_negative(a, &mut a_neg);
    let mut b_pos = BTreeSet::new();
    forced_positive(b, &mut b_pos);
    a_neg.intersection(&b_pos).next().cloned()
}

fn provably_impossible(expr: &Expr) -> bool {
    match expr {
        Expr::Signal(_) | Expr::Subscription { .. } | Expr::Not(_) => false,
        Expr::Delay { expr: inner, .. } => provably_impossible(inner),
        Expr::And(terms) => {
            terms.iter().any(provably_impossible)
                || terms.iter().any(|left| {
                    terms.iter().any(|right| {
                        !std::ptr::eq(left, right) && contradicts(left, right).is_some()
                    })
                })
        }
        Expr::Or(terms) => terms.iter().all(provably_impossible),
    }
}

fn boolean_depth(expr: &Expr) -> usize {
    match expr {
        Expr::Signal(_) | Expr::Subscription { .. } => 0,
        Expr::Not(inner) => boolean_depth(inner),
        Expr::Delay { expr: inner, .. } => boolean_depth(inner),
        Expr::And(terms) | Expr::Or(terms) => {
            1 + terms.iter().map(boolean_depth).max().unwrap_or_default()
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpannedExpr {
    pub expr: Expr,
    pub span: Span,
    pub children: Vec<SpannedExpr>,
}

impl SpannedExpr {
    pub fn build(expr: &Expr, spans: &[Span]) -> SpannedExpr {
        let mut iter = spans.iter().copied();
        match Self::try_build(expr, &mut iter) {
            Some(tree) if iter.next().is_none() => tree,
            _ => Self::spanless(expr),
        }
    }

    fn try_build(
        expr: &Expr,
        spans: &mut std::iter::Copied<std::slice::Iter<'_, Span>>,
    ) -> Option<SpannedExpr> {
        let children = match expr {
            Expr::Signal(_) | Expr::Subscription { .. } => Vec::new(),
            Expr::Not(inner) => vec![Self::try_build(inner, spans)?],
            Expr::And(terms) | Expr::Or(terms) => terms
                .iter()
                .map(|term| Self::try_build(term, spans))
                .collect::<Option<Vec<_>>>()?,
            Expr::Delay { expr: inner, .. } => vec![Self::try_build(inner, spans)?],
        };
        let span = spans.next()?;
        Some(SpannedExpr {
            expr: expr.clone(),
            span,
            children,
        })
    }

    fn spanless(expr: &Expr) -> SpannedExpr {
        let children = match expr {
            Expr::Signal(_) | Expr::Subscription { .. } => Vec::new(),
            Expr::Not(inner) => vec![Self::spanless(inner)],
            Expr::And(terms) | Expr::Or(terms) => terms.iter().map(Self::spanless).collect(),
            Expr::Delay { expr: inner, .. } => vec![Self::spanless(inner)],
        };
        SpannedExpr {
            expr: expr.clone(),
            span: Span::new(0, 0),
            children,
        }
    }

    fn node_count(&self) -> usize {
        1 + self
            .children
            .iter()
            .map(SpannedExpr::node_count)
            .sum::<usize>()
    }
}

pub struct HookLintResult {
    pub report: LintReport,
    pub condition: Expr,
    pub source: String,
    pub normalized_expression: String,
}

pub fn lint_hook_with_condition(
    _profile: Profile,
    gate: crate::Gate,
    hook_name: &str,
    hook: &str,
) -> Result<HookLintResult, LintError> {
    crate::parser::validate_hook_name(hook_name)?;
    let (hook_expr, spans) = crate::parser::parse_hook_expr_with_spans(hook)?;
    match gate {
        crate::Gate::Hook => crate::ast::validate_hook(&hook_expr.condition)?,
        crate::Gate::Filter => crate::ast::validate_filter_hook(&hook_expr.condition)?,
    }
    let normalized_expression = format!(
        "{}::{}",
        hook_expr.source,
        normalize_tight(&hook_expr.condition)
    );
    let tree = SpannedExpr::build(&hook_expr.condition, &spans);
    let diagnostics = lint_spanned_tree(&tree, hook_name);
    Ok(HookLintResult {
        report: LintReport {
            semantic_version: SEMANTIC_VERSION.to_string(),
            diagnostics,
        },
        condition: hook_expr.condition,
        source: hook_expr.source,
        normalized_expression,
    })
}

pub fn lint_hook(
    profile: Profile,
    gate: crate::Gate,
    hook_name: &str,
    hook: &str,
) -> Result<LintReport, LintError> {
    lint_hook_with_condition(profile, gate, hook_name, hook).map(|result| result.report)
}

pub fn lint_spanned_tree(tree: &SpannedExpr, hook_name: &str) -> Vec<LintDiagnostic> {
    if matches!(tree.expr, Expr::Subscription { .. }) {
        return Vec::new();
    }
    let expensive = tree.node_count() <= MAX_LINT_NODES;
    let mut diagnostics = Vec::new();
    if boolean_depth(&tree.expr) > MAX_LINT_BOOLEAN_DEPTH {
        diagnostics.push(lint_excessive_boolean_depth(tree, hook_name));
    }
    collect_group_diagnostics(tree, hook_name, expensive, &mut diagnostics);
    diagnostics.sort_by(|left, right| {
        left.code
            .cmp(right.code)
            .then(
                left.primary_span
                    .unwrap_or(Span::new(0, 0))
                    .start_byte
                    .cmp(&right.primary_span.unwrap_or(Span::new(0, 0)).start_byte),
            )
            .then(left.message.cmp(&right.message))
    });
    diagnostics.dedup_by(|left, right| left == right);
    diagnostics
}

fn collect_group_diagnostics(
    node: &SpannedExpr,
    hook_name: &str,
    expensive: bool,
    out: &mut Vec<LintDiagnostic>,
) {
    if matches!(node.expr, Expr::And(_) | Expr::Or(_)) {
        if node.children.len() > MAX_LINT_BOOLEAN_OPERANDS {
            out.push(lint_excessive_operand_count(node, hook_name));
        }
        if expensive {
            lint_duplicate_terms(node, hook_name, out);
            lint_absorbed_terms(node, hook_name, out);
            lint_dominated_delays(node, hook_name, out);
            if matches!(node.expr, Expr::And(_)) {
                lint_impossible_conjunction(node, hook_name, out);
            } else {
                lint_dead_or_branches(node, hook_name, out);
            }
        }
    }
    for child in &node.children {
        collect_group_diagnostics(child, hook_name, expensive, out);
    }
}

fn group_kind(node: &SpannedExpr) -> &'static str {
    match node.expr {
        Expr::And(_) => "AND",
        Expr::Or(_) => "OR",
        _ => "boolean",
    }
}

fn expr_str(expr: &Expr) -> String {
    normalize_tight(expr)
}

fn lint_duplicate_terms(group: &SpannedExpr, hook_name: &str, out: &mut Vec<LintDiagnostic>) {
    let mut seen: BTreeMap<String, Vec<&SpannedExpr>> = BTreeMap::new();
    for child in &group.children {
        seen.entry(expr_str(&child.expr)).or_default().push(child);
    }
    for (_, occurrences) in seen {
        if occurrences.len() < 2 {
            continue;
        }
        let first = occurrences[0];
        let fingerprint = expr_str(&first.expr);
        out.push(LintDiagnostic {
            code: "UVP-L001",
            severity: Severity::Warning,
            category: Category::Redundancy,
            hook_name: Some(hook_name.to_string()),
            message: format!(
                "duplicate operand in {} group: `{fingerprint}` appears {} times",
                group_kind(group),
                occurrences.len()
            ),
            explanation: Some(format!(
                "operand `{fingerprint}` is structurally identical; the extra occurrences can never change the group's readiness"
            )),
            primary_span: Some(first.span),
            related_spans: occurrences[1..]
                .iter()
                .map(|occurrence| RelatedSpan {
                    span: occurrence.span,
                    label: "duplicate".to_string(),
                })
                .collect(),
            proof: Some(LintProof {
                kind: "duplicate_term",
                rule: Some("identical_subtree"),
                premise: Some(fingerprint.clone()),
                conclusion: Some(fingerprint),
            }),
        });
    }
}

fn lint_impossible_conjunction(
    group: &SpannedExpr,
    hook_name: &str,
    out: &mut Vec<LintDiagnostic>,
) {
    for (index, left) in group.children.iter().enumerate() {
        for right in &group.children[index + 1..] {
            let Some(signal) = contradicts(&left.expr, &right.expr) else {
                continue;
            };
            out.push(LintDiagnostic {
                code: "UVP-L002",
                severity: Severity::Error,
                category: Category::Correctness,
                hook_name: Some(hook_name.to_string()),
                message: format!(
                    "impossible condition: `{}` and `{}` conflict on signal `{signal}`",
                    expr_str(&left.expr),
                    expr_str(&right.expr)
                ),
                explanation: Some(format!(
                    "one operand can only be ready with `{signal}` present while the other can \
                     only be ready with it absent; the conjunction can never become ready"
                )),
                primary_span: Some(left.span),
                related_spans: vec![RelatedSpan {
                    span: right.span,
                    label: "conflicting operand".to_string(),
                }],
                proof: Some(LintProof {
                    kind: "contradiction",
                    rule: Some("signal_polarity"),
                    premise: Some(expr_str(&left.expr)),
                    conclusion: Some(expr_str(&right.expr)),
                }),
            });
        }
    }
}

fn lint_absorbed_terms(group: &SpannedExpr, hook_name: &str, out: &mut Vec<LintDiagnostic>) {
    let is_and = matches!(group.expr, Expr::And(_));
    for (index, left) in group.children.iter().enumerate() {
        for right in &group.children[index + 1..] {
            if expr_str(&left.expr) == expr_str(&right.expr) {
                continue;
            }
            if same_base_delays(&left.expr, &right.expr) {
                continue;
            }
            let (absorbed, dominator) = if is_and {
                match ready_implies(&left.expr, &right.expr) {
                    ProofResult::Proven(_) => (right, left),
                    ProofResult::Unknown => match ready_implies(&right.expr, &left.expr) {
                        ProofResult::Proven(_) => (left, right),
                        ProofResult::Unknown => continue,
                    },
                }
            } else {
                match ready_implies(&right.expr, &left.expr) {
                    ProofResult::Proven(_) => (right, left),
                    ProofResult::Unknown => match ready_implies(&left.expr, &right.expr) {
                        ProofResult::Proven(_) => (left, right),
                        ProofResult::Unknown => continue,
                    },
                }
            };
            out.push(LintDiagnostic {
                code: "UVP-L003",
                severity: Severity::Warning,
                category: Category::Redundancy,
                hook_name: Some(hook_name.to_string()),
                message: format!(
                    "absorbed operand in {} group: `{}` is absorbed by `{}`",
                    group_kind(group),
                    expr_str(&absorbed.expr),
                    expr_str(&dominator.expr)
                ),
                explanation: Some(if is_and {
                    "whenever the dominator is ready the absorbed operand is already ready, so it \
                     never gates the conjunction"
                        .to_string()
                } else {
                    "whenever the absorbed operand is ready the dominator is also ready, so that \
                     branch never uniquely makes the disjunction ready"
                        .to_string()
                }),
                primary_span: Some(absorbed.span),
                related_spans: vec![RelatedSpan {
                    span: dominator.span,
                    label: "dominating operand".to_string(),
                }],
                proof: Some(LintProof {
                    kind: "absorption",
                    rule: Some(if is_and {
                        "and_absorption"
                    } else {
                        "or_absorption"
                    }),
                    premise: Some(expr_str(&dominator.expr)),
                    conclusion: Some(expr_str(&absorbed.expr)),
                }),
            });
        }
    }
}

fn same_base_delays(left: &Expr, right: &Expr) -> bool {
    match (left, right) {
        (
            Expr::Delay {
                expr: left_base, ..
            },
            Expr::Delay {
                expr: right_base, ..
            },
        ) => same_expr(left_base, right_base),
        _ => false,
    }
}

fn lint_dominated_delays(group: &SpannedExpr, hook_name: &str, out: &mut Vec<LintDiagnostic>) {
    let is_and = matches!(group.expr, Expr::And(_));
    let mut delay_groups: BTreeMap<String, Vec<&SpannedExpr>> = BTreeMap::new();
    for child in &group.children {
        if let Expr::Delay { expr: base, .. } = &child.expr {
            delay_groups.entry(expr_str(base)).or_default().push(child);
        }
    }
    for (_, members) in delay_groups {
        if members.len() < 2 {
            continue;
        }
        let durations: Vec<i64> = members
            .iter()
            .map(|member| match &member.expr {
                Expr::Delay {
                    duration_seconds, ..
                } => *duration_seconds,
                _ => 0,
            })
            .collect();
        if durations.windows(2).all(|window| window[0] == window[1]) {
            continue;
        }
        let dominated_index = if is_and {
            durations
                .iter()
                .enumerate()
                .min_by_key(|(_, duration)| **duration)
                .map(|(index, _)| index)
        } else {
            durations
                .iter()
                .enumerate()
                .max_by_key(|(_, duration)| **duration)
                .map(|(index, _)| index)
        };
        let Some(dominated_index) = dominated_index else {
            continue;
        };
        let dominated = members[dominated_index];
        let dominated_duration = match &dominated.expr {
            Expr::Delay {
                duration_seconds, ..
            } => *duration_seconds,
            _ => 0,
        };
        let strictly_dominates = |candidate: i64| {
            if is_and {
                candidate > dominated_duration
            } else {
                candidate < dominated_duration
            }
        };
        let dominator = members
            .iter()
            .enumerate()
            .filter(|(index, member)| {
                *index != dominated_index
                    && matches!(&member.expr,
                        Expr::Delay { duration_seconds, .. } if strictly_dominates(*duration_seconds))
            })
            .map(|(_, member)| member)
            .max_by_key(|member| match &member.expr {
                Expr::Delay {
                    duration_seconds, ..
                } => *duration_seconds,
                _ => 0,
            });
        let Some(dominator) = dominator else {
            continue;
        };
        out.push(LintDiagnostic {
            code: "UVP-L004",
            severity: Severity::Warning,
            category: Category::Temporal,
            hook_name: Some(hook_name.to_string()),
            message: format!(
                "dominated delay in {} group: `{}` is dominated by `{}`",
                group_kind(group),
                expr_str(&dominated.expr),
                expr_str(&dominator.expr)
            ),
            explanation: Some(if is_and {
                "the conjunction only becomes ready after the longest delay, so the shorter \
                 delay on the same operand is redundant"
                    .to_string()
            } else {
                "the disjunction becomes ready at the earliest branch, so the longer delay on \
                 the same operand never uniquely fires"
                    .to_string()
            }),
            primary_span: Some(dominated.span),
            related_spans: vec![RelatedSpan {
                span: dominator.span,
                label: "dominating delay".to_string(),
            }],
            proof: Some(if is_and {
                LintProof {
                    kind: "ready_implication",
                    rule: Some("delay_dominance"),
                    premise: Some(expr_str(&dominator.expr)),
                    conclusion: Some(expr_str(&dominated.expr)),
                }
            } else {
                LintProof {
                    kind: "ready_implication",
                    rule: Some("delay_dominance"),
                    premise: Some(expr_str(&dominated.expr)),
                    conclusion: Some(expr_str(&dominator.expr)),
                }
            }),
        });
    }
}

fn lint_dead_or_branches(group: &SpannedExpr, hook_name: &str, out: &mut Vec<LintDiagnostic>) {
    for branch in &group.children {
        if !provably_impossible(&branch.expr) {
            continue;
        }
        out.push(LintDiagnostic {
            code: "UVP-L005",
            severity: Severity::Warning,
            category: Category::Correctness,
            hook_name: Some(hook_name.to_string()),
            message: format!(
                "dead OR branch: `{}` can never become ready",
                expr_str(&branch.expr)
            ),
            explanation: Some(
                "the branch contains a locally provable contradiction and never contributes to \
                 the disjunction's readiness"
                    .to_string(),
            ),
            primary_span: Some(branch.span),
            related_spans: Vec::new(),
            proof: Some(LintProof {
                kind: "contradiction",
                rule: Some("impossible_branch"),
                premise: Some(expr_str(&branch.expr)),
                conclusion: None,
            }),
        });
    }
}

fn lint_excessive_boolean_depth(root: &SpannedExpr, hook_name: &str) -> LintDiagnostic {
    let depth = boolean_depth(&root.expr);
    LintDiagnostic {
        code: "UVP-L006",
        severity: Severity::Warning,
        category: Category::Maintainability,
        hook_name: Some(hook_name.to_string()),
        message: format!(
            "boolean nesting depth {depth} exceeds the maintainability budget of \
             {MAX_LINT_BOOLEAN_DEPTH}"
        ),
        explanation: Some(
            "deeply nested boolean conditions are hard to review; this is a maintainability \
             budget, unrelated to the parser's resource depth limit"
                .to_string(),
        ),
        primary_span: Some(root.span),
        related_spans: Vec::new(),
        proof: None,
    }
}

fn lint_excessive_operand_count(group: &SpannedExpr, hook_name: &str) -> LintDiagnostic {
    LintDiagnostic {
        code: "UVP-L007",
        severity: Severity::Warning,
        category: Category::Maintainability,
        hook_name: Some(hook_name.to_string()),
        message: format!(
            "{} group has {} operands, exceeding the maintainability budget of \
             {MAX_LINT_BOOLEAN_OPERANDS}",
            group_kind(group),
            group.children.len()
        ),
        explanation: Some(
            "a very large boolean group usually encodes too much history into a single hook; \
             consider splitting it"
                .to_string(),
        ),
        primary_span: Some(group.span),
        related_spans: Vec::new(),
        proof: None,
    }
}
