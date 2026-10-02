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
    pub task_patterns: Vec<ZhixuTaskPattern>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub dock_interface: BTreeMap<String, DockInterfaceSpec>,
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
pub struct ZhixuTaskPattern {
    pub name: String,
    #[serde(default)]
    pub stages: Vec<ZhixuStage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ZhixuStage {
    pub name: String,
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mint: Option<String>,
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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ZhixuSendSignal {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub valid_when: Option<Vec<String>>,
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
    pub zhixu_executor_config: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selectable_resource: Option<Value>,
}

pub const SUPPLIER_TYPES: [&str; 3] = ["individual", "organization", "zhixu"];

pub fn is_known_supplier_type(value: &str) -> bool {
    SUPPLIER_TYPES.contains(&value)
}

pub const FILE_TYPES: [&str; 4] = ["local", "http", "txcloud", "plain_text"];

pub fn is_known_file_type(value: &str) -> bool {
    FILE_TYPES.contains(&value)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct DockInterfaceSpec {
    pub order_modes: Vec<String>,
    #[serde(default)]
    pub inputs: BTreeMap<String, DockInputPortSource>,
    #[serde(default)]
    pub outputs: BTreeMap<String, DockOutputPortSource>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct DockInputPortSource {
    pub hook: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct DockOutputPortSource {
    pub signal: String,
}
