//! 承诺面金向量（`fixtures/commitments/golden-vectors.v1.json`）的 Rust
//! 消费线：文件由链轨 TS compiler 生成（generate:commitment-golden-vectors），
//! 本测试对 uidVectors 与 cases 两段的每一格断言
//! `derive_definition_uid(input.definition) == expected.uid`——uid 格必须
//! 全消费；hooksHash/runtimePlanHash 格无 Rust 承诺面（TS/Foundry 消费）。
//! planHash 两格经 preimages 携带的 canonical 原像串复算：先证明本侧
//! canonicalize 重排与 TS canonicalStringify 逐字节一致，再以域分隔 +
//! keccak256 重算 == pinned 哈希。

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
    preimages: CasePreimages,
    expected: CommitmentExpected,
}

#[derive(Debug, Deserialize)]
struct CellInput {
    definition: Value,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CasePreimages {
    hook_plan_canonical: String,
    onchain_plan_canonical: String,
}

#[derive(Debug, Deserialize)]
struct CellExpected {
    uid: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CommitmentExpected {
    uid: String,
    hook_plan_hash: String,
    onchain_plan_hash: String,
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

#[test]
fn plan_hash_preimages_rehash_to_every_pinned_cell() {
    let vectors: Vectors = serde_json::from_str(VECTORS).expect("commitment vectors should decode");
    for case in &vectors.cases {
        for (domain, pinned_canonical, pinned_hash) in [
            (
                uvp_ir::HOOK_PLAN_HASH_DOMAIN,
                &case.preimages.hook_plan_canonical,
                &case.expected.hook_plan_hash,
            ),
            (
                uvp_ir::ONCHAIN_PLAN_HASH_DOMAIN,
                &case.preimages.onchain_plan_canonical,
                &case.expected.onchain_plan_hash,
            ),
        ] {
            let payload: Value = serde_json::from_str(pinned_canonical)
                .unwrap_or_else(|err| panic!("{}: preimage is not JSON: {err}", case.name));
            assert_eq!(
                uvp_ir::canonical_stringify(&payload).unwrap(),
                *pinned_canonical,
                "{}: Rust canonicalize diverged from the pinned TS canonicalStringify form",
                case.name
            );
            assert_eq!(
                uvp_ir::hash_canonical(domain, &payload).unwrap(),
                *pinned_hash,
                "{}: plan hash diverged from the pinned vector",
                case.name
            );
        }
    }
}
