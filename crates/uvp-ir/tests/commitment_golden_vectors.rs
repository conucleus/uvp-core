//! 承诺面金向量（`fixtures/commitments/golden-vectors.v1.json`）的 Rust
//! 消费线：文件由链轨 TS compiler 生成（generate:commitment-golden-vectors），
//! 本测试对 uidVectors、cases 与 dockVectors 三段的每一格断言
//! `derive_definition_uid`/`derive_dock_identity` 与 TS 生成器同源——uid 格必须
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
    dock_vectors: Vec<DockVectorCell>,
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

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DockVectorCell {
    name: String,
    input: DockVectorInput,
    expected: DockVectorExpected,
}

#[derive(Debug, Deserialize)]
struct DockVectorInput {
    dock: Value,
    from: Value,
    to: Value,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DockVectorExpected {
    dkid: String,
    doc_hash: String,
    from: DockEndpoint,
    to: DockTarget,
    outputs: Vec<DockOutput>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DockEndpoint {
    zhixu: String,
    source: String,
    source_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DockTarget {
    zhixu: String,
    stage: String,
    stage_id: String,
    source: String,
    source_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DockOutput {
    signal: String,
    signal_id: String,
    source_id: String,
    stage_id: String,
    target_order_relation: u8,
    atoms: Vec<DockAtom>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DockAtom {
    source: String,
    source_id: String,
    stage: String,
    stage_id: String,
    signal: String,
    signal_id: String,
    target_order_relation: u8,
}

#[test]
fn definition_uid_matches_every_pinned_cell() {
    let vectors: Vectors = serde_json::from_str(VECTORS).expect("commitment vectors should decode");
    assert_eq!(
        vectors.schema_version, "uvp.commitmentGoldenVectors.v3",
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
        consumed >= 8,
        "uid face must keep consuming every definition cell ({consumed} consumed)"
    );
}

#[test]
fn dock_identity_and_resolved_ids_match_every_pinned_cell() {
    let vectors: Vectors = serde_json::from_str(VECTORS).expect("commitment vectors should decode");
    for cell in &vectors.dock_vectors {
        let identity =
            uvp_ir::derive_dock_identity(&cell.input.dock).expect("dock document should hash");
        assert_eq!(
            identity.dkid, cell.expected.dkid,
            "{}: dkid diverged from the pinned vector",
            cell.name
        );
        assert_eq!(
            identity.doc_hash, cell.expected.doc_hash,
            "{}: docHash diverged from the pinned vector",
            cell.name
        );
        assert_eq!(
            uvp_ir::derive_definition_uid(&cell.input.from).unwrap(),
            cell.expected.from.zhixu,
            "{}: from.zhixu diverged from the pinned vector",
            cell.name
        );
        assert_eq!(
            uvp_ir::derive_definition_uid(&cell.input.to).unwrap(),
            cell.expected.to.zhixu,
            "{}: to.zhixu diverged from the pinned vector",
            cell.name
        );
        assert_eq!(
            cell.expected.from.source_id,
            uvp_ir::keccak256_utf8_hex(&cell.expected.from.source),
            "{}: from.sourceId is not keccak(source)",
            cell.name
        );
        assert_eq!(
            cell.expected.to.stage_id,
            uvp_ir::keccak256_utf8_hex(&cell.expected.to.stage),
            "{}: to.stageId is not keccak(stage)",
            cell.name
        );
        assert_eq!(
            cell.expected.to.source_id,
            uvp_ir::keccak256_utf8_hex(&cell.expected.to.source),
            "{}: to.sourceId is not keccak(source)",
            cell.name
        );
        for output in &cell.expected.outputs {
            assert_eq!(
                output.stage_id, cell.expected.to.stage_id,
                "{}: output stageId diverged from to.stageId",
                cell.name
            );
            assert_eq!(
                output.source_id, cell.expected.to.source_id,
                "{}: output sourceId diverged from to.sourceId",
                cell.name
            );
            assert_eq!(
                output.signal_id,
                uvp_ir::keccak256_utf8_hex(&format!(
                    "{}.{}",
                    cell.expected.to.stage, output.signal
                )),
                "{}: output signalId is not keccak(to.stage.signal)",
                cell.name
            );
            assert_eq!(
                output.target_order_relation, 0,
                "{}: output targetOrderRelation must be 0 (current order)",
                cell.name
            );
            for atom in &output.atoms {
                assert_eq!(
                    atom.source, cell.expected.from.source,
                    "{}: atom source diverged from from.source",
                    cell.name
                );
                assert_eq!(
                    atom.source_id,
                    uvp_ir::keccak256_utf8_hex(&atom.source),
                    "{}: atom sourceId is not keccak(source)",
                    cell.name
                );
                assert_eq!(
                    atom.stage_id,
                    uvp_ir::keccak256_utf8_hex(&atom.stage),
                    "{}: atom stageId is not keccak(stage)",
                    cell.name
                );
                assert_eq!(
                    atom.signal_id,
                    uvp_ir::keccak256_utf8_hex(&format!("{}.{}", atom.stage, atom.signal)),
                    "{}: atom signalId is not keccak(stage.signal)",
                    cell.name
                );
                assert_eq!(
                    atom.target_order_relation, 0,
                    "{}: atom targetOrderRelation must be 0 (current order)",
                    cell.name
                );
            }
        }
    }
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
