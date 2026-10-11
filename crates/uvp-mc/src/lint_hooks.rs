//! lint-hooks：守卫根层合取项的可达性冗余检测（advisory——只建议可删
//! 项，不拒绝定义、不挂任何门）。判定式：合取项可删 ⟺ 穷举图中不存在
//! 「其余合取项就绪而该项不就绪」的可达状态。分析只吃定义本身：落地面
//! 约束唯一采信定义自身的适格面（validWhen 由引擎在发射事务内强制），
//! 无适格门的事实按可落地也可永不落地自由探索——对跨事实时序（回执先
//! 后、出生同捆）零耦合假设，可达集是真实可达集的过近似，故只可能漏报
//! 不可能误报。求值单源在 uvp-hook-dsl 真求值器。

use serde::Serialize;
use uvp_hook_dsl::{DecodedCompiledHook, EvalState, Expr, HookMode};

use crate::explore::Explorer;
use crate::manifest::Manifest;
use crate::model::{eval_decoded, Mode, SignalTimes};
use crate::vocab::Vocabulary;
use crate::Result;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HookConjunctFinding {
    pub hook: String,
    pub conjunct: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LintHooksReport {
    pub zhixu_name: String,
    pub states: u64,
    pub guards_examined: usize,
    pub subscription_guards_skipped: usize,
    pub conjuncts_examined: usize,
    pub findings: Vec<HookConjunctFinding>,
}

pub fn lint_hooks(
    definition_value: &serde_json::Value,
    max_states: u64,
) -> Result<LintHooksReport> {
    let manifest = Manifest::default();
    let vocab = Vocabulary::build(definition_value, &manifest)?;
    let mut explorer = Explorer::new(&vocab);
    let all_facts: Vec<usize> = (0..vocab.facts.len()).collect();
    let graph = explorer.graph(&all_facts, Mode::Full, max_states)?;

    struct Item {
        hook: String,
        conjunct: String,
        conjunct_hook: DecodedCompiledHook,
        rest_hook: DecodedCompiledHook,
        witnessed: bool,
    }

    let mut items = Vec::new();
    let mut skipped = 0usize;
    let mut conjuncts = 0usize;
    for guard in &vocab.guards {
        if guard.decoded.mode == HookMode::Subscription {
            skipped += 1;
            continue;
        }
        let terms: Vec<Expr> = flatten_conjuncts(&guard.decoded.expr)
            .into_iter()
            .cloned()
            .collect();
        if terms.len() < 2 {
            continue;
        }
        conjuncts += terms.len();
        for index in 0..terms.len() {
            let rest: Vec<Expr> = terms
                .iter()
                .enumerate()
                .filter(|(position, _)| *position != index)
                .map(|(_, term)| term.clone())
                .collect();
            let rest_expr = if rest.len() == 1 {
                rest[0].clone()
            } else {
                Expr::And(rest)
            };
            items.push(Item {
                hook: guard.key.clone(),
                conjunct: render_expr(&terms[index]),
                conjunct_hook: DecodedCompiledHook {
                    mode: guard.decoded.mode,
                    source: guard.decoded.source.clone(),
                    expr: terms[index].clone(),
                },
                rest_hook: DecodedCompiledHook {
                    mode: guard.decoded.mode,
                    source: guard.decoded.source.clone(),
                    expr: rest_expr,
                },
                witnessed: false,
            });
        }
    }

    for state in &graph.states {
        if items.iter().all(|item| item.witnessed) {
            break;
        }
        let times: SignalTimes = state.signal_times(&vocab);
        for item in items.iter_mut() {
            if item.witnessed {
                continue;
            }
            if eval_decoded(&item.rest_hook, &times, state.now) == Some(EvalState::Ready)
                && eval_decoded(&item.conjunct_hook, &times, state.now) != Some(EvalState::Ready)
            {
                item.witnessed = true;
            }
        }
    }

    Ok(LintHooksReport {
        zhixu_name: vocab.zhixu_name.clone(),
        states: graph.states.len() as u64,
        guards_examined: vocab.guards.len() - skipped,
        subscription_guards_skipped: skipped,
        conjuncts_examined: conjuncts,
        findings: items
            .into_iter()
            .filter(|item| !item.witnessed)
            .map(|item| HookConjunctFinding {
                hook: item.hook,
                conjunct: item.conjunct,
            })
            .collect(),
    })
}

fn flatten_conjuncts(expr: &Expr) -> Vec<&Expr> {
    fn walk<'a>(expr: &'a Expr, out: &mut Vec<&'a Expr>) {
        if let Expr::And(terms) = expr {
            for term in terms {
                walk(term, out);
            }
        } else {
            out.push(expr);
        }
    }
    let mut out = Vec::new();
    walk(expr, &mut out);
    out
}

fn render_expr(expr: &Expr) -> String {
    match expr {
        Expr::Signal(name) => name.clone(),
        Expr::Subscription { target, .. } => format!("@{target}"),
        Expr::Not(inner) => format!("~{}", render_expr(inner)),
        Expr::And(terms) => terms
            .iter()
            .map(|term| render_expr(term))
            .collect::<Vec<_>>()
            .join(" & "),
        Expr::Or(terms) => terms
            .iter()
            .map(|term| render_expr(term))
            .collect::<Vec<_>>()
            .join(" | "),
        Expr::Delay {
            expr,
            raw_duration,
            ..
        } => {
            let operand = render_expr(expr);
            if matches!(**expr, Expr::Signal(_)) {
                format!("{operand} + {raw_duration}")
            } else {
                format!("({operand}) + {raw_duration}")
            }
        }
    }
}
