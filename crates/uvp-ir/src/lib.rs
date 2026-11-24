use serde_json::{Map, Number, Value};
use sha3::{Digest, Keccak256};
use thiserror::Error;

pub const DEFINITION_UID_DOMAIN: &str = "uvp:definition-uid:v2";

pub const HOOK_PLAN_HASH_DOMAIN: &str = "uvp:hook-plan-artifact:v1";

pub const ONCHAIN_PLAN_HASH_DOMAIN: &str = "uvp:onchain-hook-plan-artifact:v1";

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

pub fn hash_canonical(domain: &str, payload: &Value) -> Result<String> {
    let canonical = canonical_stringify(payload)?;
    let preimage = format!("{domain}:{canonical}");
    let digest = Keccak256::digest(preimage.as_bytes());
    Ok(format!(
        "0x{}",
        digest
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    ))
}

/// Definition identity is content-derived over the track-neutral logic only.
/// Display/metadata fields (`metadata.name`, `metadata.annotations`,
/// `metadata.labels`) and track/deployment selectors (`spec.platform`,
/// `spec.nucleation`) are deliberately excluded so the same Zhixu logic yields
/// the exact same `definitionUid` on the cloud and chain tracks.
fn strip_definition_identity_fields(mut definition: Value) -> Value {
    if let Some(root) = definition.as_object_mut() {
        if let Some(metadata) = root
            .get_mut("metadata")
            .and_then(|metadata| metadata.as_object_mut())
        {
            metadata.remove("name");
            metadata.remove("annotations");
            metadata.remove("labels");
        }
        if let Some(spec) = root.get_mut("spec").and_then(|spec| spec.as_object_mut()) {
            spec.remove("platform");
            spec.remove("nucleation");
        }
    }
    definition
}

fn keccak_hex(preimage: &str) -> String {
    let digest = Keccak256::digest(preimage.as_bytes());
    format!(
        "0x{}",
        digest
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    )
}

fn content_digest(domain: &str, document: &Value, strip: fn(Value) -> Value) -> Result<String> {
    let stripped = strip(canonicalize(document)?);
    let canonical = canonical_stringify(&stripped)?;
    Ok(keccak_hex(&format!("{domain}:{canonical}")))
}

pub fn derive_definition_uid(definition: &Value) -> Result<String> {
    let digest = content_digest(
        DEFINITION_UID_DOMAIN,
        definition,
        strip_definition_identity_fields,
    )?;
    Ok(format!("zx-{}", &digest[2..34]))
}

pub fn keccak256_utf8_hex(value: &str) -> String {
    keccak_hex(value)
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
    fn definition_uid_is_content_derived_and_track_agnostic() {
        let base = json!({
            "apiVersion": "uvp/v0",
            "kind": "Zhixu",
            "metadata": {
                "name": "weaving_order",
                "annotations": {"origin": "console"},
                "labels": {"domain": "demo"}
            },
            "spec": {
                "platform": {"type": "cloud"},
                "nucleation": {"id": "n1"},
                "stages": [{"name": "main", "source": "buyer"}]
            }
        });
        let uid = derive_definition_uid(&base).unwrap();
        assert!(uid.starts_with("zx-"));
        assert_eq!(uid.len(), 35);
        assert!(uid[3..].chars().all(|c| c.is_ascii_hexdigit()), "{uid}");

        // Display fields and track selectors must not fork the identity.
        let changed_metadata = json!({
            "apiVersion": "uvp/v0", "kind": "Zhixu",
            "metadata": {
                "name": "renamed_order",
                "annotations": {"origin": "chain"},
                "labels": {"domain": "other", "owner": "someone"}
            },
            "spec": {
                "platform": {"type": "blockchain", "provider": "eth", "network": "base"},
                "nucleation": {"id": "n2"},
                "stages": [{"name": "main", "source": "buyer"}]
            }
        });
        assert_eq!(uid, derive_definition_uid(&changed_metadata).unwrap());

        let stripped_shell = json!({
            "apiVersion": "uvp/v0", "kind": "Zhixu",
            "metadata": {},
            "spec": {"stages": [{"name": "main", "source": "buyer"}]}
        });
        assert_eq!(uid, derive_definition_uid(&stripped_shell).unwrap());

        // Track-neutral logic still forks the identity.
        let changed_logic = json!({
            "apiVersion": "uvp/v0", "kind": "Zhixu",
            "metadata": {"name": "weaving_order"},
            "spec": {
                "platform": {"type": "cloud"},
                "nucleation": {"id": "n1"},
                "stages": [{"name": "main", "source": "seller"}]
            }
        });
        assert_ne!(uid, derive_definition_uid(&changed_logic).unwrap());

        let reordered = serde_json::from_str::<Value>(
            r#"{"spec":{"stages":[{"source":"buyer","name":"main"}],"nucleation":{"id":"n1"},"platform":{"type":"cloud"}},"metadata":{"labels":{"domain":"demo"},"annotations":{"origin":"console"},"name":"weaving_order"},"kind":"Zhixu","apiVersion":"uvp/v0"}"#,
        )
        .unwrap();
        assert_eq!(uid, derive_definition_uid(&reordered).unwrap());
    }
}
