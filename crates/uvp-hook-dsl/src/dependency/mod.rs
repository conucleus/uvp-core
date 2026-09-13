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

/// 收集延时操作数子树内的全部正向事实锚点，及其"从操作数根到该事实的
/// 累计延时"（路径上 Delay 节点的时长之和）。延时节点自身的 timer 依赖
/// 由 collect_dependencies 的 Delay 分支产出；此处只为外层延时计算
/// "操作数成熟时刻距事实到达的偏移"。否定子树不产生正向锚（与
/// validate_anchors 的口径一致：延时要求正锚）。
fn collect_positive_anchors(
    expr: &Expr,
    source: &str,
    offset: i64,
    out: &mut Vec<(String, String, i64)>,
) {
    match expr {
        Expr::Signal(signal) => out.push((source.to_string(), signal.clone(), offset)),
        // 解析期位置约束下订阅不可出现在延时操作数内；按 Signal 同形处理
        // 保持与正向依赖收集同口径。
        Expr::Subscription { source, target } => {
            out.push((source.clone(), target.clone(), offset));
        }
        Expr::Not(_) => {}
        Expr::Delay {
            expr,
            duration_seconds,
            ..
        } => collect_positive_anchors(expr, source, offset + duration_seconds, out),
        Expr::And(terms) | Expr::Or(terms) => {
            for term in terms {
                collect_positive_anchors(term, source, offset, out);
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
                // 链式延时的外层 timer 必须基于内层到期累计：(A+5s)+10s 的
                // 真实到期是 A+15s——只按本层时长出 timer(A,10s) 会把最终
                // 到期低估一个内层延时。内层延时节点自己的 timer（如
                // timer(A,5s) 的中间 poke 期限）由上方递归照常产出，与求值
                // 器的分段等待口径一致（内层到期前上浮内层 due，poke 后
                // 本层再按自身时长推进）。
                let mut inner_anchors = Vec::new();
                collect_positive_anchors(expr, source, 0, &mut inner_anchors);
                for (anchor_source, signal_name, inner_offset) in inner_anchors {
                    out.push(Dependency {
                        kind: DependencyKind::Timer,
                        source: anchor_source,
                        signal_name,
                        delay_seconds: Some(inner_offset + duration_seconds),
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
