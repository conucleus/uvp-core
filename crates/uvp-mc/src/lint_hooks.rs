//! lint-hooks：守卫根层合取项的冗余检测（advisory——只建议可删项，不拒
//! 绝定义、不挂任何门）。判定式：合取项可删 ⟺ 不存在「其余合取项就绪而
//! 该项不就绪」的共存落地场景。分析只吃定义本身，沿适格图（sendSignals
//! 的 validWhen，引擎在发射事务内强制）做带回溯的 DFS：所需事实能否按某
//! 一次序全部落地、每次落位时自身适格项成立——对跨事实时序（回执先后、
//! 出生同捆、窗口年龄）零耦合假设，时间项按无条件满足处理，可达集是真
//! 实集合的过近似，故只可能漏报不可能误报；枚举或搜索预算耗尽按证人存
//! 在处理（不产出建议），绝不因搜不完而建议删除。

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;
use serde_json::Value;
use uvp_hook_dsl::{decode_compiled_hook, DecodedCompiledHook, Expr, Gate, HookMode};

use crate::vocab::{signal_atoms, Vocabulary};
use crate::Result;

const BRANCH_CAP: usize = 512;
const DFS_NODE_BUDGET: u32 = 200_000;

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
    pub guards_examined: usize,
    pub subscription_guards_skipped: usize,
    pub conjuncts_examined: usize,
    pub search_budget_exhausted: bool,
    pub findings: Vec<HookConjunctFinding>,
}

/// 一次满足分支：positives 须全部落地，negatives 须全程不落地。
#[derive(Default, Clone, Debug)]
struct Branch {
    positives: Vec<usize>,
    negatives: Vec<usize>,
}

/// 组合式收集满足分支：And 合并（笛卡尔），Or 并联，正负冲突的分支丢弃；
/// 超容量置 exhausted（调用方按保守方向处理）。
fn branches_of_expr(
    expr: &Expr,
    source: &str,
    resolve: &BTreeMap<String, usize>,
) -> (Vec<Branch>, bool) {
    let mut exhausted = false;
    let branches = collect(expr, source, resolve, &mut exhausted);
    let branches = dedupe(branches);
    (branches, exhausted)
}

fn collect(
    expr: &Expr,
    source: &str,
    resolve: &BTreeMap<String, usize>,
    exhausted: &mut bool,
) -> Vec<Branch> {
    match expr {
        Expr::Signal(name) => match resolve.get(&format!("{source}::{name}")) {
            Some(&fact) => vec![Branch {
                positives: vec![fact],
                negatives: Vec::new(),
            }],
            None => vec![Branch::default()],
        },
        Expr::Not(inner) => match inner.as_ref() {
            Expr::Signal(name) => match resolve.get(&format!("{source}::{name}")) {
                Some(&fact) => vec![Branch {
                    positives: Vec::new(),
                    negatives: vec![fact],
                }],
                None => vec![Branch::default()],
            },
            _ => vec![Branch::default()],
        },
        Expr::Delay { expr, .. } => collect(expr, source, resolve, exhausted),
        Expr::And(terms) => {
            let mut out = vec![Branch::default()];
            for term in terms {
                let options = collect(term, source, resolve, exhausted);
                let mut next = Vec::with_capacity(out.len() * options.len());
                for base in &out {
                    for option in &options {
                        let mut merged = base.clone();
                        let mut contradictory = false;
                        for fact in &option.positives {
                            if merged.negatives.contains(fact) {
                                contradictory = true;
                                break;
                            }
                            if !merged.positives.contains(fact) {
                                merged.positives.push(*fact);
                            }
                        }
                        if !contradictory {
                            for fact in &option.negatives {
                                if merged.positives.contains(fact) {
                                    contradictory = true;
                                    break;
                                }
                                if !merged.negatives.contains(fact) {
                                    merged.negatives.push(*fact);
                                }
                            }
                        }
                        if !contradictory {
                            next.push(merged);
                        }
                    }
                    if next.len() > BRANCH_CAP {
                        *exhausted = true;
                        break;
                    }
                }
                out = next;
                if *exhausted {
                    return out;
                }
            }
            out
        }
        Expr::Or(terms) => {
            let mut out = Vec::new();
            for term in terms {
                out.extend(collect(term, source, resolve, exhausted));
                if out.len() > BRANCH_CAP {
                    *exhausted = true;
                    return out;
                }
            }
            out
        }
        Expr::Subscription { .. } => Vec::new(),
    }
}

fn dedupe(branches: Vec<Branch>) -> Vec<Branch> {
    let mut seen: BTreeSet<(Vec<usize>, Vec<usize>)> = BTreeSet::new();
    branches
        .into_iter()
        .filter(|branch| {
            let mut key = (branch.positives.clone(), branch.negatives.clone());
            key.0.sort_unstable();
            key.1.sort_unstable();
            seen.insert(key)
        })
        .collect()
}

struct Lander<'a> {
    admissions: &'a BTreeMap<usize, Vec<Branch>>,
    landed: Vec<bool>,
    chain: Vec<usize>,
    nodes: u32,
    exhausted: bool,
}

impl<'a> Lander<'a> {
    fn land_all(&mut self, need: &[usize], avoid: &BTreeSet<usize>) -> bool {
        let snapshot = self.landed.clone();
        let ok = self.land_all_inner(need, avoid);
        if !ok {
            self.landed = snapshot;
        }
        ok
    }

    fn land_all_inner(&mut self, need: &[usize], avoid: &BTreeSet<usize>) -> bool {
        self.nodes += 1;
        if self.nodes > DFS_NODE_BUDGET {
            self.exhausted = true;
            return true;
        }
        if self.exhausted {
            return true;
        }
        let unlanded: Vec<usize> = need
            .iter()
            .copied()
            .filter(|fact| !self.landed[*fact])
            .collect();
        if unlanded.is_empty() {
            return true;
        }
        let snapshot = self.landed.clone();
        for &fact in &unlanded {
            if self.land_fact(fact, avoid) && self.land_all_inner(&unlanded, avoid) {
                return true;
            }
            self.landed = snapshot.clone();
            if self.exhausted {
                return true;
            }
        }
        false
    }

    fn land_fact(&mut self, fact: usize, avoid: &BTreeSet<usize>) -> bool {
        if self.landed[fact] {
            return true;
        }
        if avoid.contains(&fact) || self.chain.contains(&fact) {
            return false;
        }
        let snapshot = self.landed.clone();
        let unconditional = [Branch::default()];
        let admissions: &[Branch] = self.admissions.get(&fact).map_or(&unconditional, |v| v.as_slice());
        for admission in admissions {
            self.chain.push(fact);
            let ok = self.land_all_inner(&admission.positives, avoid)
                && admission
                    .negatives
                    .iter()
                    .all(|blocked| !self.landed[*blocked]);
            self.chain.pop();
            if ok {
                self.landed[fact] = true;
                return true;
            }
            self.landed = snapshot.clone();
            if self.exhausted {
                return false;
            }
        }
        false
    }
}

struct Guard {
    key: String,
    decoded: DecodedCompiledHook,
}

pub fn lint_hooks(definition_value: &Value) -> Result<LintHooksReport> {
    let vocab = Vocabulary::build(definition_value, &[])?;
    let cloud = uvp_compiler::compile_cloud_artifact(definition_value, None, true)?;

    // 守卫不进检查词表；本面自建解析域：声明与适格原子之外，并入守卫
    // 原子，否则守卫里的信号在 resolve 中缺席会静默降级分析。
    let mut guards = Vec::new();
    let mut atoms: BTreeMap<String, ()> = vocab.facts.iter().map(|fact| (fact.clone(), ())).collect();
    for hook in cloud
        .get("hooks")
        .and_then(|value| value.as_array())
        .into_iter()
        .flatten()
    {
        let stage = hook
            .get("stageIdentifier")
            .and_then(|value| value.as_str())
            .unwrap_or_default();
        let hook_name = hook
            .get("hookName")
            .and_then(|value| value.as_str())
            .unwrap_or_default();
        let Some(ast) = hook.get("astJson") else {
            continue;
        };
        let Ok(decoded) = decode_compiled_hook(ast, Gate::Hook) else {
            continue;
        };
        for atom in signal_atoms(&decoded.expr, &decoded.source) {
            atoms.insert(atom, ());
        }
        guards.push(Guard {
            key: format!("{stage}#{hook_name}"),
            decoded,
        });
    }

    let facts: Vec<String> = atoms.into_keys().collect();
    let resolve: BTreeMap<String, usize> = facts
        .iter()
        .enumerate()
        .map(|(index, fact)| (fact.clone(), index))
        .collect();
    let mut admissions: BTreeMap<usize, Vec<Branch>> = BTreeMap::new();
    for intent in &vocab.intents {
        let Some(&fact) = resolve.get(&intent.fact) else {
            continue;
        };
        let (branches, exhausted) =
            branches_of_expr(&intent.decoded.expr, &intent.decoded.source, &resolve);
        if exhausted {
            admissions.insert(fact, vec![Branch::default()]);
        } else {
            admissions.insert(fact, branches);
        }
    }

    let mut findings = Vec::new();
    let mut skipped = 0usize;
    let mut conjuncts = 0usize;
    let mut budget_exhausted = false;
    for guard in &guards {
        if guard.decoded.mode == HookMode::Subscription {
            skipped += 1;
            continue;
        }
        let terms: Vec<&Expr> = flatten_conjuncts(&guard.decoded.expr);
        if terms.len() < 2 {
            continue;
        }
        conjuncts += terms.len();
        for index in 0..terms.len() {
            let rest: Vec<Expr> = terms
                .iter()
                .enumerate()
                .filter(|(position, _)| *position != index)
                .map(|(_, term)| (*term).clone())
                .collect();
            let rest_expr = if rest.len() == 1 {
                rest[0].clone()
            } else {
                Expr::And(rest)
            };
            match conjunct_verdict(
                &rest_expr,
                terms[index],
                &guard.decoded.source,
                &resolve,
                &admissions,
                facts.len(),
            ) {
                Verdict::Removable => findings.push(HookConjunctFinding {
                    hook: guard.key.clone(),
                    conjunct: render_expr(terms[index]),
                }),
                Verdict::LoadBearing => {}
                Verdict::Unknown => budget_exhausted = true,
            }
        }
    }

    Ok(LintHooksReport {
        zhixu_name: vocab.zhixu_name.clone(),
        guards_examined: guards.len() - skipped,
        subscription_guards_skipped: skipped,
        conjuncts_examined: conjuncts,
        search_budget_exhausted: budget_exhausted,
        findings,
    })
}

enum Verdict {
    Removable,
    LoadBearing,
    Unknown,
}

fn conjunct_verdict(
    rest: &Expr,
    conjunct: &Expr,
    source: &str,
    resolve: &BTreeMap<String, usize>,
    admissions: &BTreeMap<usize, Vec<Branch>>,
    fact_count: usize,
) -> Verdict {
    let conjunct_fact = match conjunct {
        Expr::Signal(name) => resolve.get(&format!("{source}::{name}")).copied(),
        Expr::Not(inner) => match inner.as_ref() {
            Expr::Signal(name) => resolve.get(&format!("{source}::{name}")).copied(),
            _ => None,
        },
        Expr::Delay { expr, .. } => match expr.as_ref() {
            Expr::Signal(name) => resolve.get(&format!("{source}::{name}")).copied(),
            _ => None,
        },
        _ => None,
    };
    let (branches, exhausted) = branches_of_expr(rest, source, resolve);
    if exhausted {
        return Verdict::Unknown;
    }
    if branches.is_empty() {
        return Verdict::Removable;
    }
    let mut lander = Lander {
        admissions,
        landed: vec![false; fact_count],
        chain: Vec::new(),
        nodes: 0,
        exhausted: false,
    };
    let mut witnessed = false;
    for branch in &branches {
        let mut avoid: BTreeSet<usize> = branch.negatives.iter().copied().collect();
        let mut need = branch.positives.clone();
        match conjunct {
            Expr::Not(_) => {
                let Some(fact) = conjunct_fact else {
                    witnessed = true;
                    break;
                };
                if avoid.contains(&fact) {
                    continue;
                }
                if !need.contains(&fact) {
                    need.push(fact);
                }
            }
            Expr::Delay { .. } | Expr::Signal(_) => {
                let Some(fact) = conjunct_fact else {
                    witnessed = true;
                    break;
                };
                if need.contains(&fact) {
                    continue;
                }
                avoid.insert(fact);
            }
            _ => {
                witnessed = true;
                break;
            }
        }
        if lander.land_all(&need, &avoid) {
            witnessed = true;
            break;
        }
        if lander.exhausted {
            return Verdict::Unknown;
        }
    }
    if lander.exhausted {
        return Verdict::Unknown;
    }
    if witnessed {
        Verdict::LoadBearing
    } else {
        Verdict::Removable
    }
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
            .map(render_expr)
            .collect::<Vec<_>>()
            .join(" & "),
        Expr::Or(terms) => terms
            .iter()
            .map(render_expr)
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
