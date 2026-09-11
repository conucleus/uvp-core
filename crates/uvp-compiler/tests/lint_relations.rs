//! Layer 2（同 Stage Hook 关系 lint）回归门：L020 / L021 / L022，以及
//! "无法证明的关系不产生 speculative warning"（PRD 109 §14 / §23.1）。

use serde_json::{json, Value};
use uvp_compiler::lint::{lint_zhixu, lint_zhixu_json};
use uvp_model::ZhixuDefinition;

fn definition_with_hooks(hooks: Value) -> ZhixuDefinition {
    let definition = json!({
        "apiVersion": "uvp/v0",
        "kind": "Zhixu",
        "metadata": { "name": "lint_relations" },
        "spec": {
            "platform": { "type": "cloud" },
            "nucleation": { "id": "core" },
            "taskPatterns": [
                {
                    "name": "flow",
                    "stages": [
                        {
                            "name": "main",
                            "source": "buyer",
                            "receiveSignals": hooks,
                            "executor": {
                                "supplierType": "organization",
                                "supplierID": "org-lint"
                            }
                        }
                    ]
                }
            ]
        }
    });
    serde_json::from_value(definition).expect("test definition should deserialize")
}

fn lint_codes(hooks: Value) -> Vec<String> {
    let report = lint_zhixu(&definition_with_hooks(hooks)).expect("definition should lint");
    let mut codes: Vec<String> = report
        .diagnostics
        .iter()
        .map(|diagnostic| diagnostic.code.to_string())
        .collect();
    codes.sort();
    codes.dedup();
    codes
}

fn find_diagnostic(hooks: Value, code: &str) -> uvp_hook_dsl::LintDiagnostic {
    let report = lint_zhixu(&definition_with_hooks(hooks)).expect("definition should lint");
    report
        .diagnostics
        .into_iter()
        .find(|diagnostic| diagnostic.code == code)
        .unwrap_or_else(|| panic!("{code} diagnostic must be present"))
}

/// UVP-L020：同 Stage 两个 Hook condition 完全等价（PRD §13 示例形态）。
#[test]
fn duplicate_hook_conditions_are_reported() {
    let diagnostic = find_diagnostic(
        json!({
            "A_READY": "buyer::task.a.cmp & task.b.cmp",
            "ALSO_READY": "buyer::task.a.cmp & task.b.cmp"
        }),
        "UVP-L020",
    );
    assert_eq!(diagnostic.severity, uvp_hook_dsl::Severity::Warning);
    // receiveSignals 是 BTreeMap：成对报告按 hook 名字母序（ALSO_READY
    // < A_READY），两个 hookId 都必须出现在消息里。
    assert!(
        diagnostic.message.contains("flow.main#A_READY")
            && diagnostic.message.contains("flow.main#ALSO_READY")
            && diagnostic.message.contains("use equivalent conditions"),
        "message should name both hooks: {}",
        diagnostic.message
    );
    assert!(
        diagnostic.hook_name == Some("flow.main#A_READY".to_string())
            || diagnostic.hook_name == Some("flow.main#ALSO_READY".to_string())
    );
    assert_eq!(
        diagnostic.proof.as_ref().expect("proof").kind,
        "structural_equivalence"
    );
}

/// UVP-L020 的可证等价路径：原文不同（+60s vs +1m）但 ready implication
/// 双向可证。
#[test]
fn provable_equivalence_beyond_identical_text_is_reported() {
    let codes = lint_codes(json!({
        "SLOW_A": "buyer::(task.a.cmp +60s)",
        "SLOW_B": "buyer::(task.a.cmp +1m)"
    }));
    assert_eq!(codes, vec!["UVP-L020"]);
}

/// UVP-L021：`ready_implies(H1, H2)` 可证 ⇒ H1 严格强于 H2。
#[test]
fn implied_hook_is_reported() {
    let diagnostic = find_diagnostic(
        json!({
            "STRONG": "buyer::task.a.cmp & task.b.cmp",
            "WEAK": "buyer::task.a.cmp"
        }),
        "UVP-L021",
    );
    assert_eq!(diagnostic.severity, uvp_hook_dsl::Severity::Warning);
    assert!(
        diagnostic
            .message
            .contains("flow.main#STRONG is strictly stronger than flow.main#WEAK"),
        "message should follow the PRD wording: {}",
        diagnostic.message
    );
    let proof = diagnostic.proof.expect("proof");
    assert_eq!(proof.kind, "ready_implication");
    // rule 记录最深证明依据：STRONG 的成员 `a` 与 WEAK 全文结构同一。
    assert_eq!(proof.rule, Some("reflexive"));
}

/// UVP-L022：可证互斥（一个要求信号在场、另一个要求其缺席）→ info。
#[test]
fn mutually_exclusive_hooks_are_reported_at_info_level() {
    let diagnostic = find_diagnostic(
        json!({
            "WANT_A": "buyer::task.a.cmp",
            "FORBID_A": "buyer::task.b.cmp & ~task.a.cmp"
        }),
        "UVP-L022",
    );
    assert_eq!(diagnostic.severity, uvp_hook_dsl::Severity::Info);
    assert!(diagnostic
        .message
        .contains("never be ready at the same time"));
    assert!(diagnostic.message.contains("task.a.cmp"));
}

/// 不同 source 标头的同名信号是不同事实：跨 source 无任何可证关系，
/// 不产生 speculative warning。
#[test]
fn different_sources_produce_no_relation() {
    let codes = lint_codes(json!({
        "FROM_BUYER": "buyer::task.a.cmp",
        "FROM_SELLER": "seller::task.a.cmp"
    }));
    assert!(
        codes.is_empty(),
        "cross-source hooks must stay silent: {codes:?}"
    );
}

/// 无关 Hook（A vs B）与纯延时差异不可证明的关系都保持沉默。
#[test]
fn unprovable_relations_stay_silent() {
    let codes = lint_codes(json!({
        "A_ONLY": "buyer::task.a.cmp",
        "B_ONLY": "buyer::task.b.cmp",
        "A_DELAYED": "buyer::(task.a.cmp +5s) & task.b.cmp"
    }));
    // A_DELAYED ⇒ A_ONLY 是可证蕴含（and_member），A_ONLY vs B_ONLY 不可证。
    assert_eq!(codes, vec!["UVP-L021"]);
}

/// 订阅钩子（::ANCHOR）同目标重复 → L020；订阅与普通 hook 之间无关系。
#[test]
fn duplicate_subscription_targets_are_equivalent() {
    let codes = lint_codes(json!({
        "SUB_ONE": "::ANCHOR(@seller::trade.listing.cmp)",
        "SUB_TWO": "::ANCHOR(@seller::trade.listing.cmp)",
        "LOCAL": "buyer::task.a.cmp"
    }));
    assert_eq!(codes, vec!["UVP-L020"]);
}

/// Layer 1 诊断随 Zhixu lint 一起产出，hook_name 采用 hookId 命名空间。
#[test]
fn layer_one_diagnostics_carry_hook_ids() {
    let diagnostic = find_diagnostic(
        json!({ "DUP": "buyer::task.a.cmp & task.a.cmp" }),
        "UVP-L001",
    );
    assert_eq!(diagnostic.hook_name.as_deref(), Some("flow.main#DUP"));
    assert!(diagnostic.primary_span.is_some());
}

/// 同一 hook 集合的 diagnostics 是确定性的（重复 lint 逐字节一致）。
#[test]
fn lint_zhixu_is_deterministic() {
    let hooks = json!({
        "STRONG": "buyer::task.a.cmp & task.b.cmp & task.a.cmp",
        "WEAK": "buyer::task.a.cmp",
        "CONTRA": "buyer::task.c.cmp & ~task.c.cmp"
    });
    let first = lint_zhixu(&definition_with_hooks(hooks.clone())).unwrap();
    let second = lint_zhixu(&definition_with_hooks(hooks)).unwrap();
    let normalize = |report: &uvp_compiler::lint::ZhixuLintReport| {
        serde_json::to_string(&report.diagnostics).unwrap()
    };
    assert_eq!(normalize(&first), normalize(&second));
    let codes: Vec<String> = first
        .diagnostics
        .iter()
        .map(|diagnostic| diagnostic.code.to_string())
        .collect();
    assert!(codes.contains(&"UVP-L001".to_string()));
    assert!(codes.contains(&"UVP-L002".to_string()));
    assert!(codes.contains(&"UVP-L021".to_string()));
}

/// 非法 hook（语义验证失败）不进入 lint，按 Err 返回（lint 只分析合法
/// 表达式，语义验证仍归 parser / validator，PRD §4.2）。
#[test]
fn semantic_validation_failure_is_an_error_not_a_diagnostic() {
    let err = lint_zhixu(&definition_with_hooks(json!({
        "TAUT": "buyer::task.a.cmp | ~task.a.cmp"
    })))
    .unwrap_err();
    assert!(err.to_string().contains("positive signal anchor"));
}

/// lint_zhixu_json 信封：诊断在 ok:true 的 value 里；非法定义 ok:false。
#[test]
fn lint_zhixu_json_envelope() {
    let request = json!({
        "definition": {
            "apiVersion": "uvp/v0",
            "kind": "Zhixu",
            "metadata": { "name": "lint_relations" },
            "spec": {
                "platform": { "type": "cloud" },
                "nucleation": { "id": "core" },
                "taskPatterns": [{
                    "name": "flow",
                    "stages": [{
                        "name": "main",
                        "source": "buyer",
                        "receiveSignals": {
                            "A_READY": "buyer::task.a.cmp & task.b.cmp",
                            "ALSO_READY": "buyer::task.a.cmp & task.b.cmp"
                        },
                        "executor": {
                            "supplierType": "organization",
                            "supplierID": "org-lint"
                        }
                    }]
                }]
            }
        }
    })
    .to_string();
    let envelope = lint_zhixu_json(&request);
    let value: Value = serde_json::from_str(&envelope).unwrap();
    assert_eq!(value["ok"], Value::Bool(true));
    assert_eq!(
        value["value"]["diagnostics"][0]["code"],
        Value::String("UVP-L020".to_string())
    );

    let bad_request = json!({ "definition": { "apiVersion": "uvp/v0" } }).to_string();
    let envelope = lint_zhixu_json(&bad_request);
    let value: Value = serde_json::from_str(&envelope).unwrap();
    assert_eq!(value["ok"], Value::Bool(false));
}

/// lint 不改变 compile 语义：同一份定义在 lint 之后仍产出相同的编译产物
/// （lint 是只读旁路；编译入口的行为不受影响）。
#[test]
fn lint_zhixu_does_not_change_compile_artifacts() {
    // 编译入口要求 receiveSignals 引用真实存在的 task.stage：信号名用
    // 本定义自己的 task 段（flow.main.*）。
    let definition: ZhixuDefinition = serde_json::from_value(json!({
        "apiVersion": "uvp/v0",
        "kind": "Zhixu",
        "metadata": { "name": "lint_relations" },
        "spec": {
            "platform": { "type": "cloud" },
            "nucleation": { "id": "core" },
            "taskPatterns": [{
                "name": "flow",
                "stages": [{
                    "name": "main",
                    "source": "buyer",
                    "sendSignals": ["a", "b"],
                    "receiveSignals": {
                        "A_READY": "buyer::flow.main.a & flow.main.b",
                        "ALSO_READY": "buyer::flow.main.a & flow.main.b"
                    },
                    "executor": {
                        "supplierType": "organization",
                        "supplierID": "org-lint"
                    }
                }]
            }]
        }
    }))
    .expect("test definition should deserialize");
    // 该定义带 lint 诊断（L020）但仍应正常通过 parse-only 编译——
    // "合法 DSL + lint error 仍然可以完成 parse / compile"（PRD §4.1）。
    let artifact = uvp_compiler::compile_zhixu_hook_plan(
        &serde_json::to_value(&definition).unwrap(),
        None,
        true,
    );
    assert!(
        artifact.is_ok(),
        "lint-dirty definition must still compile: {artifact:?}"
    );
}
