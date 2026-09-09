use serde_json::{Map, Number, Value};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum CanonicalError {
    #[error(
        "float-form JSON number {token:?} is rejected: canonical hash inputs accept integer \
         number literals only (cross-language float formatting diverges)"
    )]
    FloatNumber { token: String },
    #[error("unsupported JSON value")]
    UnsupportedValue,
}

pub type Result<T> = std::result::Result<T, CanonicalError>;

pub fn canonicalize(value: &Value) -> Result<Value> {
    match value {
        Value::Null | Value::Bool(_) | Value::String(_) => Ok(value.clone()),
        Value::Number(number) => canonicalize_number(number),
        Value::Array(items) => Ok(Value::Array(
            items.iter().map(canonicalize).collect::<Result<Vec<_>>>()?,
        )),
        Value::Object(record) => {
            let mut sorted = Map::new();
            let mut keys = record.keys().collect::<Vec<_>>();
            keys.sort();
            for key in keys {
                sorted.insert(key.clone(), canonicalize(&record[key])?);
            }
            Ok(Value::Object(sorted))
        }
    }
}

pub fn canonical_stringify(value: &Value) -> Result<String> {
    Ok(serde_json::to_string(&canonicalize(value)?).expect("canonical JSON should serialize"))
}

// 数字的 canonical 规则（Rust 是跨语言权威，TS canonical.ts 必须同口径；
// 钉死向量见 fixtures/canonical/canonical.v1.json）：
// - 哈希输入词表封闭：浮点形态的数字字面量（serde_json
//   的 f64 载荷，含整值浮点 1.0、指数写法 1e2、负零 -0.0）在权威
//   canonicalization 一律响亮拒绝并列出肇事 token——跨语言浮点格式化
//   （ryu vs JS Number→String）无逐字节对齐义务，单一拒绝面放在权威侧，
//   整数（u64/i64）原样序列化不带小数点。
// - 拒绝与 JSON 解析无关：非哈希用途的 JSON 解析不受影响，仅 canonical
//   串化（哈希 preimage 域）执行该封闭词表。
fn canonicalize_number(number: &Number) -> Result<Value> {
    if number.is_f64() {
        return Err(CanonicalError::FloatNumber {
            token: number.to_string(),
        });
    }
    Ok(Value::Number(number.clone()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn canonical_json_sorts_object_keys() {
        assert_eq!(
            canonical_stringify(&json!({"b": 1, "a": {"d": 4, "c": 3}})).unwrap(),
            r#"{"a":{"c":3,"d":4},"b":1}"#
        );
    }

    #[test]
    fn canonical_hash_inputs_reject_float_form_numbers() {
        // 整值浮点/分数/指数/负零一律拒绝，错误列出肇事
        // token；整数（含超 double 精度的 u64）照常放行。
        for (label, value) in [
            ("integral float", json!({ "a": 1.0 })),
            ("fraction", json!({ "a": 1.5 })),
            ("exponent spelling", json!({ "a": 1e2 })),
            ("negative zero", json!({ "a": -0.0 })),
            ("nested in array", json!({ "a": [1, 2.5] })),
            ("deep nested", json!({ "a": { "b": [0.1] } })),
        ] {
            let err = canonical_stringify(&value)
                .err()
                .unwrap_or_else(|| panic!("{label} must be rejected"));
            assert!(
                matches!(err, CanonicalError::FloatNumber { .. }),
                "{label}: {err}"
            );
            assert!(
                err.to_string().contains("float-form JSON number"),
                "{label}: {err}"
            );
        }
        // 深层对象的第一个肇事数字按 key 排序确定性地报出。
        let err = canonical_stringify(&json!({ "b": 0.5, "a": 0.25 }))
            .expect_err("sorted-key traversal hits the first float deterministically");
        assert!(err.to_string().contains("0.25"), "{err}");

        assert_eq!(
            canonical_stringify(&json!({ "a": 9007199254740993_u64 })).unwrap(),
            r#"{"a":9007199254740993}"#
        );
    }
}
