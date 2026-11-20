//! 闭集词表钉死（Rust 线）：supplierTypes 与 uvp-protocol 仓
//! `packages/compiler/test/closed-set-parity.test.ts` 对同一份
//! `fixtures/closed-sets/closed-sets.v1.json` 做逐元素相等比对。
//! fileType 值域开放，不在闭集语料内。

use serde::Deserialize;

const CORPUS: &str = include_str!("../../../fixtures/closed-sets/closed-sets.v1.json");

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ClosedSets {
    schema_version: String,
    supplier_types: Vec<String>,
}

#[test]
fn closed_set_constants_match_the_pinned_corpus() {
    let corpus: ClosedSets = serde_json::from_str(CORPUS).expect("closed-set corpus should decode");
    assert_eq!(corpus.schema_version, "uvp.closedSets.v1");

    let pinned_supplier_types: Vec<&str> =
        corpus.supplier_types.iter().map(String::as_str).collect();
    assert_eq!(
        pinned_supplier_types,
        uvp_model::SUPPLIER_TYPES,
        "supplierType 闭集与钉死语料分叉：改词表必须同改语料与 TS/Go 镜像"
    );
}
