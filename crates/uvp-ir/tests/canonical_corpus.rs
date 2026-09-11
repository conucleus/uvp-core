//! Canonical-JSON 跨语言钉死语料：Rust（uvp-ir）是权威，TS 侧
//! canonical.ts 必须对同一份 fixture 同口径——通过用例逐字节复现
//! expectCanonical，拒绝用例（expectReject）以同等的单一拒绝面失败
//! （哈希输入词表对浮点形态数字字面量封闭）。
//! fixture 内 rules 字段是规则文本的唯一出处。

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
    /// 互斥：存在即本用例必须以包含该子串的错误响亮失败。
    #[serde(default)]
    expect_reject: Option<String>,
    /// 互斥：存在即本用例必须产出该 canonical 串。
    #[serde(default)]
    expect_canonical: Option<String>,
}

#[test]
fn canonical_stringify_matches_the_pinned_vectors() {
    let corpus: Corpus = serde_json::from_str(CORPUS).expect("canonical corpus should decode");
    let mut pass_count = 0usize;
    let mut reject_count = 0usize;
    for case in corpus.cases {
        match (
            case.expect_reject.as_deref(),
            case.expect_canonical.as_deref(),
        ) {
            (Some(anchor), None) => {
                let err = uvp_ir::canonical_stringify(&case.input)
                    .err()
                    .unwrap_or_else(|| panic!("{} must be rejected", case.name));
                assert!(
                    err.to_string().contains(anchor),
                    "{}: rejection message lacks anchor {anchor:?}: {err}",
                    case.name
                );
                reject_count += 1;
            }
            (None, Some(expected)) => {
                let actual = uvp_ir::canonical_stringify(&case.input)
                    .unwrap_or_else(|err| panic!("{} failed to canonicalize: {err}", case.name));
                assert_eq!(
                    actual, expected,
                    "{}: canonical bytes diverged from the pinned vector",
                    case.name
                );
                pass_count += 1;
            }
            _ => panic!(
                "{}: case must carry exactly one of expectReject/expectCanonical",
                case.name
            ),
        }
    }
    assert!(
        pass_count >= 3 && reject_count >= 5,
        "corpus must keep pinning both the integer pass face ({pass_count}) and the float \
         rejection face ({reject_count})"
    );
}
