//! 产物内省：编译秩序定义抽取模型检查词表——声明信号全集、意图（适格
//! 位求值器）、延时位点。守卫不进模型：守卫点火属于执行器与世界的因
//! 果，不是定义的形式后果。求值语义单源在 uvp-hook-dsl，本模块只做结
//! 构提取；检查的探索宇宙由原子的适格闭包（closure）机械派生。

use std::collections::{BTreeMap, BTreeSet};

use uvp_hook_dsl::{
    decode_compiled_hook, parse_hook, DecodedCompiledHook, Expr, Gate, HookMode, ParseHookRequest,
    Profile,
};
use uvp_model::ZhixuDefinition;

use crate::checks::Check;
use crate::{McError, Result};

#[derive(Debug, Clone)]
pub struct Intent {
    pub fact: String,
    pub decoded: DecodedCompiledHook,
}

#[derive(Debug, Clone)]
pub struct DelaySite {
    pub source: String,
    pub operand: Expr,
    pub duration_seconds: i64,
}

pub struct Vocabulary {
    pub zhixu_name: String,
    pub facts: Vec<String>,
    pub fact_index: BTreeMap<String, usize>,
    pub declared: BTreeSet<String>,
    pub intents: Vec<Intent>,
    pub intent_fact: BTreeMap<usize, usize>,
    pub delay_sites: Vec<DelaySite>,
    pub predicates: BTreeMap<String, DecodedCompiledHook>,
}

pub const MC_PROFILE: Profile = Profile::CloudCompat;

pub fn parse_predicate(raw: &str) -> std::result::Result<DecodedCompiledHook, String> {
    let parsed = parse_hook(ParseHookRequest {
        profile: MC_PROFILE,
        gate: Gate::Filter,
        hook_name: "MC_PREDICATE".to_string(),
        hook: raw.to_string(),
    })
    .map_err(|err| format!("predicate {raw:?} does not parse: {err}"))?;
    if parsed.mode == HookMode::Subscription {
        return Err(format!(
            "predicate {raw:?} must be a per-order state judgment and must not contain subscription atoms"
        ));
    }
    decode_compiled_hook(&parsed.cloud_ast, Gate::Filter)
        .map_err(|err| format!("predicate {raw:?} failed to decode: {err}"))
}

pub fn signal_atoms(expr: &Expr, source: &str) -> Vec<String> {
    let mut out = Vec::new();
    fn walk(expr: &Expr, source: &str, out: &mut Vec<String>) {
        match expr {
            Expr::Signal(signal) => out.push(format!("{source}::{signal}")),
            Expr::Not(inner) => walk(inner, source, out),
            Expr::Delay { expr, .. } => walk(expr, source, out),
            Expr::And(terms) | Expr::Or(terms) => {
                for term in terms {
                    walk(term, source, out);
                }
            }
            Expr::Subscription { .. } => {}
        }
    }
    walk(expr, source, &mut out);
    out
}

pub fn positive_signal_atoms(expr: &Expr, source: &str) -> Vec<String> {
    let mut out = Vec::new();
    fn walk(expr: &Expr, source: &str, out: &mut Vec<String>) {
        match expr {
            Expr::Signal(signal) => out.push(format!("{source}::{signal}")),
            Expr::Not(_) => {}
            Expr::Delay { expr, .. } => walk(expr, source, out),
            Expr::And(terms) | Expr::Or(terms) => {
                for term in terms {
                    walk(term, source, out);
                }
            }
            Expr::Subscription { .. } => {}
        }
    }
    walk(expr, source, &mut out);
    out
}

/// 合取语境正位原子（见证必经集用）：与 `positive_signal_atoms` 的差别只在
/// 不下降 Or——析取语境里没有任何单个原子是见证的必要条件（`(a|b)` 的见证
/// 可只经 b），把析取臂的原子当必经项剪枝会误剪仍能长出见证的子树。
pub fn conjunctive_positive_atoms(expr: &Expr, source: &str) -> Vec<String> {
    let mut out = Vec::new();
    fn walk(expr: &Expr, source: &str, out: &mut Vec<String>) {
        match expr {
            Expr::Signal(signal) => out.push(format!("{source}::{signal}")),
            Expr::Not(_) => {}
            Expr::Delay { expr, .. } => walk(expr, source, out),
            Expr::And(terms) => {
                for term in terms {
                    walk(term, source, out);
                }
            }
            Expr::Or(_) | Expr::Subscription { .. } => {}
        }
    }
    walk(expr, source, &mut out);
    out
}

/// 落地位阻断项的原子集：只认「单个原子恰处一层 Not 之下」的直接负门
/// （~x）。时间窗负位（~(x+d)）随时间翻转、不随落地关死；复合否定
/// （~(a&b)）需诸项齐落才为假——二者都不构成落地即永死的阻断项。
pub fn negative_signal_atoms(expr: &Expr, source: &str) -> Vec<String> {
    let mut out = Vec::new();
    fn walk(expr: &Expr, source: &str, negated: bool, out: &mut Vec<String>) {
        match expr {
            Expr::Signal(signal) => {
                if negated {
                    out.push(format!("{source}::{signal}"));
                }
            }
            Expr::Not(inner) => walk(inner, source, !negated, out),
            Expr::Delay { .. } => {}
            Expr::And(terms) | Expr::Or(terms) if !negated => {
                for term in terms {
                    walk(term, source, false, out);
                }
            }
            Expr::And(_) | Expr::Or(_) | Expr::Subscription { .. } => {}
        }
    }
    walk(expr, source, false, &mut out);
    out
}

impl Vocabulary {
    pub fn build(definition_value: &serde_json::Value, checks: &[Check]) -> Result<Self> {
        let definition: ZhixuDefinition = serde_json::from_value(definition_value.clone())
            .map_err(|err| McError::Message(format!("invalid Zhixu definition: {err}")))?;
        let cloud = uvp_compiler::compile_cloud_artifact(definition_value, None, true)?;
        let hook_plan = uvp_compiler::compile_zhixu_hook_plan(definition_value, None, true)?;

        let mut declared: BTreeSet<String> = BTreeSet::new();
        for capability in hook_plan
            .get("signalCapabilities")
            .and_then(|value| value.as_array())
            .into_iter()
            .flatten()
        {
            let source = capability
                .get("targetSource")
                .and_then(|value| value.as_str())
                .unwrap_or_default();
            let signal = capability
                .get("targetSignalName")
                .and_then(|value| value.as_str())
                .unwrap_or_default();
            if !source.is_empty() && !signal.is_empty() {
                declared.insert(format!("{source}::{signal}"));
            }
        }

        let mut fact_set: BTreeMap<String, ()> =
            declared.iter().map(|fact| (fact.clone(), ())).collect();

        let mut intents = Vec::new();
        let mut delay_sites = Vec::new();
        for admission in cloud
            .get("admissions")
            .and_then(|value| value.as_array())
            .into_iter()
            .flatten()
        {
            let signal = admission
                .get("signalName")
                .and_then(|value| value.as_str())
                .unwrap_or_default();
            let source = admission
                .get("cloudAst")
                .and_then(|value| value.get("source"))
                .and_then(|value| value.as_str())
                .unwrap_or_default();
            let ast = admission.get("cloudAst").ok_or_else(|| {
                McError::Message(format!("admission {signal} is missing cloudAst"))
            })?;
            let decoded = decode_compiled_hook(ast, Gate::Filter)
                .map_err(|err| McError::Message(format!("admission {signal}: {err}")))?;
            for atom in signal_atoms(&decoded.expr, &decoded.source) {
                fact_set.insert(atom, ());
            }
            collect_delay_sites(&decoded, &mut delay_sites);
            intents.push(Intent {
                fact: format!("{source}::{signal}"),
                decoded,
            });
        }

        let mut predicates = BTreeMap::new();
        for check in checks {
            for raw in [&check.when, &check.predicate] {
                let Some(raw) = raw else { continue };
                if predicates.contains_key(raw) {
                    continue;
                }
                let decoded = parse_predicate(raw).map_err(McError::Message)?;
                for atom in signal_atoms(&decoded.expr, &decoded.source) {
                    fact_set.insert(atom, ());
                }
                collect_delay_sites(&decoded, &mut delay_sites);
                predicates.insert(raw.clone(), decoded);
            }
        }

        let facts: Vec<String> = fact_set.into_keys().collect();
        let fact_index: BTreeMap<String, usize> = facts
            .iter()
            .enumerate()
            .map(|(index, fact)| (fact.clone(), index))
            .collect();

        let intent_fact: BTreeMap<usize, usize> = intents
            .iter()
            .enumerate()
            .map(|(index, intent)| {
                let fact = *fact_index
                    .get(&intent.fact)
                    .expect("intent fact was interned during collection");
                (fact, index)
            })
            .collect();

        Ok(Vocabulary {
            zhixu_name: definition.metadata.name,
            facts,
            fact_index,
            declared,
            intents,
            intent_fact,
            delay_sites,
            predicates,
        })
    }

    /// 适格闭包：种子原子，加上经由意图适格表达式可达的全部原子，传递
    /// 到底。闭包外原子不参与种子相关求值，故在此宇宙上穷举与在全宇宙
    /// 上穷举对相关性质等价。
    pub fn closure(&self, seeds: &[usize]) -> BTreeSet<usize> {
        let mut out: BTreeSet<usize> = seeds.iter().copied().collect();
        let mut queue: Vec<usize> = seeds.to_vec();
        while let Some(fact) = queue.pop() {
            let Some(&intent) = self.intent_fact.get(&fact) else {
                continue;
            };
            for atom in signal_atoms(
                &self.intents[intent].decoded.expr,
                &self.intents[intent].decoded.source,
            ) {
                if let Some(index) = self.fact_index.get(&atom) {
                    if out.insert(*index) {
                        queue.push(*index);
                    }
                }
            }
        }
        out
    }

    /// 正位闭包（存在性检查用）：只收适格表达式里不被 Not 压住的原子。
    /// 负位原子是阻断项——见证路径永不落地它们，排除出宇宙后求值只会
    /// 更宽松，违反若存在必仍可见，通过则全宇宙亦通过。
    pub fn positive_closure(&self, seeds: &[usize]) -> BTreeSet<usize> {
        let mut out: BTreeSet<usize> = seeds.iter().copied().collect();
        let mut queue: Vec<usize> = seeds.to_vec();
        while let Some(fact) = queue.pop() {
            let Some(&intent) = self.intent_fact.get(&fact) else {
                continue;
            };
            for atom in positive_signal_atoms(
                &self.intents[intent].decoded.expr,
                &self.intents[intent].decoded.source,
            ) {
                if let Some(index) = self.fact_index.get(&atom) {
                    if out.insert(*index) {
                        queue.push(*index);
                    }
                }
            }
        }
        out
    }

    pub fn intent_facts(&self) -> Vec<usize> {
        let mut out: Vec<usize> = self.intent_fact.keys().copied().collect();
        out.sort_unstable();
        out
    }

    /// 意图的落地位阻断项：适格表达式里以负位出现的原子。事实不撤回，
    /// 阻断项一经落地该意图永久不适格——剪枝与死意图判定的依据。
    pub fn intent_blockers(&self, fact: usize) -> Vec<usize> {
        let Some(&intent) = self.intent_fact.get(&fact) else {
            return Vec::new();
        };
        negative_signal_atoms(
            &self.intents[intent].decoded.expr,
            &self.intents[intent].decoded.source,
        )
        .iter()
        .filter_map(|atom| self.fact_index.get(atom).copied())
        .collect()
    }
}

fn collect_delay_sites(decoded: &DecodedCompiledHook, sites: &mut Vec<DelaySite>) {
    fn walk(expr: &Expr, source: &str, sites: &mut Vec<DelaySite>) {
        if let Expr::Delay {
            expr: operand,
            duration_seconds,
            ..
        } = expr
        {
            sites.push(DelaySite {
                source: source.to_string(),
                operand: (**operand).clone(),
                duration_seconds: *duration_seconds,
            });
            walk(operand, source, sites);
            return;
        }
        match expr {
            Expr::Not(inner) => walk(inner, source, sites),
            Expr::And(terms) | Expr::Or(terms) => {
                for term in terms {
                    walk(term, source, sites);
                }
            }
            Expr::Signal(_) | Expr::Subscription { .. } | Expr::Delay { .. } => {}
        }
    }
    walk(&decoded.expr, &decoded.source, sites);
}
