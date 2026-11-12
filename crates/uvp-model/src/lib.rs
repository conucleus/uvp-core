use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ObjectMeta {
    pub name: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub labels: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub annotations: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ZhixuDefinition {
    pub api_version: String,
    pub kind: String,
    pub metadata: ObjectMeta,
    pub spec: ZhixuSpec,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ZhixuSpec {
    pub platform: ZhixuPlatform,
    pub nucleation: Nucleation,
    #[serde(default)]
    pub stages: Vec<ZhixuStage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ZhixuPlatform {
    #[serde(rename = "type")]
    pub platform_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub network: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub params: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Nucleation {
    pub id: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub params: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ZhixuStage {
    pub name: String,
    pub source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub executor: Option<ZhixuExecutor>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub selected_stages: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub send_signals: Vec<ZhixuSendSignal>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub receive_signals: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub file_resources: BTreeMap<String, Value>,
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "deserialize_file_writers"
    )]
    pub file_writers: Vec<String>,
}

fn deserialize_file_writers<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw = Vec::<Value>::deserialize(deserializer)?;
    let mut writers = Vec::with_capacity(raw.len());
    for (index, entry) in raw.into_iter().enumerate() {
        match entry {
            Value::String(writer) => writers.push(writer),
            other => {
                return Err(serde::de::Error::custom(format!(
                    "spec.stages[].fileWriters[{index}] must be a stage-name string, found {other}"
                )))
            }
        }
    }
    Ok(writers)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ZhixuSendSignal {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub valid_when: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub trigger: bool,
}

impl ZhixuStage {
    pub fn has_trigger(&self) -> bool {
        self.send_signals.iter().any(|signal| signal.trigger)
    }

    pub fn trigger_signal_names(&self) -> Vec<String> {
        self.send_signals
            .iter()
            .filter(|signal| signal.trigger)
            .map(|signal| {
                if signal.name.contains('.') {
                    signal.name.clone()
                } else {
                    format!("{}.{}", self.name, signal.name)
                }
            })
            .collect()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ZhixuExecutor {
    pub supplier_type: String,
    #[serde(
        rename = "supplierID",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub supplier_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selectable_resource: Option<Value>,
}

pub const SUPPLIER_TYPES: [&str; 3] = ["individual", "organization", "dock"];

pub fn is_known_supplier_type(value: &str) -> bool {
    SUPPLIER_TYPES.contains(&value)
}

pub const FILE_TYPES: [&str; 4] = ["local", "http", "txcloud", "plain_text"];

pub fn is_known_file_type(value: &str) -> bool {
    FILE_TYPES.contains(&value)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct DockDefinition {
    pub api_version: String,
    pub kind: String,
    pub metadata: ObjectMeta,
    pub spec: DockSpec,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct DockSpec {
    pub from: DockFrom,
    pub to: DockTo,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub signal_map: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct DockFrom {
    pub zhixu: String,
    pub source: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct DockTo {
    pub zhixu: String,
    pub stage: String,
}
