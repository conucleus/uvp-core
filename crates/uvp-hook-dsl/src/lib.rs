//! uvp-hook-dsl：hook DSL 的解析、规范化、依赖提取与求值。
//! crate 根只保留语义词汇（Profile/错误通道/版本常量）、JSON 信封入口
//! 与公共导出；生产逻辑见 `parser/`、`ast/`、`dependency/`、`evaluation/`
//! 与 `lint.rs`。

use serde::{Deserialize, Serialize};
use thiserror::Error;

mod ast;
mod dependency;
mod evaluation;
mod parser;

pub mod lint;

pub use lint::{
    contradicts, lint_hook, lint_hook_with_condition, ready_implies, same_expr,
    semantic_fingerprint, Category, HookLintResult, LintDiagnostic, LintError, LintProof,
    LintReport, ProofReason, ProofResult, RelatedSpan, Severity, Span, SpannedExpr,
    MAX_LINT_BOOLEAN_DEPTH, MAX_LINT_BOOLEAN_OPERANDS, MAX_LINT_NODES, MAX_PAIRWISE_HOOKS,
};

pub use ast::{Compatibility, Expr, HookExpr, HookMode, SubscriptionTarget};
pub use dependency::{Dependency, DependencyKind};
pub use evaluation::{
    eval_compiled_hook, EvalCompiledHookOutput, EvalCompiledHookRequest, EvalState, SignalFact,
};
pub use parser::{parse_hook, parse_hook_expr_with_spans, ParseHookOutput, ParseHookRequest};

#[cfg(test)]
mod tests;

#[cfg(test)]
// 模块内测试直引词法闸与 serde_json::Value（见 tests.rs）。
use parser::duration_to_seconds;
#[cfg(test)]
use serde_json::Value;

pub const CORE_VERSION: &str = env!("CARGO_PKG_VERSION");
pub const SEMANTIC_VERSION: &str = "uvp.semantic.v1";
pub const CLOUD_AST_SCHEMA_VERSION: &str = "uvp.cloudAst.v1";

/// 以下跨秩序关键字不受支持：`::OUTSIDE@`、`ANCHOR@`（裸标头）、
/// `OUTSOURCE`。解析器词法识别这些关键字，以便给出精确的 unsupported 报错
/// （统一入口为 `::ANCHOR(@source::task.stage.signal)` 订阅，见
/// subscription-mint-spec.md），而不是笼统的语法错误。扇入类标头不在
/// 词表内：该形态按通用语法错误拒绝。
pub const RETIRED_KEYWORDS_HINT: &str = "cross-source entries retired in uvp.semantic.v1; use ::ANCHOR(@source::task.stage.signal) as the unified subscription entry (see subscription-mint-spec.md)";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Profile {
    #[default]
    EvmStrict,
    CloudCompat,
}

/// 校验档（与 Profile 正交：profile 只影响归一化输出，gate 决定走哪套
/// 位置/锚点校验）。`hook`（默认）：钩子档——正锚要求 + 否决位仅合取
/// 直接子项；`filter`：过滤档（发射适格面）——无正锚要求、衰减否决位
/// 位置全放开（到达一拍求值，任何位置的瞬时值都良定义），保留 duration
/// 语法/上限、NOT 操作数词表与身份闸。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Gate {
    #[default]
    Hook,
    Filter,
}

#[derive(Debug, Error)]
pub enum HookError {
    #[error("{0}")]
    Message(String),
}

type Result<T> = std::result::Result<T, HookError>;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Envelope<T: Serialize> {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<T>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diagnostics: Option<Vec<Diagnostic>>,
}

#[derive(Debug, Serialize)]
pub struct Diagnostic {
    pub message: String,
}

pub fn parse_hook_json(input: &str) -> String {
    let result = serde_json::from_str::<ParseHookRequest>(input)
        .map_err(|err| HookError::Message(format!("invalid parse hook request: {err}")))
        .and_then(parse_hook);
    envelope_json(result)
}

pub fn eval_compiled_hook_json(input: &str) -> String {
    let result = serde_json::from_str::<EvalCompiledHookRequest>(input)
        .map_err(|err| HookError::Message(format!("invalid eval compiled hook request: {err}")))
        .and_then(eval_compiled_hook);
    envelope_json(result)
}

/// lint 的 JSON 入口（请求形态与 parse-hook 一致）。lint 不改变语言合法
/// 性：语义验证失败按 ok:false 信封返回，合法表达的 diagnostics 在
/// ok:true 的 value 里，是否阻塞由调用方 deny policy 决定。
pub fn lint_hook_json(input: &str) -> String {
    let result = serde_json::from_str::<ParseHookRequest>(input)
        .map_err(|err| HookError::Message(format!("invalid lint hook request: {err}")))
        .and_then(|req| {
            lint_hook(req.profile, req.gate, &req.hook_name, &req.hook)
                .map_err(|err| HookError::Message(err.to_string()))
        });
    envelope_json(result)
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
    serde_json::to_string(&envelope).expect("envelope serialization should not fail")
}
