//! UVP Core Lint v1 的 Layer 2：同 Stage Hook 关系 lint（PRD 109 §12–§14）。
//!
//! 放在 `uvp-compiler` 层的原因：`uvp-hook-dsl` 只理解单个 Hook
//! expression；只有编译层拥有 Zhixu / Stage / receiveSignals / hook 集合，
//! 跨 Hook 的关系分析必须在这里，而不是让 DSL 层反向理解完整 Zhixu。
//!
//! v1 只回答可局部证明的三种关系：
//!
//! ```text
//! H1 ⇒ H2      （L021 implied-hook）
//! H1 ≡ H2      （L020 duplicate-hook-condition，双向蕴含即可证等价）
//! H1 ⊥ H2      （L022 mutually-exclusive-hooks，info 级）
//! ```
//!
//! 不做通用 overlap detection（"Can H1 and H2 both be satisfied?" 很容易
//! 演变为 satisfiability problem）；证不出一律不产生 diagnostic。
//!
//! lint 不改变 compile 语义（PRD §4.1 / §21）：本模块与 compile 产物完全
//! 独立，`合法 DSL + lint error` 仍可编译，是否阻塞由调用方 deny policy
//! 决定。

use serde::Deserialize;
use serde_json::Value;
use uvp_hook_dsl::{
    contradicts, lint_hook_with_condition, ready_implies, Category, Expr, LintDiagnostic,
    LintError, LintProof, Profile, ProofResult, Severity, MAX_PAIRWISE_HOOKS,
};
use uvp_model::ZhixuDefinition;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ZhixuLintReport {
    pub semantic_version: String,
    pub diagnostics: Vec<LintDiagnostic>,
}

#[derive(Debug, Deserialize)]
// FFI/NAPI 请求信封：未知字段确定性拒绝（与 compile 入口同口径）。
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct LintZhixuRequest {
    pub definition: Value,
}

/// Zhixu 层 lint（PRD §19）：
///
/// ```text
/// semantic validation + single hook lint + same-stage relation lint
/// ```
///
/// 语义验证失败（非合法 DSL）返回 `Err`——lint 只分析已通过 semantic
/// validation 的正常 Hook（PRD §4.2），这不是 diagnostic。
pub fn lint_zhixu(definition: &ZhixuDefinition) -> Result<ZhixuLintReport, LintError> {
    let issues = crate::validate_zhixu_shape(definition);
    if !issues.is_empty() {
        return Err(LintError::Message(issues.join("; ")));
    }
    let stage_entries =
        crate::flatten_stages(definition).map_err(|err| LintError::Message(err.to_string()))?;

    let mut diagnostics = Vec::new();
    for entry in &stage_entries {
        let mut hooks = Vec::new();
        for (hook_name, raw_expression) in &entry.stage.receive_signals {
            let hook_id = format!("{}#{hook_name}", entry.stage_identifier);
            let linted = lint_hook_with_condition(Profile::CloudCompat, hook_name, raw_expression)?;
            for mut diagnostic in linted.report.diagnostics {
                // 单 Hook 诊断在 Zhixu 语境下按 hookId（stage#hook）定位，
                // 与编译产物的命名空间一致。
                diagnostic.hook_name = Some(hook_id.clone());
                diagnostics.push(diagnostic);
            }
            hooks.push(StageHook {
                hook_id,
                source: linted.source,
                condition: linted.condition,
                normalized_expression: linted.normalized_expression,
            });
        }
        lint_stage_relations(&hooks, &mut diagnostics);
    }

    diagnostics.sort_by(|left, right| {
        left.code
            .cmp(right.code)
            .then(left.hook_name.cmp(&right.hook_name))
            .then(left.message.cmp(&right.message))
    });
    Ok(ZhixuLintReport {
        semantic_version: uvp_hook_dsl::SEMANTIC_VERSION.to_string(),
        diagnostics,
    })
}

/// lint 的 JSON 入口（信封与 compile_json 同构）。diagnostics 永远在
/// ok:true 的 value 里——lint 结论不是编译失败。
pub fn lint_zhixu_json(input: &str) -> String {
    let result = serde_json::from_str::<LintZhixuRequest>(input)
        .map_err(|err| crate::CompilerError::Message(format!("invalid lint zhixu request: {err}")))
        .and_then(|req| {
            let definition: ZhixuDefinition =
                serde_json::from_value(req.definition).map_err(|err| {
                    crate::CompilerError::Message(format!("invalid Zhixu definition: {err}"))
                })?;
            lint_zhixu(&definition)
                .map(|report| serde_json::to_value(&report).unwrap_or(Value::Null))
                .map_err(|err| crate::CompilerError::Message(err.to_string()))
        });
    crate::envelope_json(result)
}

struct StageHook {
    hook_id: String,
    /// 事实 source 类。不同 source 的同名信号是不同事实：任何跨 source
    /// 蕴含 / 等价 / 互斥都不可证，直接跳过（宁可漏报）。
    source: String,
    condition: Expr,
    normalized_expression: String,
}

fn lint_stage_relations(hooks: &[StageHook], out: &mut Vec<LintDiagnostic>) {
    if hooks.len() > MAX_PAIRWISE_HOOKS {
        // 资源上限（PRD §22）：超预算跳过两两关系 lint。lint 是旁路分析，
        // 静默降级，不得成为新的资源攻击面。
        return;
    }
    for (index, left) in hooks.iter().enumerate() {
        for right in &hooks[index + 1..] {
            if left.source != right.source {
                continue;
            }
            let forward = ready_implies(&left.condition, &right.condition);
            let backward = ready_implies(&right.condition, &left.condition);
            let equivalent = left.normalized_expression == right.normalized_expression
                || matches!(forward, ProofResult::Proven(_))
                    && matches!(backward, ProofResult::Proven(_));
            if equivalent {
                out.push(duplicate_hook_condition(left, right));
                continue;
            }
            if let ProofResult::Proven(reason) = forward {
                out.push(implied_hook(left, right, reason.as_rule()));
                continue;
            }
            if let ProofResult::Proven(reason) = backward {
                out.push(implied_hook(right, left, reason.as_rule()));
                continue;
            }
            if let Some(signal) = contradicts(&left.condition, &right.condition) {
                out.push(mutually_exclusive_hooks(left, right, &signal));
            }
        }
    }
}

/// UVP-L020 duplicate-hook-condition：同 Stage 两个 Hook condition 完全
/// 等价（指纹相同，或 ready implication 双向可证——如 `+60s` 与 `+1m`）。
fn duplicate_hook_condition(left: &StageHook, right: &StageHook) -> LintDiagnostic {
    LintDiagnostic {
        code: "UVP-L020",
        severity: Severity::Warning,
        category: Category::Relation,
        hook_name: Some(left.hook_id.clone()),
        message: format!(
            "{} and {} use equivalent conditions",
            left.hook_id, right.hook_id
        ),
        explanation: Some(format!(
            "both hooks gate on the same condition (`{}`); they are always ready at the same \
             time, which is usually an authoring mistake or leftover duplication",
            left.normalized_expression
        )),
        primary_span: None,
        related_spans: Vec::new(),
        proof: Some(LintProof {
            kind: "structural_equivalence",
            rule: None,
            premise: Some(left.normalized_expression.clone()),
            conclusion: Some(right.normalized_expression.clone()),
        }),
    }
}

/// UVP-L021 implied-hook：`ready_implies(H1, H2)` 可证 ⇒ H1 就绪时 H2 必
/// 就绪。不叫 shadowed-hook：UVP runtime 中 Hook 独立裁决、独立 delivery，
/// 不存在 first-match switch 的 shadow 语义，这里提示的是确定的 co-ready
/// 蕴含关系。
fn implied_hook(premise: &StageHook, conclusion: &StageHook, rule: &'static str) -> LintDiagnostic {
    LintDiagnostic {
        code: "UVP-L021",
        severity: Severity::Warning,
        category: Category::Relation,
        hook_name: Some(premise.hook_id.clone()),
        message: format!(
            "{} is strictly stronger than {}. Whenever {} becomes ready, {} is also ready.",
            premise.hook_id, conclusion.hook_id, premise.hook_id, conclusion.hook_id
        ),
        explanation: Some(format!(
            "`{}` ready-implies `{}`: the two hooks are deterministically co-ready in one \
             direction; if that is unintentional, tighten the weaker condition",
            premise.normalized_expression, conclusion.normalized_expression
        )),
        primary_span: None,
        related_spans: Vec::new(),
        proof: Some(LintProof {
            kind: "ready_implication",
            rule: Some(rule),
            premise: Some(premise.normalized_expression.clone()),
            conclusion: Some(conclusion.normalized_expression.clone()),
        }),
    }
}

/// UVP-L022 mutually-exclusive-hooks：两个合法 Hook 可证无法同时 Ready。
/// info 级：默认 CLI 可隐藏，主要供 Store / IDE / explain / AI 消费。
fn mutually_exclusive_hooks(left: &StageHook, right: &StageHook, signal: &str) -> LintDiagnostic {
    LintDiagnostic {
        code: "UVP-L022",
        severity: Severity::Info,
        category: Category::Relation,
        hook_name: Some(left.hook_id.clone()),
        message: format!(
            "{} and {} can never be ready at the same time (conflict on `{signal}`)",
            left.hook_id, right.hook_id
        ),
        explanation: Some(format!(
            "one condition requires `{signal}` while the other requires it absent; the two \
             hooks are provably mutually exclusive",
        )),
        primary_span: None,
        related_spans: Vec::new(),
        proof: Some(LintProof {
            kind: "mutual_exclusion",
            rule: Some("signal_polarity"),
            premise: Some(left.normalized_expression.clone()),
            conclusion: Some(right.normalized_expression.clone()),
        }),
    }
}
