//! 静态 lint：请求与编译产物的一致性——检查 id 不重复、意图确有适格产
//! 物、谓词原子必须是定义声明过的信号（拼错即拒：未声明的原子会成为宇
//! 宙里的幽灵自由事实，让检查空转或误红）。

use serde_json::{json, Value};

use crate::checks::Check;
use crate::vocab::{signal_atoms, Vocabulary};
use crate::Result;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error,
}

impl Severity {
    fn as_str(&self) -> &'static str {
        match self {
            Severity::Error => "error",
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
        self.findings.is_empty()
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

pub fn lint(vocab: &Vocabulary, checks: &[Check]) -> Result<LintReport> {
    let mut findings = Vec::new();

    let mut ids = std::collections::BTreeMap::new();
    for check in checks {
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
                        if !vocab.declared.contains(&atom) {
                            findings.push(LintFinding {
                                severity: Severity::Error,
                                message: format!(
                                    "predicate in check {:?} references atom {atom:?} which the definition does not declare",
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
                Some(&fact) => {
                    if !vocab.intent_fact.contains_key(&fact) {
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
    }

    Ok(LintReport { findings })
}
