//! 静态 lint：manifest 与编译产物的一致性——效果门控引用的事实/守卫真实
//! 存在、检查谓词可解析且落在词表内、意图确有适格产物、检查 id 不重复、
//! 词表死面报告。手改 JSON/manifest 破坏自洽在此拦截。

use serde_json::{json, Value};

use crate::manifest::{Manifest, MANIFEST_SCHEMA_VERSION};
use crate::vocab::{signal_atoms, Vocabulary};
use crate::Result;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
    Info,
}

impl Severity {
    fn as_str(&self) -> &'static str {
        match self {
            Severity::Error => "error",
            Severity::Warning => "warning",
            Severity::Info => "info",
        }
    }
}

pub struct LintFinding {
    pub severity: Severity,
    pub message: String,
}

pub struct LintReport {
    pub findings: Vec<LintFinding>,
}

impl LintReport {
    pub fn ok(&self) -> bool {
        !self
            .findings
            .iter()
            .any(|finding| finding.severity == Severity::Error)
    }

    pub fn to_value(&self) -> Value {
        json!({
            "ok": self.ok(),
            "findings": self
                .findings
                .iter()
                .map(|finding| {
                    json!({
                        "severity": finding.severity.as_str(),
                        "message": finding.message,
                    })
                })
                .collect::<Vec<_>>(),
        })
    }
}

pub fn lint(vocab: &Vocabulary, manifest: &Manifest) -> Result<LintReport> {
    let mut findings = Vec::new();

    if manifest.schema_version != MANIFEST_SCHEMA_VERSION {
        findings.push(LintFinding {
            severity: Severity::Error,
            message: format!(
                "manifest schema_version {:?} is not {MANIFEST_SCHEMA_VERSION}",
                manifest.schema_version
            ),
        });
    }

    let mut referenced: std::collections::BTreeSet<usize> = std::collections::BTreeSet::new();

    for effect in &manifest.effects {
        if effect.requires_any_guards.is_empty()
            && effect.requires_all_facts.is_empty()
            && effect.requires_any_facts.is_empty()
            && effect.unless_any_facts.is_empty()
        {
            findings.push(LintFinding {
                severity: Severity::Warning,
                message: format!(
                    "effect on {:?} has no gate at all; drop the entry to declare a free fact",
                    effect.fact
                ),
            });
        }
        if let Some(fact) = vocab.fact_index.get(&effect.fact) {
            referenced.insert(*fact);
        }
    }

    let mut ids = std::collections::BTreeMap::new();
    for check in &manifest.checks {
        if ids.insert(check.id.clone(), ()).is_some() {
            findings.push(LintFinding {
                severity: Severity::Error,
                message: format!("duplicate check id {:?}", check.id),
            });
        }
        for raw in [&check.when, &check.predicate] {
            let Some(raw) = raw else { continue };
            match vocab.predicates.get(raw) {
                Some(decoded) => {
                    for atom in signal_atoms(&decoded.expr, &decoded.source) {
                        if let Some(fact) = vocab.fact_index.get(&atom) {
                            referenced.insert(*fact);
                        } else {
                            findings.push(LintFinding {
                                severity: Severity::Error,
                                message: format!(
                                    "predicate in check {:?} references atom {atom:?} outside the vocabulary",
                                    check.id
                                ),
                            });
                        }
                    }
                }
                None => findings.push(LintFinding {
                    severity: Severity::Error,
                    message: format!(
                        "predicate in check {:?} was not parsed during vocabulary build",
                        check.id
                    ),
                }),
            }
        }
        for intent in &check.intents {
            match vocab.fact_index.get(intent) {
                Some(fact) => {
                    referenced.insert(*fact);
                    if !vocab.intent_fact.contains_key(fact) {
                        findings.push(LintFinding {
                            severity: Severity::Error,
                            message: format!(
                                "check {:?} lists {intent:?} as an intent, but it declares no admission",
                                check.id
                            ),
                        });
                    }
                }
                None => findings.push(LintFinding {
                    severity: Severity::Error,
                    message: format!(
                        "check {:?} references intent {intent:?} outside the vocabulary",
                        check.id
                    ),
                }),
            }
        }
        for fact in &check.scope {
            match vocab.fact_index.get(fact) {
                Some(index) => {
                    referenced.insert(*index);
                }
                None => findings.push(LintFinding {
                    severity: Severity::Error,
                    message: format!(
                        "check {:?} scopes over fact {fact:?} outside the vocabulary",
                        check.id
                    ),
                }),
            }
        }
        if check.scope.len() == 1 {
            findings.push(LintFinding {
                severity: Severity::Warning,
                message: format!(
                    "check {:?} scopes over a single fact; a one-fact universe cannot exercise guards",
                    check.id
                ),
            });
        }
    }

    for guard in &vocab.guards {
        for atom in signal_atoms(&guard.decoded.expr, &guard.decoded.source) {
            if let Some(fact) = vocab.fact_index.get(&atom) {
                referenced.insert(*fact);
            }
        }
    }
    for intent in &vocab.intents {
        for atom in signal_atoms(&intent.decoded.expr, &intent.decoded.source) {
            if let Some(fact) = vocab.fact_index.get(&atom) {
                referenced.insert(*fact);
            }
        }
        if let Some(fact) = vocab.fact_index.get(&intent.fact) {
            referenced.insert(*fact);
        }
    }

    for (index, fact) in vocab.facts.iter().enumerate() {
        if !referenced.contains(&index) {
            findings.push(LintFinding {
                severity: Severity::Info,
                message: format!("fact {fact:?} is declared but never referenced by any guard, admission, effect or check"),
            });
        }
    }

    Ok(LintReport { findings })
}
