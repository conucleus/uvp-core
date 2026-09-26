//! UVP Core Lint v1（`docs/product/prd_109_core_lint.md`，PRD 109）。
//!
//! Lint 是一个不改变协议语义的、保守的局部关系检查器：
//!
//! * 只分析已经通过 semantic validation 的合法表达式；
//! * 所有证明服从当前 UVP evaluator 的四值语义（Ready / Wait / Impossible /
//!   NeedsMore）与 readyAt / maturity 因果，经典布尔等价式未经论证不得直接
//!   搬用（因此 v1 没有 `A | ~A` tautology 规则，也不折叠嵌套延时）；
//! * `Cannot prove → don't lint.`：证明失败一律 `Unknown`，宁可漏报；
//! * lint 只读不改：不修改 runtime AST，不进 Cloud / EVM 产物，不影响
//!   parse / compile 的接受集合——`合法 DSL + lint error` 仍可编译，是否
//!   阻塞由调用方 deny policy 决定。
//!
//! 分层：本 crate 承担 Layer 1（单 Hook 局部规则 L001–L007）；同 Stage 的
//! Hook 关系规则（L020–L022）在 `uvp-compiler` 层，因为只有编译层看得到
//! Zhixu / Stage / hook 集合。跨 signal 业务公理（Layer 3）v1 完全不支持。

use crate::{ast::normalize_tight, Expr, HookError, Profile, SEMANTIC_VERSION};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use thiserror::Error;

/// L006：boolean nesting depth 的业务可维护性预算。与 `MAX_PARSE_DEPTH`
/// （资源与安全限制）完全不同维度，二者不得混用。
pub const MAX_LINT_BOOLEAN_DEPTH: usize = 8;
/// L007：单个 boolean group 的 operand 数量预算。
pub const MAX_LINT_BOOLEAN_OPERANDS: usize = 16;
/// 单 Hook lint 的节点预算：超过则跳过 O(n²) 的两两规则（L001–L005），
/// 只保留线性规则（L006 / L007）。lint 是旁路分析，不得成为新的资源
/// 攻击面，更不得影响 compile。
pub const MAX_LINT_NODES: usize = 4096;
/// 同 Stage 两两 Hook 关系分析的 hook 数量预算（uvp-compiler 层消费）。
pub const MAX_PAIRWISE_HOOKS: usize = 64;
/// 单次 ready_implies 证明的递归步预算：证明永远只在严格变小的子树上
/// 递归（理论上有限），预算是防御纵深——耗尽即 `Unknown`，绝不发散。
const IMPLICATION_BUDGET: usize = 10_000;

/// 源码字节区间（相对 hook 条件原文，即 `::` 之后的部分）。span 只用于
/// parser / lint / diagnostics，不进入 Cloud protocol artifact，不改变
/// runtime AST 的语义。
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

/// 结构化证明理由（PRD §18）：不是只告诉作者"有问题"，而是告诉作者
/// "为什么能够确定这是问题"。v1 不要求递归 proof tree 完整化。
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

/// 证明结果只有两值：可证 / 不可证。不提供 Probably / Likely / Heuristic。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProofResult {
    Proven(ProofReason),
    Unknown,
}

/// ready implication 的局部证明依据，序列化为 `LintProof.rule`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProofReason {
    /// 结构同一（a ≡ b）。
    Reflexive,
    /// AND ready ⇒ 每个成员 ready。
    AndMember,
    /// OR ready ⇒ 至少一个成员 ready。
    OrMember,
    /// Delay ready ⇒ 内层已 ready。
    DelayReady,
    /// 同一操作数 d1 ≥ d2：长延时就绪时刻不早于短延时。
    DelayDominance,
    /// OR 的每个分支都 ⇒ 目标（无论哪个分支获胜）。
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

/// 确定性语义指纹：tight 归一化串。结构相同的表达式必得相同指纹；
/// 指纹不同不构成任何结论（指纹是等价的必要证据，不是充分证明——
/// 充分证明仍由 `same_expr` / `ready_implies` 负责）。
pub fn semantic_fingerprint(expr: &Expr) -> String {
    normalize_tight(expr)
}

/// 结构同一性（含延时原文与秒数）。是唯一允许作为"确定相同 subtree"
/// 判据的比较。
pub fn same_expr(a: &Expr, b: &Expr) -> bool {
    a == b
}

/// ready implication：表达式 `a` Ready 时能否**确定**表达式 `b` 也 Ready。
/// 证明失败必须返回 `Unknown`（PRD §8）。注意它不是 safe rewrite 的充分
/// 条件（PRD §9）：`A & B` ⇒ `A` 可证，但把 `A & B` 改写成 `A` 需要
/// state identical + readyAt identical，v1 不做任何改写。
pub fn ready_implies(a: &Expr, b: &Expr) -> ProofResult {
    let mut budget = IMPLICATION_BUDGET;
    implies(a, b, &mut budget)
}

/// UVP evaluator 语义下的局部 sound 推理：
///
/// * AND ready ⇒ 每个成员 ready ⇒ 任一成员所蕴含的都成立；
/// * Delay ready ⇒ 内层 ready；
/// * 同一内层的 d1 ≥ d2 延时：长延时就绪 ⇒ 短延时必已就绪（就绪时刻
///   t+d1 ≥ t+d2，且事实集只会增长——重复事实取最早 received_at 的
///   事实模型下信号存在性单调）；
/// * OR ready ⇒ 某个成员 ready：只有**每个**成员都蕴含同一目标时才可
///   下结论（无论获胜分支是谁）；
/// * `Not` / `Signal` / `Subscription` 不分解（负向就绪不蕴含任何正向
///   就绪；单信号只蕴含自身——由 Reflexive 覆盖）；
/// * 目标侧：a ⇒ Or(ts) 当且仅当 a ⇒ 某个 t；a ⇒ And(ts) 当且仅当
///   a ⇒ 每个 t。
///
/// 经典布尔等价式（吸收律、分配律、tautology）一律不进入这里——UVP 是
/// 四值 + readyAt 语义，未经 evaluator 论证的等价不得使用。
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

/// OR 分支集的交集：只有**每个**分支都强制的关系，才对"无论哪个分支
/// 获胜"成立（OR ready ⇒ 某个分支 ready，获胜者未知）。
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

/// 表达式 Ready 时被**强制在场**的信号集（信号存在性单调：一旦在场
/// 即保持，与 evaluator 的"重复事实取最早 received_at"事实模型一致）。
///
/// * Signal → 自身；Delay 透传；AND 取并集（每个成员都 ready）；
/// * OR 取交集——无论哪个分支获胜都必须强制该信号，才可下结论。
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

/// 表达式 Ready 时被**强制缺席**的信号集（`~S` 就绪 = S 不在场）。
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

/// 可证明的同时就绪矛盾：`a` 强制在场而 `b` 强制缺席（或对称）的信号。
/// 返回该信号名作为证明依据。无法证明返回 `None`。
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

/// 局部可证的"永不可能 Ready"（L005 dead-or-branch 的判定）：
/// * conjunction 内含矛盾对（含嵌套）或有成员自身不可能；
/// * delay 透传内层；
/// * OR 需要全部分支都不可能才整体不可能；
/// * 裸信号 / 负向条件永远无法局部证明不可能（信号可能永不到达 ⇒
///   `~S` 可能就绪；S 可能到达 ⇒ 无法断言永不就绪）。
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

/// boolean nesting depth（L006）：AND / OR 各计一层，`~` 与延时透明透传。
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

/// 位于 lint 侧的带 span 只读树：`expr` 是 runtime AST 节点的克隆，
/// `children` 与该节点的操作数一一对应。解析器在节点创建期把 span 推入
/// 侧表（次序恰为最终树的后序遍历次序），这里按后序配对还原；数量不
/// 匹配（解析器实现漂移的防御分支）时整体降级为无 span 树——lint 结论
/// 不变，只是 diagnostic 不带位置（PRD §26 第 16 条"能获取位置时"）。
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

/// 单 Hook lint 的完整上下文（uvp-compiler 层做同 Stage 关系分析时复用
/// 已解析的条件与归一化表达式，避免二次解析）。
pub struct HookLintResult {
    pub report: LintReport,
    /// 语义验证通过的原始条件（只读参考，供 Layer 2 关系分析）。
    pub condition: Expr,
    /// 条件内引用的事实所属的 source 类（标头；订阅钩子为空串）。同
    /// Stage 关系证明必须在 source 相同的前提下进行——不同 source 的
    /// 同名信号是不同事实，任何跨 source 蕴含都不可证。
    pub source: String,
    /// 含 source 标头的归一化表达式（`source::condition`）。
    pub normalized_expression: String,
}

/// 解析 + 语义验证 + Layer 1 规则。语义验证失败是 `Err`（非法 DSL 不进入
/// lint，PRD §4.2），不是 diagnostic。gate 决定走钩子档还是过滤档校验
/// （发射适格面的合法形态在过滤档下不得被 lint 误报）。
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

/// 最低层 lint API（PRD §19）。
pub fn lint_hook(
    profile: Profile,
    gate: crate::Gate,
    hook_name: &str,
    hook: &str,
) -> Result<LintReport, LintError> {
    lint_hook_with_condition(profile, gate, hook_name, hook).map(|result| result.report)
}

pub fn lint_spanned_tree(tree: &SpannedExpr, hook_name: &str) -> Vec<LintDiagnostic> {
    // 订阅钩子条件是单节点 Subscription：没有任何 boolean group，Layer 1
    // 无规则可施（重复订阅目标由 Layer 2 的 L020 负责）。
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

/// UVP-L001 duplicate-term：同一 boolean group 中确定相同的 operand
/// （结构同一，含延时）。`A & A`、`A | B | A`、`(A & B) | (A & B)`。
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

/// UVP-L002 impossible-condition：conjunction 内可局部严格证明永不 Ready
/// 的矛盾对（`A & ~A`、`A +10s & ~A`：延时 Ready ⇒ 信号已存在，而负向
/// guard 要求其缺席）。
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

/// UVP-L003 absorbed-term：直接吸收。AND 中 `A & (A | B)`（被蕴含的成员
/// 不影响就绪），OR 中 `A | (A & B)`（蕴含他人的成员支配整个 OR）。只做
/// 局部、确定结构，不做 DNF/CNF 展开。指纹相同的对已由 L001 报告；同一
/// 基座的多延时对（`A +10s & A +5s`）归 L004 专责，此处跳过，避免同一
/// 事实双报（PRD §11：same-signal multiple delay 是 dominated-delay 的
/// 内部检测形态）。
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
            // AND：p ⇒ q 时 q 被吸收（AND 就绪需要全部成员，q 的就绪已被
            // p 保证）。OR：q ⇒ p 时 q 被吸收（q 就绪时 p 必就绪，q 永远
            // 不是 OR 的独立成因）。
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

/// 同一基座（内层表达式结构同一）的延时对：`A +10s` 与 `A +5s`。该形态
/// 的支配关系归 L004 dominated-delay 专责报告。
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

/// UVP-L004 dominated-delay：同一 normalized operand 带多个 delay。
/// `A +10s & A +5s`（AND：较短延时冗余）、`A +10s | A +5s`（OR：较长
/// 分支冗余）。证明基础是 delay dominance（d1 ≥ d2 ⇒ 长延时就绪时短延时
/// 必已就绪）。基座（内层表达式）必须结构同一；嵌套延时不做任何折叠
/// 建议（PRD §5.2）。
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
            // 完全相同的延时对是 L001 的 identical subtree；不重复报告。
            continue;
        }
        // AND：最长延时支配（较短冗余）；OR：最短延时支配（较长冗余）。
        // std 的 min_by_key/max_by_key 并列时取最后一个遍历项——被支配项
        // 存在时长并列的孪生（部分重复形态，如 (A+10s)|(A+10s)|(A+5s)）
        // 时，支配者必须按严格时长差选取：AND 严格长于被支配项、OR 严格
        // 短于被支配项。时长相同的孪生既不是支配者（会产出 "`A+10s` is
        // dominated by `A+10s`" 的自引诊断，且与 L001 的相同子树报告重复），
        // 也不归 L004 报告；无严格支配者（纯重复组）时整组让位给 L001。
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
            // delay dominance 的证明方向固定为"长延时就绪 ⇒ 短延时必已就绪"
            // （函数头注释）。AND 的被支配项是短延时，OR 的被支配项是长延时
            // ——premise（长）随分支取自不同成员，不能统一取
            // (dominator, dominated)：OR 下那样会写出
            // "短就绪 ⇒ 长就绪" 的不可证方向（2609100328 L4）。
            proof: Some(if is_and {
                LintProof {
                    kind: "ready_implication",
                    rule: Some("delay_dominance"),
                    premise: Some(expr_str(&dominator.expr)),
                    conclusion: Some(expr_str(&dominated.expr)),
                }
            } else {
                // OR：dominated 是最长延时（premise=长），dominator 是其余
                // 成员中最长者（恒短于 dominated），蕴含式为真。
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

/// UVP-L005 dead-or-branch：OR 中确定 Impossible 的 branch（如
/// `(A & ~A) | B` 的左分支）。
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
