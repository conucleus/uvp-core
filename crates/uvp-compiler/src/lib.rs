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
// 模块内测试直引能力表规模上限与 serde_json::Map（见 tests.rs）。
use artifact::MAX_SIGNAL_CAPABILITIES;
#[cfg(test)]
use serde_json::Map;

#[derive(Debug, Error)]
pub enum CompilerError {
    #[error("{0}")]
    Message(String),
    #[error("compilation failed: {0}")]
    Issues(String),
}

type Result<T> = std::result::Result<T, CompilerError>;

#[derive(Debug, Deserialize)]
// FFI/NAPI 最外层请求信封：未知字段确定性拒绝（拼错的调用方输入不得
// 被静默忽略成零值语义）。
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct CompileRequest {
    #[serde(default = "default_target")]
    pub target: String,
    pub definition: Value,
    /// Dock resolution manifest：由 Store/发布系统或离线
    /// lock 文件提供；含 zhixu executor 的可运行编译必须提供，否则返回
    /// `UNRESOLVED_DOCK_TARGET`。
    #[serde(default)]
    pub resolution_manifest: Option<Value>,
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
    let manifest = req.resolution_manifest.as_ref();
    match req.target.as_str() {
        "hook_plan" | "evm" => compile_zhixu_hook_plan(&req.definition, manifest, false),
        "cloud" | "cloud_db" => compile_cloud_artifact(&req.definition, manifest, false),
        // parse-only：允许 unresolved route。
        "parse" => compile_zhixu_hook_plan(&req.definition, manifest, true),
        // dock link 编译 target（uvp.dock-link v1 产物面）已删除
        // （无消费方，机制直接移除）：link 校验由
        // hook_plan/cloud/parse 在 resolutionManifest 在场时同一链路承担，
        // 无独立产物面。
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
