//! 标识符严格语法的跨语言钉死（Rust 线）：本测试与 uvp-utils 仓
//! `api/core/v0` 的 ValidateIdentifierPart 语料测试对同一份
//! `fixtures/identifiers/identifier-parts.v1.json` 做逐条判定比对——语料
//! 条目作为 sendSignals 裸名进入 compile 的严格语法闸（valid_identifier_part），
//! 任一侧漂移（放宽字符集、改首字符规则）都会让两侧测试同声报警。

use serde::Deserialize;
use serde_json::{json, Value};

const CORPUS: &str = include_str!("../../../fixtures/identifiers/identifier-parts.v1.json");

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct IdentifierPartsCorpus {
    schema_version: String,
    valid: Vec<String>,
    invalid: Vec<String>,
}

fn corpus_definition(send_signal: &str) -> Value {
    json!({
        "apiVersion": "uvp/v0",
        "kind": "Zhixu",
        "metadata": {"name": "identifier_corpus"},
        "spec": {
            "platform": {"type": "cloud"},
            "nucleation": {"id": "core"},
            "taskPatterns": [{
                "name": "seed",
                "stages": [{
                    "name": "boot",
                    "source": "buyer",
                    "receiveSignals": {"START": "buyer::seed.boot.ignition"},
                    "sendSignals": [{"name": "ignition"}, {"name": send_signal}],
                    "executor": {"supplierType": "organization", "supplierID": "org-a"}
                }]
            }]
        }
    })
}

fn compile_with_send_signal(
    send_signal: &str,
) -> std::result::Result<Value, uvp_compiler::CompilerError> {
    uvp_compiler::compile_request(&uvp_compiler::CompileRequest {
        target: "hook_plan".to_string(),
        definition: corpus_definition(send_signal),
        dock_targets: None,
    })
}

#[test]
fn send_signal_identifier_grammar_matches_the_pinned_corpus() {
    let corpus: IdentifierPartsCorpus =
        serde_json::from_str(CORPUS).expect("identifier-parts corpus should decode");
    assert_eq!(
        corpus.schema_version, "uvp.identifierParts.v1",
        "identifier-parts corpus schemaVersion drifted: migrate every consumer before shipping the new file"
    );
    assert!(
        !corpus.valid.is_empty() && !corpus.invalid.is_empty(),
        "identifier-parts corpus lists must be non-empty"
    );

    for name in &corpus.valid {
        let result = compile_with_send_signal(name);
        assert!(
            result.is_ok(),
            "corpus member {name:?} must satisfy the strict identifier grammar: {result:?}"
        );
    }
    for name in &corpus.invalid {
        let result = compile_with_send_signal(name);
        assert!(
            result.is_err(),
            "corpus member {name:?} must be rejected by the strict identifier grammar"
        );
    }
}
