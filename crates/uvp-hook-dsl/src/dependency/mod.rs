//! 依赖提取：从 hook 条件 AST 提取 positive/negative/timer 依赖清单。

use serde::Serialize;
use std::collections::BTreeSet;

use crate::{ast::Expr, HookExpr, Profile};

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

pub(crate) fn extract_dependencies(hook: &HookExpr, _profile: Profile) -> Vec<Dependency> {
    let mut deps = Vec::new();
    collect_dependencies(&hook.condition, &hook.source, false, &mut deps);
    dedupe_dependencies(deps)
}

/// 收集延时操作数子树内的正向事实锚点（延时节点自身的 timer 依赖由
/// collect_dependencies 的 Delay 分支产出）。语法面两档拒绝嵌套延时后，
/// 操作数子树不含 Delay 节点，锚点即操作数内的正向信号本身；否定子树
/// 不产生锚（与 validate_anchors 的口径一致：延时要求正锚）。
fn collect_positive_anchors(expr: &Expr, source: &str, out: &mut Vec<(String, String)>) {
    match expr {
        Expr::Signal(signal) => out.push((source.to_string(), signal.clone())),
        // 解析期位置约束下订阅不可出现在延时操作数内；按 Signal 同形处理
        // 保持与正向依赖收集同口径。
        Expr::Subscription { source, target } => {
            out.push((source.clone(), target.clone()));
        }
        Expr::Not(_) => {}
        // 嵌套延时对合法输入不可达；防御性下钻保持 match 全覆盖。
        Expr::Delay { expr, .. } => collect_positive_anchors(expr, source, out),
        Expr::And(terms) | Expr::Or(terms) => {
            for term in terms {
                collect_positive_anchors(term, source, out);
            }
        }
    }
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
                // 每个延时节点产出一个 timer：到期 = 锚点事实到达 + 本层
                // 时长。嵌套延时被语法面拒绝，不存在内层到期累计。
                let mut anchors = Vec::new();
                collect_positive_anchors(expr, source, &mut anchors);
                for (anchor_source, signal_name) in anchors {
                    out.push(Dependency {
                        kind: DependencyKind::Timer,
                        source: anchor_source,
                        signal_name,
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
