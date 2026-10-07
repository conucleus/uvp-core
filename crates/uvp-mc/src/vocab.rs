//! 产物内省：从秩序定义编译出 cloud artifact 与 hook plan，抽取模型检查
//! 所需的词表——事实全集、守卫钩子（hook 位求值器）、意图（适格位求值
//! 器）、延时位点（时间纪元边界）。求值语义单源在 uvp-hook-dsl，本模块
//! 只做结构提取，不做第二份语义。

use std::collections::BTreeMap;

use uvp_hook_dsl::{
    decode_compiled_hook, parse_hook, DecodedCompiledHook, Expr, Gate, HookMode, ParseHookRequest,
    Profile,
};
use uvp_model::ZhixuDefinition;

use crate::manifest::Manifest;
use crate::{McError, Result};

#[derive(Debug, Clone)]
pub struct Guard {
    pub key: String,
    pub decoded: DecodedCompiledHook,
}

#[derive(Debug, Clone)]
pub struct Intent {
    pub fact: String,
    pub decoded: DecodedCompiledHook,
}

#[derive(Debug, Clone, Default)]
pub struct ResolvedEffect {
    pub any_guards: Vec<usize>,
    pub all_facts: Vec<usize>,
    pub any_facts: Vec<usize>,
    pub unless_facts: Vec<usize>,
    pub env_timed: bool,
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
    pub guards: Vec<Guard>,
    pub guard_index: BTreeMap<String, usize>,
    pub intents: Vec<Intent>,
    pub intent_fact: BTreeMap<usize, usize>,
    pub effects: BTreeMap<usize, ResolvedEffect>,
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

impl Vocabulary {
    pub fn build(definition_value: &serde_json::Value, manifest: &Manifest) -> Result<Self> {
        let definition: ZhixuDefinition = serde_json::from_value(definition_value.clone())
            .map_err(|err| McError::Message(format!("invalid Zhixu definition: {err}")))?;
        let cloud = uvp_compiler::compile_cloud_artifact(definition_value, None, true)?;
        let hook_plan = uvp_compiler::compile_zhixu_hook_plan(definition_value, None, true)?;

        let mut fact_set: BTreeMap<String, ()> = BTreeMap::new();
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
                fact_set.insert(format!("{source}::{signal}"), ());
            }
        }

        let mut guards = Vec::new();
        let mut guard_index = BTreeMap::new();
        let mut delay_sites = Vec::new();
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
            let ast = hook.get("astJson").ok_or_else(|| {
                McError::Message(format!("hook {stage}#{hook_name} is missing astJson"))
            })?;
            let decoded = decode_compiled_hook(ast, Gate::Hook)
                .map_err(|err| McError::Message(format!("hook {stage}#{hook_name}: {err}")))?;
            for atom in signal_atoms(&decoded.expr, &decoded.source) {
                fact_set.insert(atom, ());
            }
            collect_delay_sites(&decoded, &mut delay_sites);
            let key = format!("{stage}#{hook_name}");
            guard_index.insert(key.clone(), guards.len());
            guards.push(Guard { key, decoded });
        }

        let mut intents = Vec::new();
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
        for check in &manifest.checks {
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

        let mut effects = BTreeMap::new();
        for effect in &manifest.effects {
            let fact_id = *fact_index.get(&effect.fact).ok_or_else(|| {
                McError::Message(format!(
                    "effect fact {:?} is not in the vocabulary",
                    effect.fact
                ))
            })?;
            let mut resolved = ResolvedEffect::default();
            for guard_key in &effect.requires_any_guards {
                let guard = *guard_index.get(guard_key).ok_or_else(|| {
                    McError::Message(format!(
                        "effect on {:?} references unknown guard {guard_key:?}",
                        effect.fact
                    ))
                })?;
                resolved.any_guards.push(guard);
            }
            for fact in &effect.requires_all_facts {
                resolved.all_facts.push(need_fact(&fact_index, fact)?);
            }
            for fact in &effect.requires_any_facts {
                resolved.any_facts.push(need_fact(&fact_index, fact)?);
            }
            for fact in &effect.unless_any_facts {
                resolved.unless_facts.push(need_fact(&fact_index, fact)?);
            }
            resolved.env_timed = effect.env_timed;
            effects.insert(fact_id, resolved);
        }

        Ok(Vocabulary {
            zhixu_name: definition.metadata.name,
            facts,
            fact_index,
            guards,
            guard_index,
            intents,
            intent_fact,
            effects,
            delay_sites,
            predicates,
        })
    }
}

fn need_fact(fact_index: &BTreeMap<String, usize>, fact: &str) -> Result<usize> {
    fact_index
        .get(fact)
        .copied()
        .ok_or_else(|| McError::Message(format!("fact {fact:?} is not in the vocabulary")))
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
