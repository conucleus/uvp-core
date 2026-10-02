//! UVP Core Lint v1 的 Layer 2：同 Stage Hook 关系 lint（PRD 109 §12–§14）。

use serde::Deserialize;
use serde_json::Value;
use uvp_hook_dsl::{
    contradicts, lint_hook_with_condition, ready_implies, Category, Expr, Gate, LintDiagnostic,
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
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct LintZhixuRequest {
    pub definition: Value,
}

pub fn lint_zhixu(definition: &ZhixuDefinition) -> Result<ZhixuLintReport, LintError> {
    let issues = crate::validate::validate_zhixu_shape(definition);
    if !issues.is_empty() {
        return Err(LintError::Message(issues.join("; ")));
    }
    let stage_entries = crate::lower::flatten_stages(definition)
        .map_err(|err| LintError::Message(err.to_string()))?;

    let mut diagnostics = Vec::new();
    for entry in &stage_entries {
        let mut hooks = Vec::new();
        for (hook_name, raw_expression) in &entry.stage.receive_signals {
            let hook_id = format!("{}#{hook_name}", entry.stage_identifier);
            let linted = lint_hook_with_condition(
                Profile::CloudCompat,
                Gate::Hook,
                hook_name,
                raw_expression,
            )?;
            for mut diagnostic in linted.report.diagnostics {
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
    source: String,
    condition: Expr,
    normalized_expression: String,
}

fn lint_stage_relations(hooks: &[StageHook], out: &mut Vec<LintDiagnostic>) {
    if hooks.len() > MAX_PAIRWISE_HOOKS {
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
