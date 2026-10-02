//! uvp-compiler：定义到目标产物（hook_plan / cloud artifact）的编译器。
//! crate 根只保留编译入口（compile_json/compile_request）、请求信封与
//! 公共导出；生产逻辑见 `validate/`、`lower/`、`docking/`、`artifact/`、
//! `dock.rs`（对接接口与路由链接）与 `lint.rs`。

use serde::Deserialize;
use serde_json::Value;
use thiserror::Error;

pub use artifact::{compile_cloud_artifact, compile_zhixu_hook_plan};

pub mod dock;
pub mod lint;

mod artifact;
mod docking;
mod lower;
mod validate;

pub use artifact::{CLOUD_ARTIFACT_SCHEMA_VERSION, HOOK_PLAN_SCHEMA_VERSION};

#[cfg(test)]
mod tests;

#[cfg(test)]
use serde_json::Map;

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
    #[serde(default)]
    pub dock_targets: Option<Value>,
}

fn default_target() -> String {
    "hook_plan".to_string()
}

pub fn compile_json(input: &str) -> String {
    let result = serde_json::from_str::<CompileRequest>(input)
        .map_err(|err| CompilerError::Message(format!("invalid compile request: {err}")))
        .and_then(|req| compile_request(&req));
    envelope_json(result)
}

pub fn compile_request(req: &CompileRequest) -> Result<Value> {
    let dock_targets = req.dock_targets.as_ref();
    match req.target.as_str() {
        "hook_plan" | "evm" => compile_zhixu_hook_plan(&req.definition, dock_targets, false),
        "cloud" | "cloud_db" => compile_cloud_artifact(&req.definition, dock_targets, false),
        "parse" => compile_zhixu_hook_plan(&req.definition, dock_targets, true),
        other => Err(CompilerError::Message(format!(
            "unsupported compile target {other:?}"
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
    serde_json::to_string(&envelope).expect("compile envelope should serialize")
}
