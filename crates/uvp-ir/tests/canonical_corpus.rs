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
    schema_version: String,
    cases: Vec<Case>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Case {
    name: String,
    input: Value,
    #[serde(default)]
    expect_reject: Option<String>,
    #[serde(default)]
    expect_canonical: Option<String>,
    /// Cross-track exception: TS must reject this Rust acceptance vector
    /// (JS numbers are limited to ±(2^53-1)).
    #[serde(default)]
    expect_ts_reject: Option<String>,
}

fn has_integer_beyond_safe_range(value: &Value) -> bool {
    const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;
    match value {
        Value::Number(number) => {
            number
                .as_u64()
                .is_some_and(|integer| integer > MAX_SAFE_INTEGER)
                || number
                    .as_i64()
                    .is_some_and(|integer| integer < -(MAX_SAFE_INTEGER as i64))
        }
        Value::Array(items) => items.iter().any(has_integer_beyond_safe_range),
        Value::Object(record) => record.values().any(has_integer_beyond_safe_range),
        _ => false,
    }
}

#[test]
fn canonical_stringify_matches_the_pinned_vectors() {
    let corpus: Corpus = serde_json::from_str(CORPUS).expect("canonical corpus should decode");
    assert_eq!(
        corpus.schema_version, "uvp.canonicalJsonCorpus.v1",
        "canonical corpus schemaVersion drifted; migrate this consumer before trusting the file"
    );
    let mut pass_count = 0usize;
    let mut reject_count = 0usize;
    let mut ts_reject_count = 0usize;
    for case in corpus.cases {
        match (
            case.expect_reject.as_deref(),
            case.expect_canonical.as_deref(),
            case.expect_ts_reject.as_deref(),
        ) {
            (Some(anchor), None, None) => {
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
            (None, Some(expected), ts_reject) => {
                let actual = uvp_ir::canonical_stringify(&case.input)
                    .unwrap_or_else(|err| panic!("{} failed to canonicalize: {err}", case.name));
                assert_eq!(
                    actual, expected,
                    "{}: canonical bytes diverged from the pinned vector",
                    case.name
                );
                if let Some(ts_anchor) = ts_reject {
                    assert_eq!(
                        ts_anchor, "safe integer",
                        "{}: expectTsReject must name the TS safe-integer rejection face",
                        case.name
                    );
                    assert!(
                        has_integer_beyond_safe_range(&case.input),
                        "{}: expectTsReject only applies to integers beyond ±(2^53-1)",
                        case.name
                    );
                    ts_reject_count += 1;
                }
                pass_count += 1;
            }
            _ => panic!(
                "{}: case must carry expectReject, or expectCanonical with an optional expectTsReject",
                case.name
            ),
        }
    }
    assert!(
        pass_count >= 3 && reject_count >= 5,
        "corpus must keep pinning both the integer pass face ({pass_count}) and the float \
         rejection face ({reject_count})"
    );
    assert!(
        ts_reject_count >= 1,
        "corpus must keep the > 2^53 cross-track contract explicit (Rust accepts u64, TS rejects)"
    );
}
