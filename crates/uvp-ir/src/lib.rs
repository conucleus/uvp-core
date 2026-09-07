use serde_json::{Map, Number, Value};
use sha3::{Digest, Keccak256};
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

pub fn hash_canonical(domain: &str, payload: &Value) -> Result<String> {
    let input = format!("{domain}:{}", canonical_stringify(payload)?);
    Ok(keccak256_hex(input.as_bytes()))
}

pub fn keccak256_hex(data: &[u8]) -> String {
    let digest = Keccak256::digest(data);
    let mut out = String::with_capacity(66);
    out.push_str("0x");
    for byte in digest {
        out.push(hex_char(byte >> 4));
        out.push(hex_char(byte & 0x0f));
    }
    out
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

fn hex_char(nibble: u8) -> char {
    match nibble {
        0..=9 => (b'0' + nibble) as char,
        10..=15 => (b'a' + nibble - 10) as char,
        _ => unreachable!("nibble is always <= 15"),
    }
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
    fn keccak_matches_existing_vector() {
        assert_eq!(
            hash_canonical("uvp:test", &json!({"a": 1})).unwrap(),
            "0x8c020ee5a62ce7f8e00b8b079cc4d573dc3c21dc57c8e00912c8f02c7b6587a4"
        );
    }
}
