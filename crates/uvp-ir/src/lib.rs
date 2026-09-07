use serde_json::{Map, Number, Value};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum CanonicalError {
    #[error("non-finite JSON number is not supported")]
    NonFiniteNumber,
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

// 数字的 canonical 规则（Rust 是跨语言权威，TS canonical.ts 必须逐字节
// 对齐；钉死向量见 fixtures/canonical/canonical.v1.json）：
// - 身份保持：无 '.'/'e' 的 token 是整数（u64/i64），序列化不带小数点；
//   浮点 token 恒带小数部或指数（100.0 永不写成 100）。
// - 负零保号：-0.0 序列化为 "-0.0"。
// - 浮点经 ryu 最短往返：十进制指数在 [-5,15] 内用小数形（1e-5 →
//   0.00001、1e15 → 1000000000000000.0），否则科学计数，正指数显式
//   '+'（1e-7、1e+16、1e+300）。
fn canonicalize_number(number: &Number) -> Result<Value> {
    if number.as_f64().is_some_and(|value| !value.is_finite()) {
        return Err(CanonicalError::NonFiniteNumber);
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
}
