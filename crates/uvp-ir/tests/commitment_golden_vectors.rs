//! 承诺面金向量（`fixtures/commitments/golden-vectors.v1.json`）的 Rust
//! 消费线：文件由链轨 TS compiler 生成（generate:commitment-golden-vectors），
//! 本测试对 uidVectors 与 cases 两段的每一格断言
//! `derive_definition_uid(input.definition) == expected.uid`——uid 格必须
//! 全消费；planHash/hooksHash 格无 Rust 承诺面（链轨 TS/Foundry 消费），
//! 不在本测试范围。

use serde::Deserialize;
use serde_json::Value;

const VECTORS: &str = include_str!("../../../fixtures/commitments/golden-vectors.v1.json");

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Vectors {
    schema_version: String,
    uid_vectors: Vec<UidCell>,
    cases: Vec<CommitmentCase>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UidCell {
    name: String,
    input: CellInput,
    expected: CellExpected,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CommitmentCase {
    name: String,
    input: CellInput,
    expected: CellExpected,
}

#[derive(Debug, Deserialize)]
struct CellInput {
    definition: Value,
}

#[derive(Debug, Deserialize)]
struct CellExpected {
    uid: String,
}

#[test]
fn definition_uid_matches_every_pinned_cell() {
    let vectors: Vectors = serde_json::from_str(VECTORS).expect("commitment vectors should decode");
    assert_eq!(
        vectors.schema_version, "uvp.commitmentGoldenVectors.v1",
        "commitment golden vectors schemaVersion drifted; migrate this consumer before trusting the file"
    );
    let mut consumed = 0usize;
    for cell in &vectors.uid_vectors {
        assert_eq!(
            uvp_ir::derive_definition_uid(&cell.input.definition).unwrap(),
            cell.expected.uid,
            "{}: uid diverged from the pinned vector",
            cell.name
        );
        consumed += 1;
    }
    for case in &vectors.cases {
        assert_eq!(
            uvp_ir::derive_definition_uid(&case.input.definition).unwrap(),
            case.expected.uid,
            "{}: uid diverged from the pinned vector",
            case.name
        );
        consumed += 1;
    }
    assert!(
        consumed >= 11,
        "uid face must keep consuming every definition cell ({consumed} consumed)"
    );
}
