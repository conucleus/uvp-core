//! uvp-mc：秩序定义的穷举式模型检查器，只裁决定义自身的形式后果。转
//! 移语义：意图按适格落地、无适格事实自由落地、时间推进到截止——每一
//! 步经 uvp-hook-dsl 的真求值器裁决，模型即实现，无第二份语义源。不装
//! 载任何因果公理：谁导致谁、回执何时来，是执行器与世界的事，MC 不为
//! 之作证。检查项以数据喂入（CLI flag 或 JSON 请求），探索宇宙由检查
//! 原子的适格闭包机械派生；空检查集触发裸跑套件（死意图检查 + 意图共
//! 存矩阵）。

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

pub mod check;
pub mod checks;
pub mod explore;
pub mod lint;
pub mod lint_hooks;
pub mod model;
pub mod vocab;

#[cfg(test)]
mod tests;

pub use check::{CheckOutcome, Checker, Status};
pub use checks::{Check, CheckKind, DEFAULT_MAX_STATES};
pub use lint::{lint, LintFinding, LintReport};
pub use lint_hooks::{lint_hooks, HookConjunctFinding, LintHooksReport};
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
    #[serde(default)]
    pub checks: Vec<Check>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_states: Option<u64>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct IntentPair {
    pub a: String,
    pub b: String,
    pub coexist: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct McCheckReport {
    pub zhixu_name: String,
    pub passed: bool,
    pub lint: Value,
    pub checks: Vec<CheckOutcome>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub intent_pairs: Option<Vec<IntentPair>>,
}

pub fn mc_check(request: McCheckRequest) -> Result<McCheckReport> {
    let vocab = Vocabulary::build(&request.definition, &request.checks)?;
    let lint_report = lint::lint(&vocab, &request.checks)?;
    if !lint_report.ok() {
        return Ok(McCheckReport {
            zhixu_name: vocab.zhixu_name.clone(),
            passed: false,
            lint: lint_report.to_value(),
            checks: Vec::new(),
            intent_pairs: None,
        });
    }
    let max_states = request.max_states.unwrap_or(DEFAULT_MAX_STATES);
    let (checks, intent_pairs) = if request.checks.is_empty() {
        bare_run(&vocab, max_states)?
    } else {
        let mut checker = Checker::new(&vocab, max_states);
        let mut checks = Vec::with_capacity(request.checks.len());
        for check in &request.checks {
            checks.push(checker.run(check)?);
        }
        (checks, None)
    };
    let passed = checks.iter().all(|outcome| outcome.status == Status::Pass);
    Ok(McCheckReport {
        zhixu_name: vocab.zhixu_name.clone(),
        passed,
        lint: lint_report.to_value(),
        checks,
        intent_pairs,
    })
}

/// 裸跑套件：零检查项时对定义自身做形式体检——每个意图在闭包宇宙内至
/// 少存在一个适格时刻（死意图即红），意图两两共存矩阵供人眼扫意外组合。
fn bare_run(vocab: &Vocabulary, max_states: u64) -> Result<(Vec<CheckOutcome>, Option<Vec<IntentPair>>)> {
    let mut checker = Checker::new(vocab, max_states);
    let mut checks = Vec::new();
    for fact in vocab.intent_facts() {
        checks.push(checker.run_dead_intent(fact)?);
    }
    let pairs = intent_coexistence(vocab, max_states)?;
    Ok((checks, Some(pairs)))
}

fn intent_coexistence(vocab: &Vocabulary, max_states: u64) -> Result<Vec<IntentPair>> {
    let intents = vocab.intent_facts();
    let mut pairs = Vec::new();
    for (position, &a) in intents.iter().enumerate() {
        for &b in &intents[position + 1..] {
            let universe: Vec<usize> = vocab.positive_closure(&[a, b]).into_iter().collect();
            let graph = explore::explore_universe(
                vocab,
                &universe,
                max_states,
                Some(&|state| {
                    Ok((state.fact_epochs[a].is_some() && state.fact_epochs[b].is_some())
                        .then_some(explore::StopKind::Satisfied))
                }),
                None,
            )?;
            pairs.push(IntentPair {
                a: vocab.facts[a].clone(),
                b: vocab.facts[b].clone(),
                coexist: graph.stopped.is_some(),
            });
        }
    }
    Ok(pairs)
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
    #[serde(default)]
    pub checks: Vec<Check>,
}

pub fn mc_lint_json(input: &str) -> String {
    let result = serde_json::from_str::<McLintRequest>(input)
        .map_err(|err| McError::Message(format!("invalid mc lint request: {err}")))
        .and_then(|request| {
            let vocab = Vocabulary::build(&request.definition, &request.checks)?;
            Ok(lint::lint(&vocab, &request.checks)?.to_value())
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
