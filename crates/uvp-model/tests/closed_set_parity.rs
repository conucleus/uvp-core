//! 链上承诺闭集词表的跨语言钉死（Rust 线）：本测试与 uvp-protocol 仓
//! `packages/compiler/test/closed-set-parity.test.ts` 对同一份
//! `fixtures/closed-sets/closed-sets.v1.json` 做逐元素相等比对——闭集
//! 内容与成员序都是承诺面，任一侧漂移（增删成员、改序、换比对口径）
//! 会让两侧测试同声报警，不存在单侧静默改词表的路径。

use serde::Deserialize;
use serde_json::Value;

const CORPUS: &str = include_str!("../../../fixtures/closed-sets/closed-sets.v1.json");

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ClosedSets {
    schema_version: String,
    supplier_types: Vec<String>,
    file_types: Vec<String>,
}

#[test]
fn closed_set_constants_match_the_pinned_corpus() {
    let corpus: ClosedSets =
        serde_json::from_str(CORPUS).expect("closed-set corpus should decode");
    assert_eq!(corpus.schema_version, "uvp.closedSets.v1");

    let pinned_supplier_types: Vec<&str> = corpus.supplier_types.iter().map(String::as_str).collect();
    assert_eq!(
        pinned_supplier_types, uvp_model::SUPPLIER_TYPES,
        "supplierType 闭集与钉死语料分叉：改词表必须同改语料与 TS/Go 镜像"
    );

    let pinned_file_types: Vec<&str> = corpus.file_types.iter().map(String::as_str).collect();
    assert_eq!(
        pinned_file_types, uvp_model::FILE_TYPES,
        "fileType 闭集与钉死语料分叉：改词表必须同改语料与 TS/Go 镜像"
    );
}

#[test]
fn closed_set_membership_is_exact_match() {
    // 精确匹配、不 trim 是两侧共同口径：带空白变体按闭集外拒绝，且
    // serde 值（语料/产物携带的字符串）与常量同源判定。
    let corpus: Value =
        serde_json::from_str(CORPUS).expect("closed-set corpus should decode");
    for word in corpus["supplierTypes"].as_array().expect("supplierTypes array") {
        let word = word.as_str().expect("supplierType words are strings");
        assert!(uvp_model::is_known_supplier_type(word));
    }
    for word in corpus["fileTypes"].as_array().expect("fileTypes array") {
        let word = word.as_str().expect("fileType words are strings");
        assert!(uvp_model::is_known_file_type(word));
    }
    for padded in [" individual", "organization ", "\tzhixu"] {
        assert!(!uvp_model::is_known_supplier_type(padded));
    }
    for padded in [" local", "http ", "plain_text\n"] {
        assert!(!uvp_model::is_known_file_type(padded));
    }
}
