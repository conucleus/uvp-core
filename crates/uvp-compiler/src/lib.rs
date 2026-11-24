//! uvp-compiler：定义到中性分析（`analyze`）与云侧产物（`cloud artifact`）的
//! 编译器。链轨 hook_plan lowering 归链轨 TS 编译器，本 crate 不产出该产物。
//! crate 根只保留编译入口（compile_json/compile_request）、
//! supplier 尺寸校验入口、请求信封与公共导出；生产逻辑见 `validate/`、
//! `lower/`、`artifact/` 与 `lint.rs`。

use serde::Deserialize;
use serde_json::Value;
use thiserror::Error;

pub use analysis::compile_zhixu_analysis;
pub use artifact::compile_cloud_artifact;

pub mod lint;

mod analysis;
mod artifact;
mod lower;
mod validate;

pub use artifact::CLOUD_ARTIFACT_SCHEMA_VERSION;

#[cfg(test)]
mod tests;

#[derive(Debug, Error)]
pub enum CompilerError {
    #[error("{0}")]
    Message(String),
    #[error("compilation failed: {0}")]
    Issues(String),
}

pub(crate) const MAX_ISSUES_STRING_BYTES: usize = 16 * 1024;

pub(crate) fn join_issues_bounded(issues: &[String]) -> String {
    let mut out = String::new();
    let mut remaining = issues.len();
    for issue in issues {
        remaining -= 1;
        let piece = if out.is_empty() {
            issue.clone()
        } else {
            format!("; {issue}")
        };
        if out.len() + piece.len() > MAX_ISSUES_STRING_BYTES {
            out.push_str(&format!(
                "; …({} issues truncated: error string capped at {MAX_ISSUES_STRING_BYTES} bytes)",
                remaining + 1
            ));
            return out;
        }
        out.push_str(&piece);
    }
    out
}

type Result<T> = std::result::Result<T, CompilerError>;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct CompileRequest {
    #[serde(default = "default_target")]
    pub target: String,
    pub definition: Value,
}

fn default_target() -> String {
    "analyze".to_string()
}

pub fn compile_json(input: &str) -> String {
    let result = serde_json::from_str::<CompileRequest>(input)
        .map_err(|err| CompilerError::Message(format!("invalid compile request: {err}")))
        .and_then(|req| compile_request(&req));
    envelope_json(result)
}

pub fn compile_request(req: &CompileRequest) -> Result<Value> {
    match req.target.as_str() {
        "cloud" | "cloud_db" => compile_cloud_artifact(&req.definition),
        "analyze" => compile_zhixu_analysis(&req.definition),
        other => Err(CompilerError::Message(format!(
            "unsupported compile target {other:?} (expected \"analyze\", \"cloud\" or \"cloud_db\"; \
             the chain-track hook_plan lowering lives in the TS compiler)"
        ))),
    }
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct Envelope {
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    value: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    diagnostics: Option<Vec<Diagnostic>>,
}

#[derive(Debug, serde::Serialize)]
struct Diagnostic {
    message: String,
}

fn envelope_json(result: Result<Value>) -> String {
    let envelope = match result {
        Ok(value) => Envelope {
            ok: true,
            value: Some(value),
            diagnostics: None,
        },
        Err(err) => Envelope {
            ok: false,
            value: None,
            diagnostics: Some(vec![Diagnostic {
                message: err.to_string(),
            }]),
        },
    };
    serde_json::to_string(&envelope).unwrap_or_else(|err| {
        serde_json::json!({
            "ok": false,
            "infrastructure": true,
            "diagnostics": [
                { "message": format!("uvp-core response envelope serialization failed: {err}") }
            ]
        })
        .to_string()
    })
}
