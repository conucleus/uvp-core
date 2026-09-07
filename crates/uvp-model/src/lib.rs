use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ObjectMeta {
    /// 作者技术标签（PRD_102）：参与内容派生，但无唯一性/关系语义。
    /// 定义身份由编译器从内容派生，uid 不是作者可写字段——出现即未知字段。
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

/// spec 顶层拒绝未知字段：不受支持或拼错的字段都是确定性非法输入，
/// 静默忽略会把"看似生效"的定义落成零值
/// （与 Go 入口 decodeObjectStrict 的 DisallowUnknownFields 等值）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ZhixuSpec {
    pub platform: ZhixuPlatform,
    pub nucleation: Nucleation,
    #[serde(default)]
    pub task_patterns: Vec<ZhixuTaskPattern>,
    /// 目标侧公开的具名对接接口 map（PRD_100 §9）：键为接口名。调用方
    /// 只能引用接口名与端口名，不能看到目标内部 stage/signal。
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
    /// per-fact：本阶段为出生阶段，订阅事实逐条由引擎代铸订单。
    /// 由出生阶段声明；编译期唯一锚定依据（见 subscription-mint-spec.md）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub executor: Option<ZhixuExecutor>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub selected_stages: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub send_signals: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub receive_signals: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub file_resources: BTreeMap<String, Value>,
}

/// typed executor（PRD94 §2/§12.1）。
/// `supplierID` 在 `supplierType: zhixu` 时由编译器禁止（D001）。
/// 未知字段直接拒绝：flatten 透传会静默吞掉拼错字段，
/// 与 Go 入口的 DisallowUnknownFields 等值；`zhixuExecutorConfig` 内容
/// 由 dock 模块按 D002 校验。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ZhixuExecutor {
    pub supplier_type: String,
    /// schema 字段名为 `supplierID`（全大写 ID），非 camelCase。
    #[serde(
        rename = "supplierID",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub supplier_id: Option<String>,
    /// 保持 Value 以便编译器产出带 JSON path / 错误码（D001-D007）的
    /// 结构化错误，而不是裸 serde 报错。语义权威在 uvp-compiler::dock。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zhixu_executor_config: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selectable_resource: Option<Value>,
}

/// `spec.dockInterface` 下的具名接口（PRD_100 §9.1-§9.4）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct DockInterfaceSpec {
    /// `{new, existing}` 的非空子集，无重复；new ⇒ 至少一个 input 端口
    /// （建单型服务必须有入口）。
    pub order_modes: Vec<String>,
    #[serde(default)]
    pub inputs: BTreeMap<String, DockInputPortSource>,
    #[serde(default)]
    pub outputs: BTreeMap<String, DockOutputPortSource>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct DockInputPortSource {
    /// `<task>.<stage>#<receiveHookName>`
    pub hook: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct DockOutputPortSource {
    /// `<source>::<task>.<stage>.<signal>`
    pub signal: String,
}
