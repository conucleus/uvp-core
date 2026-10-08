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
            "stages": [{
                            "name": "main",
                            "source": "buyer",
                            "receiveSignals": hooks,
                            "executor": {
                                "supplierType": "organization",
                                "supplierID": "org-lint"
                            }
                        }]
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

#[test]
fn duplicate_hook_conditions_are_reported() {
    let diagnostic = find_diagnostic(
        json!({
            "A_READY": "buyer::a.cmp & b.cmp",
            "ALSO_READY": "buyer::a.cmp & b.cmp"
        }),
        "UVP-L020",
    );
    assert_eq!(diagnostic.severity, uvp_hook_dsl::Severity::Warning);
    assert!(
        diagnostic.message.contains("main#A_READY")
            && diagnostic.message.contains("main#ALSO_READY")
            && diagnostic.message.contains("use equivalent conditions"),
        "message should name both hooks: {}",
        diagnostic.message
    );
    assert!(
        diagnostic.hook_name == Some("main#A_READY".to_string())
            || diagnostic.hook_name == Some("main#ALSO_READY".to_string())
    );
    assert_eq!(
        diagnostic.proof.as_ref().expect("proof").kind,
        "structural_equivalence"
    );
}

#[test]
fn provable_equivalence_beyond_identical_text_is_reported() {
    let codes = lint_codes(json!({
        "SLOW_A": "buyer::(a.cmp +60s)",
        "SLOW_B": "buyer::(a.cmp +1m)"
    }));
    assert_eq!(codes, vec!["UVP-L020"]);
}

#[test]
fn implied_hook_is_reported() {
    let diagnostic = find_diagnostic(
        json!({
            "STRONG": "buyer::a.cmp & b.cmp",
            "WEAK": "buyer::a.cmp"
        }),
        "UVP-L021",
    );
    assert_eq!(diagnostic.severity, uvp_hook_dsl::Severity::Warning);
    assert!(
        diagnostic
            .message
            .contains("main#STRONG is strictly stronger than main#WEAK"),
        "message should follow the PRD wording: {}",
        diagnostic.message
    );
    let proof = diagnostic.proof.expect("proof");
    assert_eq!(proof.kind, "ready_implication");
    assert_eq!(proof.rule, Some("reflexive"));
}

#[test]
fn mutually_exclusive_hooks_are_reported_at_info_level() {
    let diagnostic = find_diagnostic(
        json!({
            "WANT_A": "buyer::a.cmp",
            "FORBID_A": "buyer::b.cmp & ~a.cmp"
        }),
        "UVP-L022",
    );
    assert_eq!(diagnostic.severity, uvp_hook_dsl::Severity::Info);
    assert!(diagnostic
        .message
        .contains("never be ready at the same time"));
    assert!(diagnostic.message.contains("a.cmp"));
}

#[test]
fn different_sources_produce_no_relation() {
    let codes = lint_codes(json!({
        "FROM_BUYER": "buyer::a.cmp",
        "FROM_SELLER": "seller::a.cmp"
    }));
    assert!(
        codes.is_empty(),
        "cross-source hooks must stay silent: {codes:?}"
    );
}

#[test]
fn unprovable_relations_stay_silent() {
    let codes = lint_codes(json!({
        "A_ONLY": "buyer::a.cmp",
        "B_ONLY": "buyer::b.cmp",
        "A_DELAYED": "buyer::(a.cmp +5s) & b.cmp"
    }));
    assert_eq!(codes, vec!["UVP-L021"]);
}

#[test]
fn duplicate_subscription_targets_are_equivalent() {
    let codes = lint_codes(json!({
        "SUB_ONE": "::ANCHOR(@seller::listing.cmp)",
        "SUB_TWO": "::ANCHOR(@seller::listing.cmp)",
        "LOCAL": "buyer::a.cmp"
    }));
    assert_eq!(codes, vec!["UVP-L020"]);
}

#[test]
fn layer_one_diagnostics_carry_hook_ids() {
    let diagnostic = find_diagnostic(json!({ "DUP": "buyer::a.cmp & a.cmp" }), "UVP-L001");
    assert_eq!(diagnostic.hook_name.as_deref(), Some("main#DUP"));
    assert!(diagnostic.primary_span.is_some());
}

#[test]
fn semantic_validation_failure_is_an_error_not_a_diagnostic() {
    let err = lint_zhixu(&definition_with_hooks(json!({
        "TAUT": "buyer::a.cmp | ~a.cmp"
    })))
    .unwrap_err();
    assert!(err.to_string().contains("positive signal anchor"));
}

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
                "stages": [{
                        "name": "main",
                        "source": "buyer",
                        "receiveSignals": {
                            "A_READY": "buyer::a.cmp & b.cmp",
                            "ALSO_READY": "buyer::a.cmp & b.cmp"
                        },
                        "executor": {
                            "supplierType": "organization",
                            "supplierID": "org-lint"
                        }
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

#[test]
fn lint_zhixu_does_not_change_compile_artifacts() {
    let definition: ZhixuDefinition = serde_json::from_value(json!({
            "apiVersion": "uvp/v0",
            "kind": "Zhixu",
            "metadata": { "name": "lint_relations" },
            "spec": {
                "platform": { "type": "cloud" },
                "nucleation": { "id": "core" },
                "stages": [{
                        "name": "main",
                        "source": "buyer",
                        "sendSignals": [
    { "name": "a" },
    { "name": "b" }
    ],
                        "receiveSignals": {
                            "A_READY": "buyer::main.a & main.b",
                            "ALSO_READY": "buyer::main.a & main.b"
                        },
                        "executor": {
                            "supplierType": "organization",
                            "supplierID": "org-lint"
                        }
                    }]
            }
        }))
    .expect("test definition should deserialize");
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
