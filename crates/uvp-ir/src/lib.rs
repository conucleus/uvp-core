use serde_json::{Map, Number, Value};
use sha3::{Digest, Keccak256};
use thiserror::Error;

/// 定义身份 uid 的派生域。内容派生：定义文档的 canonical JSON 是
/// preimage；展示性字段（metadata.name、metadata.annotations）先剔除，
/// 改展示名不换身份。同一内容跨环境、跨轨得到同一 uid——发布由此获得
/// 免费幂等，内容变即新身份，不存在「重发布」。
pub const DEFINITION_UID_DOMAIN: &str = "uvp:definition-uid:v2";

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

/// 定义文档（canonical 形态）剔除展示性字段：metadata.name 与
/// metadata.annotations。metadata 缺省时原样返回。
fn strip_definition_display_fields(mut definition: Value) -> Value {
    let Some(metadata) = definition
        .as_object_mut()
        .and_then(|root| root.get_mut("metadata"))
        .and_then(|metadata| metadata.as_object_mut())
    else {
        return definition;
    };
    metadata.remove("name");
    metadata.remove("annotations");
    definition
}

/// 定义身份 uid：`zx-<32hex>`。preimage = domain 串 + 剔除展示字段后的
/// canonical JSON；keccak256 与链轨派生同一公式族（链侧 TS 实现以共享
/// 金向量语料互钉）。
pub fn derive_definition_uid(definition: &Value) -> Result<String> {
    let stripped = strip_definition_display_fields(canonicalize(definition)?);
    let canonical = canonical_stringify(&stripped)?;
    let preimage = format!("{DEFINITION_UID_DOMAIN}:{canonical}");
    let digest = Keccak256::digest(preimage.as_bytes());
    let mut uid = String::with_capacity(3 + 64);
    uid.push_str("zx-");
    // 与链轨公式族一致：取摘要前 32 个 hex 字符（16 字节），uid 总长 35。
    for byte in digest.iter().take(16) {
        uid.push_str(&format!("{byte:02x}"));
    }
    Ok(uid)
}

// 数字的 canonical 规则（Rust 是跨语言权威，TS canonical.ts 必须同口径；
// 钉死向量见 fixtures/canonical/canonical.v1.json）：
// - 哈希输入词表封闭：浮点形态的数字字面量（serde_json
//   的 f64 载荷，含整值浮点 1.0、指数写法 1e2、负零 -0.0）在权威
//   canonicalization 一律响亮拒绝并列出肇事 token——跨语言浮点格式化
//   （ryu vs JS Number→String）无逐字节对齐义务，单一拒绝面放在权威侧。
// - 整数放行的精确边界：i64/u64 载荷（含超 double 精度的 u64 整数，
//   如 2^53+1）原样序列化不带小数点；超出 64 位范围的整数字面量
//   （如 2^64）在 JSON 解析层即成 f64 载荷，按浮点形态拒绝——拒绝的
//   裁决面是载荷类型，不是字面量的书写形态。
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
    fn definition_uid_is_content_derived_and_display_agnostic() {
        let base = json!({
            "apiVersion": "uvp/v0",
            "kind": "Zhixu",
            "metadata": {
                "name": "weaving_order",
                "annotations": {"origin": "console"},
                "labels": {"domain": "demo"}
            },
            "spec": {"platform": {"type": "cloud"}}
        });
        let uid = derive_definition_uid(&base).unwrap();
        assert!(uid.starts_with("zx-"));
        assert_eq!(uid.len(), 35);
        assert!(uid[3..].chars().all(|c| c.is_ascii_hexdigit()), "{uid}");

        // 改展示名/注解不换身份。
        let renamed = json!({
            "apiVersion": "uvp/v0", "kind": "Zhixu",
            "metadata": {"name": "renamed_order", "labels": {"domain": "demo"}},
            "spec": {"platform": {"type": "cloud"}}
        });
        assert_eq!(uid, derive_definition_uid(&renamed).unwrap());
        // 改实质内容换身份。
        let changed = json!({
            "apiVersion": "uvp/v0", "kind": "Zhixu",
            "metadata": {"name": "weaving_order", "labels": {"domain": "demo"}},
            "spec": {"platform": {"type": "cloud"}, "nucleation": {"id": "n1"}}
        });
        assert_ne!(uid, derive_definition_uid(&changed).unwrap());
        // 键序不影响身份。
        let reordered = serde_json::from_str::<Value>(
            r#"{"spec":{"platform":{"type":"cloud"}},"metadata":{"labels":{"domain":"demo"},"annotations":{"origin":"console"},"name":"weaving_order"},"kind":"Zhixu","apiVersion":"uvp/v0"}"#,
        )
        .unwrap();
        assert_eq!(uid, derive_definition_uid(&reordered).unwrap());
    }

    #[test]
    fn definition_uid_golden_vectors() {
        // 金向量：链轨 TS 实现与 Go FFI 消费方按同一向量对拍；向量变更=
        // 派生公式变更=所有定义换 uid。
        let cases: &[(Value, &str)] = &[
            (
                json!({"apiVersion":"uvp/v0","kind":"Zhixu","metadata":{"name":"weaving_order"},"spec":{"platform":{"type":"cloud"}}}),
                "zx-e906ad47866918682d1e2ed2528682f5",
            ),
        ];
        for (definition, want) in cases {
            let got = derive_definition_uid(definition).unwrap();
            assert_eq!(&got, want, "definition: {definition}");
        }
    }

    #[test]
    fn canonical_json_sorts_object_keys() {
        assert_eq!(
            canonical_stringify(&json!({"b": 1, "a": {"d": 4, "c": 3}})).unwrap(),
            r#"{"a":{"c":3,"d":4},"b":1}"#
        );
    }

    #[test]
    fn canonical_hash_inputs_reject_float_form_numbers() {
        // 整值浮点/分数/指数/负零一律拒绝，错误列出肇事 token；i64/u64
        // 载荷的整数（含超 double 精度的 u64）照常放行；超出 64 位范围的
        // 整数字面量经 JSON 解析即成 f64 载荷，按浮点形态拒绝（权威行为
        // =实现，拒绝面按载荷类型裁决）。
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

        // 超出 64 位范围的整数字面量：JSON 解析层即成 f64 载荷，按浮点
        // 形态拒绝（钉住权威行为——拒绝面按载荷类型裁决，2^64 整字面量
        // 不是"整数放行"的例外）。
        let beyond_u64: Value =
            serde_json::from_str(r#"{"a":18446744073709551616}"#).expect("parses as f64");
        let err = canonical_stringify(&beyond_u64).expect_err(
            "integer literals beyond the 64-bit range parse as f64 and must be rejected",
        );
        assert!(matches!(err, CanonicalError::FloatNumber { .. }), "{err}");
        assert!(err.to_string().contains("float-form JSON number"), "{err}");
    }
}
