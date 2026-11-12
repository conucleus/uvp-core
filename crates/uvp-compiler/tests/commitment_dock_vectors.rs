//! `dockVectors` 金向量的 Rust 生产侧消费：文件由链轨 TS compiler 生成，
//! 本测试对每条 dockVector 用 `compile_dock_artifact` 重编译 Dock 文档，
//! 与 TS 侧用 @uvp-eth/hook-core parser 产出的 expected 做整对象全等
//! （dkid/docHash、from/to resolved IDs、每 output 的 rawExpression/AST/
//! dependencies/atoms/IDs/relation），钉住 Dock 产物契约；无 DNF。

use serde_json::{json, Value};

const VECTORS: &str = include_str!("../../../fixtures/commitments/golden-vectors.v1.json");

#[test]
fn dock_artifact_matches_every_pinned_cell() {
    let fixture: Value = serde_json::from_str(VECTORS).expect("commitment vectors should decode");
    let dock_vectors = fixture["dockVectors"]
        .as_array()
        .expect("commitment vectors carry dockVectors");
    let mut consumed = 0usize;
    for cell in dock_vectors {
        let name = cell["name"].as_str().unwrap_or("<unnamed>");
        let input = &cell["input"];
        let expected = &cell["expected"];
        let published = json!([
            {
                "uid": expected["from"]["zhixu"],
                "definition": input["from"],
            },
            {
                "uid": expected["to"]["zhixu"],
                "definition": input["to"],
            },
        ]);
        let artifact = uvp_compiler::compile_dock_artifact(&input["dock"], &published)
            .unwrap_or_else(|issues| {
                let messages = issues
                    .iter()
                    .map(std::string::ToString::to_string)
                    .collect::<Vec<_>>()
                    .join("; ");
                panic!("{name}: dock artifact compilation failed: {messages}");
            });
        let expected_artifact = json!({
            "schemaVersion": "uvp.dock.v1",
            "dkid": expected["dkid"],
            "docHash": expected["docHash"],
            "from": expected["from"],
            "to": expected["to"],
            "signalMap": expected["outputs"],
        });
        assert_eq!(
            artifact, expected_artifact,
            "{name}: dock artifact diverged from the pinned vector"
        );
        consumed += 1;
    }
    assert!(
        consumed >= 2,
        "dock face must keep consuming every dockVector ({consumed} consumed)"
    );
}
