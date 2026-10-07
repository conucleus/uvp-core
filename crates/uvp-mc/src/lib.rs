//! uvp-mc：秩序定义的穷举式模型检查器。转移系统的每一步（意图落地、守卫
//! 触发、时间推进）都经 uvp-hook-dsl 的真求值器裁决——模型即实现，无双
//! 语义源；不变量与效果门控以 manifest 数据（uvp.mc.manifest.v1）喂入，
//! 内核业务盲。业务侧 manifest 建模见 backend mcguard 与 uvp-deploy
//! services/init。

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

pub mod check;
pub mod explore;
pub mod lint;
pub mod manifest;
pub mod model;
pub mod vocab;

#[cfg(test)]
mod tests;

pub use check::{CheckOutcome, Checker, Status};
pub use lint::{lint, LintFinding, LintReport, Severity};
pub use manifest::{
    Check, CheckKind, Effect, Manifest, DEFAULT_MAX_STATES, MANIFEST_SCHEMA_VERSION,
};
pub use vocab::Vocabulary;

#[derive(Debug, Error)]
pub enum McError {
    #[error("{0}")]
    Message(String),
}

impl From<uvp_compiler::CompilerError> for McError {
    fn from(err: uvp_compiler::CompilerError) -> Self {
        McError::Message(err.to_string())
    }
}

pub type Result<T> = std::result::Result<T, McError>;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub struct McCheckRequest {
    pub definition: Value,
    pub manifest: Manifest,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct McCheckReport {
    pub zhixu_name: String,
    pub passed: bool,
    pub lint: Value,
    pub checks: Vec<CheckOutcome>,
}

pub fn mc_check(request: McCheckRequest) -> Result<McCheckReport> {
    let vocab = Vocabulary::build(&request.definition, &request.manifest)?;
    let lint_report = lint::lint(&vocab, &request.manifest)?;
    if !lint_report.ok() {
        return Ok(McCheckReport {
            zhixu_name: vocab.zhixu_name.clone(),
            passed: false,
            lint: lint_report.to_value(),
            checks: Vec::new(),
        });
    }
    let mut checker = Checker::new(&vocab, request.manifest.max_states());
    let mut checks = Vec::with_capacity(request.manifest.checks.len());
    for check in &request.manifest.checks {
        checks.push(checker.run(check)?);
    }
    let passed = checks
        .iter()
        .all(|outcome| outcome.status == Status::Pass);
    Ok(McCheckReport {
        zhixu_name: vocab.zhixu_name.clone(),
        passed,
        lint: lint_report.to_value(),
        checks,
    })
}

pub fn mc_check_json(input: &str) -> String {
    let result = serde_json::from_str::<McCheckRequest>(input)
        .map_err(|err| McError::Message(format!("invalid mc check request: {err}")))
        .and_then(mc_check);
    envelope_json(result)
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub struct McLintRequest {
    pub definition: Value,
    pub manifest: Manifest,
}

pub fn mc_lint_json(input: &str) -> String {
    let result = serde_json::from_str::<McLintRequest>(input)
        .map_err(|err| McError::Message(format!("invalid mc lint request: {err}")))
        .and_then(|request| {
            let vocab = Vocabulary::build(&request.definition, &request.manifest)?;
            Ok(lint::lint(&vocab, &request.manifest)?.to_value())
        });
    envelope_json(result)
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct Envelope<T: Serialize> {
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    value: Option<T>,
    #[serde(skip_serializing_if = "Option::is_none")]
    diagnostics: Option<Vec<Diagnostic>>,
}

#[derive(Debug, Serialize)]
struct Diagnostic {
    message: String,
}

fn envelope_json<T: Serialize>(result: Result<T>) -> String {
    let envelope = match result {
        Ok(value) => Envelope {
            ok: true,
            value: Some(value),
            diagnostics: None,
        },
        Err(err) => Envelope::<T> {
            ok: false,
            value: None,
            diagnostics: Some(vec![Diagnostic {
                message: err.to_string(),
            }]),
        },
    };
    serde_json::to_string(&envelope).expect("mc envelope should serialize")
}
