use serde_json::{Map, Number, Value};
use sha3::{Digest, Keccak256};
use thiserror::Error;

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

pub fn derive_definition_uid(definition: &Value) -> Result<String> {
    let stripped = strip_definition_display_fields(canonicalize(definition)?);
    let canonical = canonical_stringify(&stripped)?;
    let preimage = format!("{DEFINITION_UID_DOMAIN}:{canonical}");
    let digest = Keccak256::digest(preimage.as_bytes());
    let mut uid = String::with_capacity(3 + 64);
    uid.push_str("zx-");
    for byte in digest.iter().take(16) {
        uid.push_str(&format!("{byte:02x}"));
    }
    Ok(uid)
}

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

        let renamed = json!({
            "apiVersion": "uvp/v0", "kind": "Zhixu",
            "metadata": {"name": "renamed_order", "labels": {"domain": "demo"}},
            "spec": {"platform": {"type": "cloud"}}
        });
        assert_eq!(uid, derive_definition_uid(&renamed).unwrap());
        let changed = json!({
            "apiVersion": "uvp/v0", "kind": "Zhixu",
            "metadata": {"name": "weaving_order", "labels": {"domain": "demo"}},
            "spec": {"platform": {"type": "cloud"}, "nucleation": {"id": "n1"}}
        });
        assert_ne!(uid, derive_definition_uid(&changed).unwrap());
        let reordered = serde_json::from_str::<Value>(
            r#"{"spec":{"platform":{"type":"cloud"}},"metadata":{"labels":{"domain":"demo"},"annotations":{"origin":"console"},"name":"weaving_order"},"kind":"Zhixu","apiVersion":"uvp/v0"}"#,
        )
        .unwrap();
        assert_eq!(uid, derive_definition_uid(&reordered).unwrap());
    }

    #[test]
    fn definition_uid_golden_vectors() {
        let cases: &[(Value, &str)] = &[(
            json!({"apiVersion":"uvp/v0","kind":"Zhixu","metadata":{"name":"weaving_order"},"spec":{"platform":{"type":"cloud"}}}),
            "zx-e906ad47866918682d1e2ed2528682f5",
        )];
        for (definition, want) in cases {
            let got = derive_definition_uid(definition).unwrap();
            assert_eq!(&got, want, "definition: {definition}");
        }
    }
}
