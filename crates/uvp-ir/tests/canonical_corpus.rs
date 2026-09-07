//! Canonical-JSON 跨语言钉死语料：Rust（uvp-ir）是权威，TS 侧
//! canonical.ts 必须对同一份 fixture 产出逐字节相同的 canonical 串。
//! 关键分歧点全部来自浮点：整数/浮点身份保持、整值浮点的 `.0`、
//! 负零符号、ryu 最短往返与科学计数边界（fixture 内 rules 字段是
//! 规则文本的唯一出处）。

use serde::Deserialize;
use serde_json::Value;

const CORPUS: &str = include_str!("../../../fixtures/canonical/canonical.v1.json");

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Corpus {
    cases: Vec<Case>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Case {
    name: String,
    input: Value,
    expect_canonical: String,
}

#[test]
fn canonical_stringify_matches_the_pinned_vectors() {
    let corpus: Corpus = serde_json::from_str(CORPUS).expect("canonical corpus should decode");
    for case in corpus.cases {
        let actual = uvp_ir::canonical_stringify(&case.input)
            .unwrap_or_else(|err| panic!("{} failed to canonicalize: {err}", case.name));
        assert_eq!(
            actual, case.expect_canonical,
            "{}: canonical bytes diverged from the pinned vector",
            case.name
        );
    }
}
