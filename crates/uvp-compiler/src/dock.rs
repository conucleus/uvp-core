//! Zhixu Dock 委托协议 DSL 壳的结构性语义。
//!
//! 本模块固定：
//! - 调用方 `executor.zhixuExecutorConfig`（键闭集 {target, interface,
//!   order, inputMap, signalMap}）与目标 `spec.dockInterface`（具名接口
//!   map）的 source 语义；
//! - 跨定义 linker（resolution manifest 输入，纯函数，无网络；目标按
//!   name 解析）；
//! - 中性 route/接口声明产物形状；
//! - 编译期错误码 D001-D016、D019-D020、D025、接口形状错误码
//!   D021/D022。
//!
//! 哈希承诺与派生身份是各轨权威的内务（链轨 TS、云轨 DB），不入共享
//! core：目标引用一律走 `metadata.name`（slug），产物不携带任何
//! uid/hash/root 字段。

use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use uvp_hook_dsl::{parse_hook, ParseHookRequest, Profile};
use uvp_model::{DockInterfaceSpec, ZhixuStage};

// ---------------------------------------------------------------------------
// 冻结常量
// ---------------------------------------------------------------------------

pub const DOCK_ROUTE_SCHEMA_VERSION: &str = "uvp.dockRoute.v2";
/// 未解析 route 的声明面产物形态：本地声明完整——
/// target:null 动态选择的目标空缺（云轨运行时由选择记录补齐）；静态
/// 目标 route 在 parse-only 产物中同面携带作者声明的 target.zhixu。
pub const DOCK_ROUTE_UNRESOLVED_SCHEMA_VERSION: &str = "uvp.dockRoute.unresolved.v1";
pub const DOCK_RESOLUTION_SCHEMA_VERSION: &str = "uvp.dock.resolution.v2";

pub const MAX_DOCK_INPUTS: usize = 8;
pub const MAX_DOCK_OUTPUTS: usize = 16;
/// Maximum number of definitions in a statically linked startup path. Runtime
/// adapters must enforce the same limit against the actual parent instance
/// depth as well; this linker check cannot observe runtime-created orders.
pub const MAX_DOCK_DEPTH: u8 = 8;
/// `^[a-z][a-z0-9_]{0,31}$`：端口名与接口名同规则。
pub const MAX_PORT_NAME_BYTES: usize = 32;

/// signalMap key 上限：运行期 hook 命名空间 = "signalMap." + key（10 字节
/// 前缀）而 hook_name 列宽 36 ⇒ key 上限 26。与 Go 镜像统一口径，避免
/// 27-36 字节 key 在一侧收、另一侧放的分裂。
pub const MAX_SIGNAL_MAP_KEY_LENGTH: usize = 26;

/// canonical 三段式信号名（task.stage.signal）落
/// individual_record.signal_name / hook_dependency.signal_name 的列宽。
/// stage 标识符 + "." + signalMap key 的组合长度按同值钉死。
pub const MAX_SIGNAL_NAME_BYTES: usize = 100;

/// 单接口端口数（inputs+outputs）上限：D016 的 route 绑定数上限
/// （8/16）只约束单条 route，接口侧端口可被多条 route 跨定义绑定——
/// 端口计数无闸时声明面规模随 plan 输入无界增长（M31 计数闸）。
pub const MAX_INTERFACE_PORTS: usize = 64;

/// resolution manifest 的 definitions 条目上限：manifest 是发布方数据，
/// 计数无闸时毒 manifest 可让 link 期的图规模无界增长（M31 计数闸）。
pub const MAX_MANIFEST_DEFINITIONS: usize = 256;

/// order.mode 与接口 orderModes 的闭集取值。
pub const ORDER_MODE_NEW: &str = "new";
pub const ORDER_MODE_EXISTING: &str = "existing";
pub const ORDER_MODES: [&str; 2] = [ORDER_MODE_NEW, ORDER_MODE_EXISTING];

// ---------------------------------------------------------------------------
// 错误模型
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct DockIssue {
    pub code: &'static str,
    pub path: String,
    pub message: String,
}

impl DockIssue {
    fn new(code: &'static str, path: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code,
            path: path.into(),
            message: message.into(),
        }
    }
}

impl std::fmt::Display for DockIssue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} {}: {}", self.code, self.path, self.message)
    }
}

pub type DockResult<T> = std::result::Result<T, Vec<DockIssue>>;

const UNSUPPORTED_HINT: &str = "Zhixu delegation binds a named target interface: publish target \
    spec.dockInterface {<interface>: {orderModes, inputs, outputs}} and bind \
    executor.zhixuExecutorConfig {target, interface, order.mode, inputMap, signalMap-to-port-names}; \
    re-link and republish, do not expect runtime compatibility";

// ---------------------------------------------------------------------------
// 调用方 executor config（source 层）
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct ZhixuExecutorConfig {
    /// `None` = `target: null`（动态选择，运行时由选择记录补齐）；
    /// `Some(name)` = 目标定义的 `metadata.name`。
    pub target_name: Option<String>,
    pub interface_name: String,
    /// `new` | `existing`。
    pub order_mode: String,
    pub input_map: BTreeMap<String, String>,
    pub signal_map: BTreeMap<String, String>,
}

/// 解析并本地校验 `executor.zhixuExecutorConfig`（D001-D006、D010、D019）。
/// `stage` 为该 executor 所属 stage；`stage_identifier` 为报错 JSON path 前缀。
pub fn parse_zhixu_executor_config(
    executor_value: &Value,
    stage: &ZhixuStage,
    stage_identifier: &str,
) -> DockResult<ZhixuExecutorConfig> {
    let path = format!("{stage_identifier}.executor.zhixuExecutorConfig");
    let mut issues = Vec::new();

    // D001：zhixu executor 禁止 supplierID——键出现即违规（空串/纯空白是
    // "看似生效"的零值占位，与拼错字段同罪，不因空值豁免）。
    if executor_value.get("supplierID").is_some() {
        issues.push(DockIssue::new(
            "D001",
            format!("{stage_identifier}.executor.supplierID"),
            "supplierID is forbidden when supplierType is zhixu; the target identity must live in zhixuExecutorConfig.target",
        ));
    }

    let Some(config) = executor_value.get("zhixuExecutorConfig") else {
        issues.push(DockIssue::new(
            "D002",
            &path,
            "zhixuExecutorConfig is required when supplierType is zhixu",
        ));
        return Err(issues);
    };
    let Some(config_object) = config.as_object() else {
        issues.push(DockIssue::new(
            "D002",
            &path,
            "zhixuExecutorConfig must be an object",
        ));
        return Err(issues);
    };

    // D002：键闭集（schemaVersion 等残留键 = 未知字段硬错误）。
    const ALLOWED_KEYS: [&str; 5] = ["target", "interface", "order", "inputMap", "signalMap"];
    for key in config_object.keys() {
        if !ALLOWED_KEYS.contains(&key.as_str()) {
            issues.push(DockIssue::new(
                "D002",
                format!("{path}.{key}"),
                format!("unknown field {key:?}; allowed: {ALLOWED_KEYS:?}"),
            ));
        }
    }

    // D003：target 必填键——{zhixu: <目标定义 metadata.name>} 或显式 null
    // （动态选择）。跨轨引用一律走 name，壳上不携带任何派生身份。
    let target_name = match config_object.get("target") {
        None => {
            issues.push(DockIssue::new(
                "D003",
                format!("{path}.target"),
                "target is required: {zhixu: <target definition metadata.name>} for a static target, or null for runtime selection",
            ));
            None
        }
        Some(Value::Null) => None,
        Some(Value::Object(target)) => {
            for key in target.keys() {
                if key != "zhixu" {
                    issues.push(DockIssue::new(
                        "D002",
                        format!("{path}.target.{key}"),
                        format!("unknown field {key:?}; allowed: [\"zhixu\"]"),
                    ));
                }
            }
            match target.get("zhixu").and_then(Value::as_str) {
                Some(name) => {
                    let name = name.trim();
                    if !crate::validate::is_name_slug(name) {
                        issues.push(DockIssue::new(
                            "D003",
                            format!("{path}.target.zhixu"),
                            format!(
                                "target.zhixu must be the target definition's metadata.name, matching ^[a-z][a-z0-9_-]{{0,99}}$, found {name:?}; {UNSUPPORTED_HINT}"
                            ),
                        ));
                    }
                    Some(name.to_string())
                }
                None => {
                    issues.push(DockIssue::new(
                        "D003",
                        format!("{path}.target.zhixu"),
                        "target.zhixu is required when target is an object (the target definition metadata.name)",
                    ));
                    None
                }
            }
        }
        Some(_) => {
            issues.push(DockIssue::new(
                "D003",
                format!("{path}.target"),
                "target must be an object {zhixu: <target definition metadata.name>} or null (dynamic selection)",
            ));
            None
        }
    };

    // D002：interface 必填，接口名与端口名同规则。
    let interface_name = config_object
        .get("interface")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    if interface_name.is_empty() {
        issues.push(DockIssue::new(
            "D002",
            format!("{path}.interface"),
            "interface is required (the target interface name)",
        ));
    } else if !valid_port_name(&interface_name) {
        issues.push(DockIssue::new(
            "D002",
            format!("{path}.interface"),
            format!(
                "interface must match ^[a-z][a-z0-9_]{{0,31}}$, found {interface_name:?}; {UNSUPPORTED_HINT}"
            ),
        ));
    }

    // D004：order.mode 闭集 {new, existing}。
    let order_mode = match config_object.get("order") {
        Some(Value::Object(order)) => {
            for key in order.keys() {
                if key != "mode" {
                    issues.push(DockIssue::new(
                        "D002",
                        format!("{path}.order.{key}"),
                        format!("unknown field {key:?}; allowed: [\"mode\"]"),
                    ));
                }
            }
            order
                .get("mode")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .trim()
                .to_string()
        }
        _ => {
            issues.push(DockIssue::new(
                "D004",
                format!("{path}.order.mode"),
                "order.mode is required and must be \"new\" or \"existing\"",
            ));
            String::new()
        }
    };
    if !issues
        .iter()
        .any(|issue| issue.path == format!("{path}.order.mode"))
        && !ORDER_MODES.contains(&order_mode.as_str())
    {
        issues.push(DockIssue::new(
            "D004",
            format!("{path}.order.mode"),
            format!("must be \"new\" or \"existing\", found {order_mode:?}"),
        ));
    }

    let mut parse_map = |key: &str, code: &'static str| -> Option<Map<String, Value>> {
        match config_object.get(key) {
            None => Some(Map::new()),
            Some(Value::Object(map)) => Some(map.clone()),
            Some(_) => {
                issues.push(DockIssue::new(
                    code,
                    format!("{path}.{key}"),
                    format!("{key} must be an object mapping local channels to target port names"),
                ));
                None
            }
        }
    };
    let input_map = parse_map("inputMap", "D005").unwrap_or_default();
    let signal_map = parse_map("signalMap", "D006").unwrap_or_default();

    // D005：inputMap key 必须是本地 receiveSignals 通道；value 必须是合法端口名。
    let mut parsed_input = BTreeMap::new();
    for (hook_name, port) in &input_map {
        if !stage.receive_signals.contains_key(hook_name) {
            issues.push(DockIssue::new(
                "D005",
                format!("{path}.inputMap.{hook_name}"),
                format!("key is not a receiveSignals channel of stage {stage_identifier}"),
            ));
            continue;
        }
        let Some(port_name) = port.as_str() else {
            issues.push(DockIssue::new(
                "D005",
                format!("{path}.inputMap.{hook_name}"),
                "value must be a target input port name string",
            ));
            continue;
        };
        if !valid_port_name(port_name) {
            issues.push(DockIssue::new(
                "D005",
                format!("{path}.inputMap.{hook_name}"),
                format!(
                    "value must be a port name matching ^[a-z][a-z0-9_]{{0,31}}$, found {port_name:?}; {UNSUPPORTED_HINT}"
                ),
            ));
            continue;
        }
        parsed_input.insert(hook_name.clone(), port_name.to_string());
    }
    // 同一目标端口在一次 inputMap 中只能绑定一次。
    let mut ports_seen = BTreeSet::new();
    for (hook_name, port) in &parsed_input {
        if !ports_seen.insert(port.clone()) {
            issues.push(DockIssue::new(
                "D005",
                format!("{path}.inputMap.{hook_name}"),
                format!("target input port {port:?} is bound more than once in this route"),
            ));
        }
    }

    // D006：signalMap key 必须是本地 send signal。key 同时是运行期 hook
    // 命名空间：'.' 是信号名分隔符、组合长度受 signal_name 列宽约束
    // （与 Go 镜像 zhixu_schema.go 同款校验）。
    let mut parsed_signal = BTreeMap::new();
    for (signal_name, port) in &signal_map {
        if signal_name.contains('.') || signal_name.len() > MAX_SIGNAL_MAP_KEY_LENGTH {
            issues.push(DockIssue::new(
                "D006",
                format!("{path}.signalMap.{signal_name}"),
                format!(
                    "key must not contain '.' and must be at most {MAX_SIGNAL_MAP_KEY_LENGTH} bytes"
                ),
            ));
            continue;
        }
        if stage_identifier.len() + 1 + signal_name.len() > MAX_SIGNAL_NAME_BYTES {
            issues.push(DockIssue::new(
                "D006",
                format!("{path}.signalMap.{signal_name}"),
                format!(
                    "stage {stage_identifier:?} combined signal name is {} bytes, exceeds {MAX_SIGNAL_NAME_BYTES} (individual_record.signal_name)",
                    stage_identifier.len() + 1 + signal_name.len()
                ),
            ));
            continue;
        }
        // D006 存在性与引用面同口径：按展开后的全名比较（crate::
        // declares_signal_expanding_to）——裸名 key 展开为
        // <task>.<stage>.<key>，canonical 显式自指声明的信号即可被裸名
        // key 引用命中；裸名精确匹配会把 canonical 声明判成"声明即不可
        // 投递"。
        if !crate::validate::declares_signal_expanding_to(
            &stage.send_signals,
            stage_identifier,
            &format!("{stage_identifier}.{signal_name}"),
        ) {
            issues.push(DockIssue::new(
                "D006",
                format!("{path}.signalMap.{signal_name}"),
                format!("key is not a sendSignals signal of stage {stage_identifier}"),
            ));
            continue;
        }
        let Some(port_name) = port.as_str() else {
            issues.push(DockIssue::new(
                "D006",
                format!("{path}.signalMap.{signal_name}"),
                "value must be a target output port name string",
            ));
            continue;
        };
        if !valid_port_name(port_name) {
            issues.push(DockIssue::new(
                "D006",
                format!("{path}.signalMap.{signal_name}"),
                format!(
                    "value must be a port name matching ^[a-z][a-z0-9_]{{0,31}}$, found {port_name:?}; {UNSUPPORTED_HINT}"
                ),
            ));
            continue;
        }
        parsed_signal.insert(signal_name.clone(), port_name.to_string());
    }
    let mut output_ports_seen = BTreeSet::new();
    for (signal_name, port) in &parsed_signal {
        if !output_ports_seen.insert(port.clone()) {
            issues.push(DockIssue::new(
                "D006",
                format!("{path}.signalMap.{signal_name}"),
                format!("target output port {port:?} is bound more than once in this route"),
            ));
        }
    }

    // D019：至少声明一项输入或输出映射。
    if parsed_input.is_empty() && parsed_signal.is_empty() {
        issues.push(DockIssue::new(
            "D019",
            &path,
            "at least one of inputMap/signalMap must bind a port (a route maps an input or an output; business str/cmp are not required)",
        ));
    }

    // D010：new 模式恰好一条 input 绑定（建单入口需要确定的出生锚）。
    if order_mode == ORDER_MODE_NEW && parsed_input.len() != 1 {
        issues.push(DockIssue::new(
            "D010",
            format!("{path}.inputMap"),
            format!(
                "order.mode new requires exactly one inputMap binding (the birth anchor), found {}",
                parsed_input.len()
            ),
        ));
    }

    // D016：binding 数量上限在声明面（parse 期）钉死——target:null 的动态
    // 选择 route 不进 link，上限若只放在 link 期会被未解析 route 绕过。
    if parsed_input.len() > MAX_DOCK_INPUTS {
        issues.push(DockIssue::new(
            "D016",
            format!("{path}.inputMap"),
            format!(
                "route binds {} input ports, limit is {MAX_DOCK_INPUTS}",
                parsed_input.len()
            ),
        ));
    }
    if parsed_signal.len() > MAX_DOCK_OUTPUTS {
        issues.push(DockIssue::new(
            "D016",
            format!("{path}.signalMap"),
            format!(
                "route binds {} output ports, limit is {MAX_DOCK_OUTPUTS}",
                parsed_signal.len()
            ),
        ));
    }

    if !issues.is_empty() {
        return Err(issues);
    }
    Ok(ZhixuExecutorConfig {
        target_name,
        interface_name,
        order_mode,
        input_map: parsed_input,
        signal_map: parsed_signal,
    })
}

pub fn valid_port_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    if bytes.is_empty() || bytes.len() > MAX_PORT_NAME_BYTES {
        return false;
    }
    if !bytes[0].is_ascii_lowercase() {
        return false;
    }
    bytes[1..]
        .iter()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'_')
}

/// orderModes 闭集校验：{new, existing} 的非空子集，无重复。
fn valid_order_modes(modes: &[String]) -> bool {
    if modes.is_empty() {
        return false;
    }
    let mut seen = BTreeSet::new();
    modes
        .iter()
        .all(|mode| ORDER_MODES.contains(&mode.as_str()) && seen.insert(mode.as_str()))
}

fn is_zhixu_executor(executor: &Option<uvp_model::ZhixuExecutor>) -> bool {
    // 精确比较：supplierType 闭集（含空白变体拒绝）已在 validate_zhixu_shape
    // 把关，此处不留 trim 容忍（单一口径）。
    executor
        .as_ref()
        .is_some_and(|e| e.supplier_type == "zhixu")
}

/// 未链接 route：本地编译产物（调用方侧）。
#[derive(Debug, Clone)]
pub struct UnlinkedDockRoute {
    pub stage_identifier: String,
    /// 本地 stage source：未解析 route 声明面的 localSource（运行期按
    /// source 维度投递，须随声明面携带）。
    pub stage_source: String,
    pub config: ZhixuExecutorConfig,
}

/// 收集并本地校验一个定义内全部 zhixu executor route（不解析目标端口）。
/// 非 zhixu executor 携带 `zhixuExecutorConfig` 在此响亮拒绝（D001 同罪：
/// "既静态执行者又委托对接"的矛盾键组合不得静默烧进 executorRoutes 承诺）。
pub fn collect_unlinked_routes(
    entries: &[(String, ZhixuStage)],
) -> DockResult<Vec<UnlinkedDockRoute>> {
    let mut routes = Vec::new();
    let mut issues = Vec::new();
    for (stage_identifier, stage) in entries {
        let Some(executor) = &stage.executor else {
            continue;
        };
        if !is_zhixu_executor(&stage.executor) {
            if executor.zhixu_executor_config.is_some() {
                issues.push(DockIssue::new(
                    "D002",
                    format!("{stage_identifier}.executor.zhixuExecutorConfig"),
                    format!(
                        "zhixuExecutorConfig is only valid when supplierType is zhixu, found {:?}; a static executor cannot also declare delegation (same contract as D001's misplaced supplierID)",
                        executor.supplier_type
                    ),
                ));
            }
            continue;
        }
        let executor_value =
            serde_json::to_value(executor).unwrap_or_else(|_| Value::Object(Map::new()));
        match parse_zhixu_executor_config(&executor_value, stage, stage_identifier) {
            Ok(config) => routes.push(UnlinkedDockRoute {
                stage_identifier: stage_identifier.clone(),
                stage_source: stage.source.clone(),
                config,
            }),
            Err(mut stage_issues) => issues.append(&mut stage_issues),
        }
    }
    if !issues.is_empty() {
        return Err(issues);
    }
    Ok(routes)
}

impl UnlinkedDockRoute {
    /// 未解析 route 的声明面产物：本地声明完整、不携带
    /// 任何派生字段。target:null（动态选择）的目标空缺——目标身份/承诺
    /// 由各轨在选择记录补齐后自行计算；静态目标 route 携带作者声明的
    /// target.zhixu（name 引用，非派生身份），与动态目标对称进声明面。
    pub fn unresolved_json(&self) -> Value {
        let mut route = json!({
            "schemaVersion": DOCK_ROUTE_UNRESOLVED_SCHEMA_VERSION,
            "stageIdentifier": self.stage_identifier,
            "localSource": self.stage_source,
            "interfaceName": self.config.interface_name,
            "orderMode": self.config.order_mode,
            "inputBindings": self.config.input_map.iter().map(|(hook_name, port)| json!({
                "hookId": format!("{}#{hook_name}", self.stage_identifier),
                "port": port,
            })).collect::<Vec<_>>(),
            "outputBindings": self.config.signal_map.iter().map(|(signal_name, port)| json!({
                "signal": signal_name,
                "port": port,
            })).collect::<Vec<_>>(),
        });
        if let Some(target_name) = &self.config.target_name {
            route["target"] = json!({ "zhixu": target_name });
        }
        route
    }
}

// ---------------------------------------------------------------------------
// 目标接口（spec.dockInterface → 中性接口声明）
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct InterfacePortInput {
    pub port: String,
    /// 输入端口所属 stage 的 source 类（单源 seam 的 input 侧观测面）。
    /// hook 引用 `<task>.<stage>#<receiveHookName>` 本身不携带 source——
    /// 中性声明补 source 兄弟键后，linker 才能对 input 与 output 两侧
    /// 执行同一单源校验（文法 §4.2）。
    pub source: String,
    /// `<task>.<stage>#<receiveHookName>`
    pub hook: String,
}

#[derive(Debug, Clone)]
pub struct InterfacePortOutput {
    pub port: String,
    /// `<source>::<task>.<stage>.<signal>`
    pub signal: String,
}

/// 一个具名接口的中性声明（与 resolution manifest 的接口元素同形状）。
#[derive(Debug, Clone)]
pub struct InterfaceDeclaration {
    pub name: String,
    pub order_modes: Vec<String>,
    pub inputs: Vec<InterfacePortInput>,
    pub outputs: Vec<InterfacePortOutput>,
}

/// 编译本地 `spec.dockInterface` 的产物：中性接口声明 + entrance 出生
/// 事实键（U2 出生通道键并集查重的 dock 侧输入）。entrance 键只从本地
/// 接口编译收集——manifest 侧接口是远端目标的声明面，不参与本 plan 的
/// 出生通道键并集。
#[derive(Debug, Clone)]
pub struct CompiledInterfaces {
    pub declarations: Vec<InterfaceDeclaration>,
    /// (source, task.stage.signal) → 发布该出生键的 entrance input 端口
    /// 路径（`<interface>.<port>`；同键被多个端口发布时逐一列出）。
    pub entrance_fact_keys: BTreeMap<(String, String), Vec<String>>,
}

impl InterfaceDeclaration {
    fn to_json(&self) -> Value {
        let inputs = Map::from_iter(self.inputs.iter().map(|port| {
            (
                port.port.clone(),
                json!({ "source": port.source, "hook": port.hook }),
            )
        }));
        let outputs = Map::from_iter(
            self.outputs
                .iter()
                .map(|port| (port.port.clone(), json!({ "signal": port.signal }))),
        );
        json!({
            "name": self.name,
            "orderModes": self.order_modes,
            "inputs": inputs,
            "outputs": outputs,
        })
    }
}

/// 编译目标定义的 `spec.dockInterface`（D013/D014、D021/D022、D025）。
/// 产物是中性声明：接口名/orderModes/inputs/outputs 原文，不含任何
/// 哈希或身份字段；entrance 端口（orderModes 含 new 的接口的 input
/// 端口）的 atom 事实键随产物携带，供出生通道键并集查重消费。
pub fn compile_dock_interface(
    dock: &BTreeMap<String, DockInterfaceSpec>,
    entries: &[(String, ZhixuStage)],
) -> DockResult<CompiledInterfaces> {
    let mut issues = Vec::new();

    let stages_by_identifier: BTreeMap<&str, &ZhixuStage> = entries
        .iter()
        .map(|(identifier, stage)| (identifier.as_str(), stage))
        .collect();
    // mailbox hook 全定义唯一发布：同一物理入口被两个公开端口重复发布会
    // 让外部投递出现两条可寻址路径。
    let mut hooks_claimed: BTreeMap<String, String> = BTreeMap::new();
    let mut interfaces = Vec::new();
    let mut entrance_fact_keys: BTreeMap<(String, String), Vec<String>> = BTreeMap::new();

    for (interface_name, spec) in dock {
        let base_path = format!("spec.dockInterface.{interface_name}");
        if !valid_port_name(interface_name) {
            issues.push(DockIssue::new(
                "D021",
                &base_path,
                "interface name must match ^[a-z][a-z0-9_]{0,31}$ (same rule as port names)",
            ));
            continue;
        }
        if !valid_order_modes(&spec.order_modes) {
            issues.push(DockIssue::new(
                "D025",
                format!("{base_path}.orderModes"),
                "orderModes must be a non-empty subset of {new, existing} without duplicates",
            ));
            continue;
        }
        // D016（声明面计数闸）：单接口端口总数（inputs+outputs）上限。
        if spec.inputs.len() + spec.outputs.len() > MAX_INTERFACE_PORTS {
            issues.push(DockIssue::new(
                "D016",
                &base_path,
                format!(
                    "interface exposes {} ports ({} inputs + {} outputs), limit is {MAX_INTERFACE_PORTS}",
                    spec.inputs.len() + spec.outputs.len(),
                    spec.inputs.len(),
                    spec.outputs.len()
                ),
            ));
            continue;
        }

        let mut inputs = Vec::new();
        let mut outputs = Vec::new();
        // entrance 端口 atom 的事实键（接口级收集；仅在接口 orderModes 含
        // new 时并入出生键集合——与 entrance_hook_ids 同一判定）。
        let mut entrance_atoms: Vec<(String, String, String)> = Vec::new();

        for (port_name, port) in &spec.inputs {
            let path = format!("{base_path}.inputs.{port_name}");
            if !valid_port_name(port_name) {
                issues.push(DockIssue::new(
                    "D021",
                    &path,
                    "port name must match ^[a-z][a-z0-9_]{0,31}$",
                ));
                continue;
            }
            // D022：hook 引用 + 真实存在。
            let Some((stage_identifier, hook_name)) = parse_hook_reference(&port.hook) else {
                issues.push(DockIssue::new(
                    "D022",
                    format!("{path}.hook"),
                    format!(
                        "hook must be <task>.<stage>#<receiveHookName>, found {:?}",
                        port.hook
                    ),
                ));
                continue;
            };
            let Some(stage) = stages_by_identifier.get(stage_identifier.as_str()).copied() else {
                issues.push(DockIssue::new(
                    "D022",
                    format!("{path}.hook"),
                    format!("references unknown stage {stage_identifier}"),
                ));
                continue;
            };
            // input 端口的中性声明携带所属 stage 的 source 类（单源 seam 的
            // input 侧观测面）：hook 引用本身无 source 维度，无法派生即响亮
            // 失败，不留静默空串兜底。
            if stage.source.trim().is_empty() {
                issues.push(DockIssue::new(
                    "D022",
                    format!("{path}.hook"),
                    format!(
                        "references stage {stage_identifier} which declares no source: the input port source cannot be derived"
                    ),
                ));
                continue;
            }
            let Some(raw_expression) = stage.receive_signals.get(&hook_name) else {
                issues.push(DockIssue::new(
                    "D022",
                    format!("{path}.hook"),
                    format!("stage {stage_identifier} has no receiveSignals hook {hook_name}"),
                ));
                continue;
            };
            if let Some(previous) = hooks_claimed.get(&port.hook) {
                issues.push(DockIssue::new(
                    "D022",
                    format!("{path}.hook"),
                    format!("hook {} is already published by port {previous}", port.hook),
                ));
                continue;
            }
            hooks_claimed.insert(port.hook.clone(), format!("{interface_name}.{port_name}"));

            let parsed = match parse_hook(ParseHookRequest {
                profile: Profile::EvmStrict,
                hook_name: hook_name.clone(),
                hook: raw_expression.clone(),
            }) {
                Ok(parsed) => parsed,
                Err(err) => {
                    issues.push(DockIssue::new(
                        "D013",
                        format!("{stage_identifier}.receiveSignals.{hook_name}"),
                        format!("input port hook expression is invalid: {err}"),
                    ));
                    continue;
                }
            };
            // D013：恰好一个正向 canonical signal atom；禁止组合/否定/计时/订阅。
            // atom 信号不要求 ∈ sendSignals——它是 dock 注入的输入事实。
            // 判定看去重前的语法结构（AST 根即 Signal 节点）：依赖列表是
            // 去重后的产物，A&A / A|A 会把组合式伪装成"单依赖"绕过本闸。
            let single_atom = parsed
                .ast
                .get("condition")
                .and_then(|condition| condition.get("kind"))
                .and_then(Value::as_str)
                .is_some_and(|kind| kind == "signal");
            if !single_atom {
                issues.push(DockIssue::new(
                    "D013",
                    format!("{stage_identifier}.receiveSignals.{hook_name}"),
                    "input port hook must be exactly one positive canonical signal atom (no &, |, ~, timers, aggregation, or ANCHOR)",
                ));
                continue;
            }
            let dependency = &parsed.dependencies[0];
            if dependency.source != stage.source {
                issues.push(DockIssue::new(
                    "D013",
                    format!("{stage_identifier}.receiveSignals.{hook_name}"),
                    format!(
                        "input port atom source {} must equal the owning stage source {}",
                        dependency.source, stage.source
                    ),
                ));
                continue;
            }
            // atom 的 (task, stage) 必须落在所属 stage 上：mailbox 地址不可指向别处。
            if !dependency
                .signal_name
                .starts_with(&format!("{stage_identifier}."))
            {
                issues.push(DockIssue::new(
                    "D013",
                    format!("{stage_identifier}.receiveSignals.{hook_name}"),
                    format!(
                        "input port atom must address the owning stage {stage_identifier}, found {}",
                        dependency.signal_name
                    ),
                ));
                continue;
            }
            entrance_atoms.push((
                port_name.clone(),
                dependency.source.clone(),
                dependency.signal_name.clone(),
            ));

            inputs.push(InterfacePortInput {
                port: port_name.clone(),
                source: stage.source.clone(),
                hook: port.hook.clone(),
            });
        }

        for (port_name, port) in &spec.outputs {
            let path = format!("{base_path}.outputs.{port_name}");
            if !valid_port_name(port_name) {
                issues.push(DockIssue::new(
                    "D021",
                    &path,
                    "port name must match ^[a-z][a-z0-9_]{0,31}$",
                ));
                continue;
            }
            // D014：真实 send capability。
            let Some((source, stage_identifier, signal_name)) =
                parse_canonical_signal(&port.signal)
            else {
                issues.push(DockIssue::new(
                    "D014",
                    format!("{path}.signal"),
                    format!(
                        "signal must be <source>::<task>.<stage>.<signal>, found {:?}",
                        port.signal
                    ),
                ));
                continue;
            };
            let Some(stage) = stages_by_identifier.get(stage_identifier.as_str()).copied() else {
                issues.push(DockIssue::new(
                    "D014",
                    format!("{path}.signal"),
                    format!("references unknown stage {stage_identifier}"),
                ));
                continue;
            };
            if stage.source != source {
                issues.push(DockIssue::new(
                    "D014",
                    format!("{path}.signal"),
                    format!(
                        "signal source {source} must equal stage {stage_identifier} source {}",
                        stage.source
                    ),
                ));
                continue;
            }
            // D014 的存在性按展开后的全名统一比较（crate::
            // declares_signal_expanding_to）：裸名声明展开为
            // <task>.<stage>.<signal>，canonical 三段式（强制自指）本身即
            // 全名——只比第三段会把 canonical 声明误判为悬空引用。
            let full_signal_name = format!("{stage_identifier}.{signal_name}");
            if !crate::validate::declares_signal_expanding_to(
                &stage.send_signals,
                &stage_identifier,
                &full_signal_name,
            ) {
                issues.push(DockIssue::new(
                    "D014",
                    format!("{path}.signal"),
                    format!("signal {signal_name} is not in stage {stage_identifier} sendSignals"),
                ));
                continue;
            }

            outputs.push(InterfacePortOutput {
                port: port_name.clone(),
                signal: port.signal.clone(),
            });
        }

        // D025：new ∈ orderModes ⇒ 至少一个 input 端口（建单型服务必须有入口）。
        if spec.order_modes.iter().any(|m| m == ORDER_MODE_NEW) && inputs.is_empty() {
            issues.push(DockIssue::new(
                "D025",
                format!("{base_path}.inputs"),
                "an interface supporting order mode new must expose at least one input port (the birth anchor)",
            ));
            continue;
        }
        if spec.order_modes.iter().any(|m| m == ORDER_MODE_NEW) {
            for (port_name, atom_source, atom_signal) in entrance_atoms {
                entrance_fact_keys
                    .entry((atom_source, atom_signal))
                    .or_default()
                    .push(format!("{interface_name}.{port_name}"));
            }
        }

        interfaces.push(InterfaceDeclaration {
            name: interface_name.clone(),
            order_modes: spec.order_modes.clone(),
            inputs,
            outputs,
        });
    }

    if !issues.is_empty() {
        return Err(issues);
    }
    // 接口按名排序（BTreeMap 迭代已按名升序，此处显式钉住口径）。
    interfaces.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(CompiledInterfaces {
        declarations: interfaces,
        entrance_fact_keys,
    })
}

/// 目标定义全部接口的中性声明产物（接口名升序数组）。
pub fn interface_declarations_json(interfaces: &[InterfaceDeclaration]) -> Value {
    Value::Array(
        interfaces
            .iter()
            .map(InterfaceDeclaration::to_json)
            .collect(),
    )
}

/// 全部 input 端口引用的本地 hook 集合（`<task>.<stage>#<hook>`）：
/// 这些 mailbox hook 不走普通依赖引用校验。
pub fn input_port_hook_ids(interfaces: &[InterfaceDeclaration]) -> BTreeSet<String> {
    interfaces
        .iter()
        .flat_map(|interface| interface.inputs.iter())
        .map(|port| port.hook.clone())
        .collect()
}

/// 可作为 new 模式出生锚的 input 端口（orderModes 含 new 的接口的全部
/// input 端口——new 模式 route 的唯一 input 绑定可落在其中任意一个）
/// 引用的本地 hook 集合，供 orderTriggerKind=dock 标记使用。
pub fn entrance_hook_ids(interfaces: &[InterfaceDeclaration]) -> BTreeSet<String> {
    interfaces
        .iter()
        .filter(|interface| interface.order_modes.iter().any(|m| m == ORDER_MODE_NEW))
        .flat_map(|interface| interface.inputs.iter())
        .map(|port| port.hook.clone())
        .collect()
}

fn parse_hook_reference(reference: &str) -> Option<(String, String)> {
    let (stage_part, hook_name) = reference.split_once('#')?;
    if hook_name.is_empty() || hook_name.contains('#') || hook_name.contains('.') {
        return None;
    }
    let parts: Vec<&str> = stage_part.split('.').collect();
    if parts.len() != 2 || parts[0].is_empty() || parts[1].is_empty() {
        return None;
    }
    Some((stage_part.to_string(), hook_name.to_string()))
}

fn parse_canonical_signal(signal: &str) -> Option<(String, String, String)> {
    let (source, rest) = signal.split_once("::")?;
    let parts: Vec<&str> = rest.split('.').collect();
    // 空段（"buyer::main..cmp"、"buyer::main.stage."）不是合法 canonical
    // 形态：拼出的空 stage/空 signal 是永不匹配的寻址键，按形态错误拒绝
    // （与 Go 镜像 isCanonicalSignalShape 的非空段校验同口径）。
    if parts.len() != 3 || source.is_empty() || parts.iter().any(|part| part.is_empty()) {
        return None;
    }
    Some((
        source.to_string(),
        format!("{}.{}", parts[0], parts[1]),
        parts[2].to_string(),
    ))
}

// ---------------------------------------------------------------------------
// Resolution manifest + linker
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct ResolutionTarget {
    pub name: String,
    pub interfaces: Vec<InterfaceDeclaration>,
    /// 该定义声明的静态 dock 出边（目标定义 name）。linker 用它做 D015
    /// 环/深度检测；缺省 = 无出边，运行时建立的边由各轨解析面兜底。
    pub dock_edges: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct ResolutionManifest {
    pub targets: Vec<ResolutionTarget>,
}

/// 解析 resolution manifest（Store/发布系统或离线 lock 文件提供）。
/// manifest 是中性 name 目录：name→interfaces 是 linker 的解析面（manifest
/// 内 name 重名是发布方数据错误，响亮拒绝；name 到实体的解析权威在各
/// 轨），可选 dockEdges 声明目标自身的静态出边供启动图检测。
pub fn parse_resolution_manifest(value: &Value) -> DockResult<ResolutionManifest> {
    let mut issues = Vec::new();
    let schema = value
        .get("schemaVersion")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if schema != DOCK_RESOLUTION_SCHEMA_VERSION {
        issues.push(DockIssue::new(
            "D008",
            "resolutionManifest.schemaVersion",
            format!("must be \"{DOCK_RESOLUTION_SCHEMA_VERSION}\", found {schema:?}"),
        ));
        return Err(issues);
    }
    let mut targets = Vec::new();
    let mut names_seen = BTreeSet::new();
    let definitions = value
        .get("definitions")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    // D008（manifest 计数闸）：definitions 条目数上限——发布方数据错误
    // 的规模面在解析期收口，不给 link 期留下无界图。
    if definitions.len() > MAX_MANIFEST_DEFINITIONS {
        issues.push(DockIssue::new(
            "D008",
            "resolutionManifest.definitions",
            format!(
                "manifest carries {} definitions, limit is {MAX_MANIFEST_DEFINITIONS}",
                definitions.len()
            ),
        ));
        return Err(issues);
    }
    for (index, entry) in definitions.iter().enumerate() {
        let path = format!("resolutionManifest.definitions[{index}]");
        // 条目级键闭集与 interface/dockEdges 闸口同口径：拼错的字段（如
        // interface 单数、dockEdgess）被静默吸收会让发布方数据错误以缺省
        // 语义参与 link。
        if let Some(entry_object) = entry.as_object() {
            for key in entry_object.keys() {
                if !matches!(key.as_str(), "name" | "dockEdges" | "interfaces") {
                    issues.push(DockIssue::new(
                        "D008",
                        format!("{path}.{key}"),
                        format!(
                            "unknown field {key:?}; allowed: [\"name\", \"dockEdges\", \"interfaces\"]"
                        ),
                    ));
                }
            }
        }
        let name = entry
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_string();
        if name.is_empty() {
            issues.push(DockIssue::new(
                "D008",
                format!("{path}.name"),
                "name is required (the target definition metadata.name)",
            ));
            continue;
        }
        if !crate::validate::is_name_slug(&name) {
            issues.push(DockIssue::new(
                "D008",
                format!("{path}.name"),
                format!(
                    "name must be the target definition metadata.name, matching ^[a-z][a-z0-9_-]{{0,99}}$, found {name:?}"
                ),
            ));
            continue;
        }
        if !names_seen.insert(name.clone()) {
            issues.push(DockIssue::new(
                "D008",
                &path,
                format!("duplicate definition name {name:?}: names are the resolution key and must be unique in the manifest"),
            ));
            continue;
        }
        // 可选 dockEdges：[{target: <definition name>}]，纯 name 出边。
        // 缺省视为无出边；形状/残留键非法按 D008 响亮拒绝。
        let mut dock_edges = Vec::new();
        match entry.get("dockEdges") {
            None => {}
            Some(Value::Array(edge_values)) => {
                for (edge_index, edge) in edge_values.iter().enumerate() {
                    let edge_path = format!("{path}.dockEdges[{edge_index}]");
                    let Some(edge_object) = edge.as_object() else {
                        issues.push(DockIssue::new(
                            "D008",
                            &edge_path,
                            "dock edge must be an object {target: <definition name>}",
                        ));
                        continue;
                    };
                    for key in edge_object.keys() {
                        if key != "target" {
                            issues.push(DockIssue::new(
                                "D008",
                                format!("{edge_path}.{key}"),
                                format!("unknown field {key:?}; allowed: [\"target\"]"),
                            ));
                        }
                    }
                    let edge_target = edge_object
                        .get("target")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .trim()
                        .to_string();
                    if !crate::validate::is_name_slug(&edge_target) {
                        issues.push(DockIssue::new(
                            "D008",
                            format!("{edge_path}.target"),
                            format!(
                                "target must be a definition metadata.name, matching ^[a-z][a-z0-9_-]{{0,99}}$, found {edge_target:?}"
                            ),
                        ));
                        continue;
                    }
                    dock_edges.push(edge_target);
                }
            }
            Some(_) => {
                issues.push(DockIssue::new(
                    "D008",
                    format!("{path}.dockEdges"),
                    "dockEdges must be an array of {target: <definition name>}",
                ));
            }
        }
        let mut interfaces = Vec::new();
        let mut interfaces_valid = true;
        for (interface_index, interface_value) in entry
            .get("interfaces")
            .and_then(Value::as_array)
            .unwrap_or(&Vec::new())
            .iter()
            .enumerate()
        {
            let interface_path = format!("{path}.interfaces[{interface_index}]");
            match parse_interface_declaration(interface_value) {
                Ok(interface) => interfaces.push(interface),
                Err(mut interface_issues) => {
                    for issue in &mut interface_issues {
                        issue.path = format!("{interface_path}.{}", issue.path);
                    }
                    issues.append(&mut interface_issues);
                    interfaces_valid = false;
                }
            }
        }
        if !interfaces_valid {
            continue;
        }
        if interfaces.is_empty() {
            issues.push(DockIssue::new(
                "D008",
                format!("{path}.interfaces"),
                "target must publish at least one named interface",
            ));
            continue;
        }
        let mut interface_names = BTreeSet::new();
        for interface in &interfaces {
            if !interface_names.insert(interface.name.clone()) {
                issues.push(DockIssue::new(
                    "D008",
                    format!("{path}.interfaces.{}", interface.name),
                    format!("duplicate interface name {:?}", interface.name),
                ));
            }
        }
        if !issues.is_empty() {
            continue;
        }
        targets.push(ResolutionTarget {
            name,
            interfaces,
            dock_edges,
        });
    }
    if !issues.is_empty() {
        return Err(issues);
    }
    Ok(ResolutionManifest { targets })
}

fn parse_interface_declaration(value: &Value) -> DockResult<InterfaceDeclaration> {
    let mut issues = Vec::new();
    let Some(object) = value.as_object() else {
        return Err(vec![DockIssue::new(
            "D008",
            "",
            "interface must be an object",
        )]);
    };
    // 键闭集：拼错的字段不得被静默忽略成零值语义。
    for key in object.keys() {
        if !matches!(key.as_str(), "name" | "orderModes" | "inputs" | "outputs") {
            issues.push(DockIssue::new(
                "D008",
                "",
                format!("unknown field {key:?}; allowed: [\"name\", \"orderModes\", \"inputs\", \"outputs\"]"),
            ));
        }
    }
    let name = object
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    if !valid_port_name(&name) {
        issues.push(DockIssue::new(
            "D008",
            "name",
            format!("interface name must match ^[a-z][a-z0-9_]{{0,31}}$, found {name:?}"),
        ));
    }
    // orderModes：数组形态 + 字符串项都按确定性非法输入响亮拒绝——
    // filter_map 吞掉非字符串项会让 ["new", 123] 静默解析成 ["new"]，
    // 发布方数据错误以缺省语义参与 link。
    let mut order_modes = Vec::new();
    match object.get("orderModes") {
        None => {}
        Some(Value::Array(modes)) => {
            for mode in modes {
                match mode.as_str() {
                    Some(mode) => order_modes.push(mode.to_string()),
                    None => issues.push(DockIssue::new(
                        "D008",
                        "orderModes",
                        format!("orderModes entries must be strings, found {mode}"),
                    )),
                }
            }
        }
        Some(other) => issues.push(DockIssue::new(
            "D008",
            "orderModes",
            format!("orderModes must be an array of strings, found {other}"),
        )),
    }
    if !valid_order_modes(&order_modes) {
        issues.push(DockIssue::new(
            "D008",
            "orderModes",
            "orderModes must be a non-empty subset of {new, existing} without duplicates",
        ));
    }

    let mut inputs = Vec::new();
    // inputs/outputs 的类型非法（字符串、数组等）是确定性发布方数据错误，
    // 响亮拒绝——as_object().unwrap_or(&Map::new()) 会把 "inputs":
    // "OOPS" 静默吞成空 map，link 按"无端口"继续。
    let mut input_ports: Option<&Map<String, Value>> = None;
    match object.get("inputs") {
        None => {}
        Some(Value::Object(ports)) => input_ports = Some(ports),
        Some(other) => issues.push(DockIssue::new(
            "D008",
            "inputs",
            format!(
                "inputs must be an object mapping port names to {{source, hook}}, found {other}"
            ),
        )),
    }
    for (port_name, port) in input_ports.into_iter().flatten() {
        let port_path = format!("inputs.{port_name}");
        if !valid_port_name(port_name) {
            issues.push(DockIssue::new(
                "D008",
                &port_path,
                "port name must match ^[a-z][a-z0-9_]{0,31}$",
            ));
            continue;
        }
        let Some(port_object) = port.as_object() else {
            issues.push(DockIssue::new(
                "D008",
                &port_path,
                "input port must be an object {source, hook}",
            ));
            continue;
        };
        for key in port_object.keys() {
            if !matches!(key.as_str(), "source" | "hook") {
                issues.push(DockIssue::new(
                    "D008",
                    format!("{port_path}.{key}"),
                    format!("unknown field {key:?}; allowed: [\"source\", \"hook\"]"),
                ));
            }
        }
        // source 必填（单源 seam 的 input 侧观测面）：缺失/空白即响亮失败，
        // 不回退、不臆造——linker 的双侧单源校验依赖该字段。
        let source = port_object
            .get("source")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_string();
        if source.is_empty() {
            issues.push(DockIssue::new(
                "D008",
                format!("{port_path}.source"),
                "source is required (the owning stage's source class); it must not be blank",
            ));
            continue;
        }
        let hook = port_object
            .get("hook")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if parse_hook_reference(hook).is_none() {
            issues.push(DockIssue::new(
                "D008",
                format!("{port_path}.hook"),
                format!("hook must be <task>.<stage>#<receiveHookName>, found {hook:?}"),
            ));
            continue;
        }
        inputs.push(InterfacePortInput {
            port: port_name.clone(),
            source,
            hook: hook.to_string(),
        });
    }

    let mut outputs = Vec::new();
    let mut output_ports: Option<&Map<String, Value>> = None;
    match object.get("outputs") {
        None => {}
        Some(Value::Object(ports)) => output_ports = Some(ports),
        Some(other) => issues.push(DockIssue::new(
            "D008",
            "outputs",
            format!("outputs must be an object mapping port names to {{signal}}, found {other}"),
        )),
    }
    for (port_name, port) in output_ports.into_iter().flatten() {
        let port_path = format!("outputs.{port_name}");
        if !valid_port_name(port_name) {
            issues.push(DockIssue::new(
                "D008",
                &port_path,
                "port name must match ^[a-z][a-z0-9_]{0,31}$",
            ));
            continue;
        }
        let Some(port_object) = port.as_object() else {
            issues.push(DockIssue::new(
                "D008",
                &port_path,
                "output port must be an object {signal: <source>::<task>.<stage>.<signal>}",
            ));
            continue;
        };
        for key in port_object.keys() {
            if key != "signal" {
                issues.push(DockIssue::new(
                    "D008",
                    format!("{port_path}.{key}"),
                    format!("unknown field {key:?}; allowed: [\"signal\"]"),
                ));
            }
        }
        let signal = port_object
            .get("signal")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if parse_canonical_signal(signal).is_none() {
            issues.push(DockIssue::new(
                "D008",
                format!("{port_path}.signal"),
                format!("signal must be <source>::<task>.<stage>.<signal>, found {signal:?}"),
            ));
            continue;
        }
        outputs.push(InterfacePortOutput {
            port: port_name.clone(),
            signal: signal.to_string(),
        });
    }

    if !issues.is_empty() {
        return Err(issues);
    }
    Ok(InterfaceDeclaration {
        name,
        order_modes,
        inputs,
        outputs,
    })
}

// ---------------------------------------------------------------------------
// 已解析 DockRoute（中性产物）
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct DockRouteInput {
    /// `<task>.<stage>#<localHookName>`
    pub hook_id: String,
    pub target_port: String,
}

#[derive(Debug, Clone)]
pub struct DockRouteOutput {
    /// 本地 sendSignals 信号名。
    pub signal: String,
    pub target_port: String,
}

#[derive(Debug, Clone)]
pub struct DockRoute {
    pub stage_identifier: String,
    pub target_name: String,
    pub target_interface_name: String,
    /// `new` | `existing`。
    pub order_mode: String,
    pub inputs: Vec<DockRouteInput>,
    pub outputs: Vec<DockRouteOutput>,
}

impl DockRoute {
    pub fn to_json(&self) -> Value {
        json!({
            "schemaVersion": DOCK_ROUTE_SCHEMA_VERSION,
            "local": {
                "stageIdentifier": self.stage_identifier,
            },
            "target": {
                "name": self.target_name,
                "interfaceName": self.target_interface_name,
            },
            "orderMode": self.order_mode,
            "inputBindings": self.inputs.iter().map(|input| json!({
                "hookId": input.hook_id,
                "port": input.target_port,
            })).collect::<Vec<_>>(),
            "outputBindings": self.outputs.iter().map(|output| json!({
                "signal": output.signal,
                "port": output.target_port,
            })).collect::<Vec<_>>(),
        })
    }
}

/// Link：本地未链接 routes + resolution manifest → 已解析 DockRoute 列表
/// （D008-D012、D015、D020）。纯函数，无网络、无 I/O。
/// 目标按 name 查找；跨定义的内容校验（身份重算/哈希比对）是各轨解析
/// 面的内务，不在 core。binding 数量上限（D016）在 parse 期钉死——
/// target:null 的 route 不经此处，link 期不重复设闸。
pub fn link_dock_routes(
    local_name: &str,
    unlinked: &[UnlinkedDockRoute],
    manifest: &ResolutionManifest,
) -> DockResult<Vec<DockRoute>> {
    let mut issues = Vec::new();

    let find_target = |name: &str| -> Option<&ResolutionTarget> {
        manifest.targets.iter().find(|target| target.name == name)
    };

    let mut routes = Vec::new();
    for route in unlinked {
        // issues 按 route 独立收集再合并：若共享一个累积 vec，首个出错
        // route 的残留会让后续 route 在收尾闸处被整体跳过，错误一次报不全。
        let mut route_issues = Vec::new();
        let config = &route.config;
        let path = format!("{}.executor.zhixuExecutorConfig", route.stage_identifier);
        let Some(target_name) = &config.target_name else {
            route_issues.push(DockIssue::new(
                "D008",
                format!("{path}.target"),
                "target is null (dynamic selection): a statically linked compilation cannot resolve this route — cloud runtimes fill dynamic targets from selection records",
            ));
            issues.extend(route_issues);
            continue;
        };
        let Some(target) = find_target(target_name) else {
            route_issues.push(DockIssue::new(
                "D008",
                format!("{path}.target"),
                format!("resolution manifest has no definition named {target_name:?}"),
            ));
            issues.extend(route_issues);
            continue;
        };

        // D009：接口按名解析。
        let Some(interface) = target
            .interfaces
            .iter()
            .find(|interface| interface.name == config.interface_name)
        else {
            route_issues.push(DockIssue::new(
                "D009",
                format!("{path}.interface"),
                format!(
                    "target {target_name:?} has no interface {:?}",
                    config.interface_name
                ),
            ));
            issues.extend(route_issues);
            continue;
        };
        // D020：mode 必须 ∈ 目标接口 orderModes（不静默替代，A05）。
        if !interface
            .order_modes
            .iter()
            .any(|mode| mode == &config.order_mode)
        {
            route_issues.push(DockIssue::new(
                "D020",
                format!("{path}.order.mode"),
                format!(
                    "interface {:?} of target {target_name:?} allows orderModes {:?}, found {:?}",
                    config.interface_name, interface.order_modes, config.order_mode
                ),
            ));
            issues.extend(route_issues);
            continue;
        };

        // D009（端口存在 + 方向）。
        let mut resolved_inputs = Vec::new();
        for (local_hook, port_name) in &config.input_map {
            if interface.inputs.iter().all(|port| &port.port != port_name) {
                route_issues.push(DockIssue::new(
                    "D009",
                    format!("{path}.inputMap.{local_hook}"),
                    format!(
                        "interface {:?} of target {target_name:?} has no input port {port_name:?}",
                        config.interface_name
                    ),
                ));
                continue;
            }
            resolved_inputs.push(DockRouteInput {
                hook_id: format!("{}#{local_hook}", route.stage_identifier),
                target_port: port_name.clone(),
            });
        }

        let mut resolved_outputs = Vec::new();
        for (local_signal, port_name) in &config.signal_map {
            if interface.outputs.iter().all(|port| &port.port != port_name) {
                route_issues.push(DockIssue::new(
                    "D009",
                    format!("{path}.signalMap.{local_signal}"),
                    format!(
                        "interface {:?} of target {target_name:?} has no output port {port_name:?}",
                        config.interface_name
                    ),
                ));
                continue;
            }
            resolved_outputs.push(DockRouteOutput {
                signal: local_signal.clone(),
                target_port: port_name.clone(),
            });
        }

        // D012：被绑定接口同一 source seam——双侧同口径、双侧都只看本
        // route 实际绑定的端口（文法 §8.4/规格 §2.4：seam 是"同一条 route
        // 引用的全部 input/output 端口"必须来自的同一个目标 source）：
        // input 侧从 route 绑定的 input 端口 source 观测，output 侧从
        // route 绑定的输出端口 canonical signal 前缀观测。接口声明但未被
        // 本 route 绑定的端口不属于这条执行通道（可由其他 route 另行
        // 绑定），不得参与 seam。两侧并集必须恰好一个 seam——绑定端口
        // 跨源寻址在此编译期拒绝。
        let mut seams = BTreeSet::new();
        for input in &resolved_inputs {
            if let Some(port) = interface
                .inputs
                .iter()
                .find(|port| port.port == input.target_port)
            {
                seams.insert(port.source.clone());
            }
        }
        for output in &resolved_outputs {
            if let Some(port) = interface
                .outputs
                .iter()
                .find(|port| port.port == output.target_port)
            {
                if let Some((source, _)) = port.signal.split_once("::") {
                    seams.insert(source.to_string());
                }
            }
        }
        if seams.len() > 1 {
            route_issues.push(DockIssue::new(
                "D012",
                &path,
                format!(
                    "the bound interface must expose a single target source seam across route-bound input port sources and route-bound output signal prefixes, found {seams:?}"
                ),
            ));
            issues.extend(route_issues);
            continue;
        }

        // 收尾闸只看本 route 的 issues：route_issues 为空则照常产 route，
        // 前序 route 的失败不得让后续干净 route 被跳过。
        if !route_issues.is_empty() {
            issues.append(&mut route_issues);
            continue;
        }

        routes.push(DockRoute {
            stage_identifier: route.stage_identifier.clone(),
            target_name: target_name.clone(),
            target_interface_name: interface.name.clone(),
            order_mode: config.order_mode.clone(),
            inputs: resolved_inputs,
            outputs: resolved_outputs,
        });
    }

    if !issues.is_empty() {
        return Err(issues);
    }

    // D015：route 启动图无环且深度受限。节点为定义 name，边 = 本地
    // resolved route + manifest 各定义声明的静态 dockEdges；缺 dockEdges
    // 的定义视为无出边（运行时建立的边由各轨解析面对实际父实例深度
    // 兜底）。环拒绝先于深度计算，保证最长路径有限且确定。
    let mut edges: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let local_node = local_name.to_string();
    for target in &manifest.targets {
        for edge in &target.dock_edges {
            edges
                .entry(target.name.clone())
                .or_default()
                .insert(edge.clone());
        }
    }
    for route in &routes {
        edges
            .entry(local_node.clone())
            .or_default()
            .insert(route.target_name.clone());
    }
    if let Some(cycle) = find_route_cycle(&edges) {
        issues.push(DockIssue::new(
            "D015",
            "dockRoutes",
            format!(
                "dock route startup graph has a reachable cycle: {}",
                cycle.join(" -> ")
            ),
        ));
    } else {
        let depth = max_reachable_route_depth(&edges, &local_node);
        if depth > usize::from(MAX_DOCK_DEPTH) {
            issues.push(DockIssue::new(
                "D015",
                "dockRoutes",
                format!(
                    "dock route startup graph depth {depth} exceeds MAX_DOCK_DEPTH {MAX_DOCK_DEPTH}"
                ),
            ));
        }
    }

    if !issues.is_empty() {
        return Err(issues);
    }
    routes.sort_by(|left, right| left.stage_identifier.cmp(&right.stage_identifier));
    Ok(routes)
}

fn find_route_cycle(edges: &BTreeMap<String, BTreeSet<String>>) -> Option<Vec<String>> {
    for start in edges.keys() {
        let mut parents: BTreeMap<String, String> = BTreeMap::new();
        let mut queue: std::collections::VecDeque<String> = std::collections::VecDeque::new();
        for next in edges.get(start).into_iter().flatten() {
            if next == start {
                return Some(vec![start.clone(), start.clone()]);
            }
            if parents.insert(next.clone(), start.clone()).is_none() {
                queue.push_back(next.clone());
            }
        }
        while let Some(current) = queue.pop_front() {
            for next in edges.get(&current).into_iter().flatten() {
                if next == start {
                    let mut cycle = vec![current.clone()];
                    let mut node = current.clone();
                    while node != *start {
                        node = parents[&node].clone();
                        cycle.push(node.clone());
                    }
                    cycle.reverse();
                    cycle.push(start.clone());
                    return Some(cycle);
                }
                if parents.insert(next.clone(), current.clone()).is_none() {
                    queue.push_back(next.clone());
                }
            }
        }
    }
    None
}

/// Return the maximum number of nodes on a route-startup path rooted at the
/// local definition. Cycles are rejected by `find_route_cycle` before this is
/// called, so the longest-path calculation is finite and deterministic.
fn max_reachable_route_depth(edges: &BTreeMap<String, BTreeSet<String>>, root: &str) -> usize {
    let mut depths = BTreeMap::from([(root.to_string(), 1usize)]);
    let mut queue = VecDeque::from([(root.to_string(), 1usize)]);
    let mut maximum = 1usize;
    while let Some((node, depth)) = queue.pop_front() {
        maximum = maximum.max(depth);
        for next in edges.get(&node).into_iter().flatten() {
            let next_depth = depth + 1;
            let should_visit = depths
                .get(next)
                .is_none_or(|known_depth| next_depth > *known_depth);
            if should_visit {
                depths.insert(next.clone(), next_depth);
                queue.push_back((next.clone(), next_depth));
            }
        }
    }
    maximum
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_rejects_duplicate_definition_names() {
        // name 是 linker 的解析键：manifest 内重名会让同名查找二义，
        // 必须在解析期响亮拒绝而不是静默取第一条。
        let manifest = json!({
            "schemaVersion": DOCK_RESOLUTION_SCHEMA_VERSION,
            "definitions": [
                { "name": "payment_execution", "interfaces": [minimal_interface("svc")] },
                { "name": "payment_execution", "interfaces": [minimal_interface("svc")] },
            ]
        });
        let issues = parse_resolution_manifest(&manifest).unwrap_err();
        assert!(
            issues
                .iter()
                .any(|issue| issue.code == "D008" && issue.message.contains("duplicate")),
            "{issues:?}"
        );
    }

    #[test]
    fn manifest_parses_optional_name_dock_edges() {
        // dockEdges 是可选的纯 name 出边：缺省=无出边；形状/残留键/非法
        // name 都是确定性的 D008，不得静默吞成空边。
        let manifest = json!({
            "schemaVersion": DOCK_RESOLUTION_SCHEMA_VERSION,
            "definitions": [{
                "name": "payment_execution",
                "interfaces": [minimal_interface("svc")],
                "dockEdges": [{ "target": "settlement" }],
            }]
        });
        let parsed = parse_resolution_manifest(&manifest).expect("dockEdges parse");
        assert_eq!(parsed.targets[0].dock_edges, vec!["settlement".to_string()]);

        let without_edges = json!({
            "schemaVersion": DOCK_RESOLUTION_SCHEMA_VERSION,
            "definitions": [{
                "name": "payment_execution",
                "interfaces": [minimal_interface("svc")],
            }]
        });
        let parsed = parse_resolution_manifest(&without_edges).expect("missing dockEdges parses");
        assert!(parsed.targets[0].dock_edges.is_empty());

        for (label, bad) in [
            ("not an array", json!("nope")),
            (
                "unknown edge field",
                json!([{ "target": "settlement", "zhixu": "x" }]),
            ),
            ("bad target slug", json!([{ "target": "Not-A-Name" }])),
            ("missing target", json!([{ "ref": "settlement" }])),
        ] {
            let mut poisoned = manifest.clone();
            poisoned["definitions"][0]["dockEdges"] = bad;
            let issues = parse_resolution_manifest(&poisoned)
                .expect_err(&format!("{label}: dockEdges must be rejected"));
            assert!(
                !issues.is_empty() && issues.iter().all(|issue| issue.code == "D008"),
                "{label}: {issues:?}"
            );
        }
    }

    #[test]
    fn manifest_rejects_unknown_interface_fields() {
        let interface = json!({
            "name": "svc",
            "orderModes": ["new"],
            "inputs": { "execute": { "hook": "main.work#DOCK_ENTER" } },
            "outputs": {},
            "artifactHash": "0x00",
        });
        let manifest = json!({
            "schemaVersion": DOCK_RESOLUTION_SCHEMA_VERSION,
            "definitions": [{ "name": "payment_execution", "interfaces": [interface] }],
        });
        let issues = parse_resolution_manifest(&manifest).unwrap_err();
        assert!(
            issues
                .iter()
                .any(|issue| issue.code == "D008" && issue.message.contains("artifactHash")),
            "{issues:?}"
        );
    }

    #[test]
    fn manifest_rejects_unknown_definition_fields() {
        // 条目级键闭集：interface 声明合法但残留拼错键（interface 单数）也
        // 按未知字段拒绝——发布方数据错误不得以缺省语义参与 link。
        let manifest = json!({
            "schemaVersion": DOCK_RESOLUTION_SCHEMA_VERSION,
            "definitions": [{
                "name": "payment_execution",
                "interfaces": [minimal_interface("svc")],
                "interface": [minimal_interface("svc")],
            }]
        });
        let issues = parse_resolution_manifest(&manifest).unwrap_err();
        assert!(
            issues.iter().any(|issue| issue.code == "D008"
                && issue.message.contains("unknown field")
                && issue.message.contains("\"interface\"")),
            "{issues:?}"
        );
    }

    fn minimal_interface(name: &str) -> Value {
        json!({
            "name": name,
            "orderModes": ["new"],
            "inputs": { "execute": { "source": "buyer", "hook": "main.work#DOCK_ENTER" } },
            "outputs": {},
        })
    }

    #[test]
    fn manifest_input_ports_require_source() {
        // input 端口的 source 兄弟键是 manifest 必填项——
        // 缺失/空白/非字符串都是确定性 D008，不回退、不臆造。
        let base = json!({
            "schemaVersion": DOCK_RESOLUTION_SCHEMA_VERSION,
            "definitions": [{
                "name": "payment_execution",
                "interfaces": [{
                    "name": "svc",
                    "orderModes": ["new"],
                    "inputs": { "execute": { "source": "buyer", "hook": "main.work#DOCK_ENTER" } },
                    "outputs": {},
                }],
            }]
        });
        parse_resolution_manifest(&base).expect("input port with source parses");

        let mut missing = base.clone();
        missing["definitions"][0]["interfaces"][0]["inputs"]["execute"]
            .as_object_mut()
            .unwrap()
            .remove("source");
        let issues = parse_resolution_manifest(&missing).unwrap_err();
        assert!(
            issues.iter().any(|issue| issue.code == "D008"
                && issue.path.ends_with("inputs.execute.source")
                && issue.message.contains("source is required")),
            "{issues:?}"
        );

        let mut blank = base.clone();
        blank["definitions"][0]["interfaces"][0]["inputs"]["execute"]["source"] = json!("  ");
        let issues = parse_resolution_manifest(&blank).unwrap_err();
        assert!(
            issues
                .iter()
                .any(|issue| issue.code == "D008" && issue.message.contains("must not be blank")),
            "{issues:?}"
        );

        // 键闭集同步：source/hook 之外的兄弟键按未知字段拒绝。
        let mut extra = base;
        extra["definitions"][0]["interfaces"][0]["inputs"]["execute"]["sourceClass"] =
            json!("buyer");
        let issues = parse_resolution_manifest(&extra).unwrap_err();
        assert!(
            issues.iter().any(|issue| issue.code == "D008"
                && issue.message.contains("unknown field")
                && issue.message.contains("sourceClass")),
            "{issues:?}"
        );
    }

    #[test]
    fn link_rejects_cross_source_seams_from_both_sides() {
        // D012 从 input 端口 source 与 output signal 前缀双侧
        // 观测 seam——被绑定接口的任一侧跨源即拒绝（编译期 input 侧校验）。
        let unlinked = vec![UnlinkedDockRoute {
            stage_identifier: "local.stage".to_string(),
            stage_source: "local-src".to_string(),
            config: ZhixuExecutorConfig {
                target_name: Some("target_def".to_string()),
                interface_name: "svc".to_string(),
                order_mode: ORDER_MODE_NEW.to_string(),
                input_map: BTreeMap::from([("ENTER".to_string(), "execute".to_string())]),
                signal_map: BTreeMap::new(),
            },
        }];

        // 基线：双侧同源放行。
        let single_seam_manifest = json!({
            "schemaVersion": DOCK_RESOLUTION_SCHEMA_VERSION,
            "definitions": [{
                "name": "target_def",
                "interfaces": [{
                    "name": "svc",
                    "orderModes": ["new"],
                    "inputs": {
                        "execute": { "source": "alpha", "hook": "main.work#DOCK_ENTER" }
                    },
                    "outputs": {},
                }],
            }]
        });
        let manifest = parse_resolution_manifest(&single_seam_manifest).unwrap();
        link_dock_routes("local", &unlinked, &manifest)
            .expect("single-seam interface on both sides links");

        // 未绑定的跨源 input 端口不参与 seam（文法 §8.4：seam 只覆盖本
        // route 引用的端口）：接口另有一个 beta 源端口，但本 route 只绑定
        // execute——照常链接。旧行为（被绑定接口的全部 input 端口都参与
        // 观测）会把该形态误拒。
        let unbound_cross = json!({
            "schemaVersion": DOCK_RESOLUTION_SCHEMA_VERSION,
            "definitions": [{
                "name": "target_def",
                "interfaces": [{
                    "name": "svc",
                    "orderModes": ["new", "existing"],
                    "inputs": {
                        "execute": { "source": "alpha", "hook": "main.work#DOCK_ENTER" },
                        "audit": { "source": "beta", "hook": "main.work#DOCK_AUDIT" }
                    },
                    "outputs": {},
                }],
            }]
        });
        let manifest = parse_resolution_manifest(&unbound_cross).unwrap();
        link_dock_routes("local", &unlinked, &manifest)
            .expect("unbound cross-source input port must not join the seam");

        // route 绑定的两个 input 端口跨源（existing 模式允许 0..N 条 input
        // 绑定）：同一执行通道内混入两个 source 类，D012 拒绝。
        let inputs_cross_unlinked = vec![UnlinkedDockRoute {
            stage_identifier: "local.stage".to_string(),
            stage_source: "local-src".to_string(),
            config: ZhixuExecutorConfig {
                target_name: Some("target_def".to_string()),
                interface_name: "svc".to_string(),
                order_mode: ORDER_MODE_EXISTING.to_string(),
                input_map: BTreeMap::from([
                    ("ENTER".to_string(), "execute".to_string()),
                    ("AUDIT".to_string(), "audit".to_string()),
                ]),
                signal_map: BTreeMap::new(),
            },
        }];
        let manifest = parse_resolution_manifest(&unbound_cross).unwrap();
        let issues = link_dock_routes("local", &inputs_cross_unlinked, &manifest).unwrap_err();
        assert!(
            issues.iter().any(|issue| issue.code == "D012"
                && issue.message.contains("input port sources")
                && issue.message.contains("route-bound output signal prefixes")),
            "{issues:?}"
        );

        // input 与 output 两侧跨源：route-bound 输出端口的前缀与 input
        // 端口 source 不一致。
        let sides_cross_unlinked = vec![UnlinkedDockRoute {
            stage_identifier: "local.stage".to_string(),
            stage_source: "local-src".to_string(),
            config: ZhixuExecutorConfig {
                target_name: Some("target_def".to_string()),
                interface_name: "svc".to_string(),
                order_mode: ORDER_MODE_NEW.to_string(),
                input_map: BTreeMap::from([("ENTER".to_string(), "execute".to_string())]),
                signal_map: BTreeMap::from([("done_sig".to_string(), "done".to_string())]),
            },
        }];
        let sides_cross = json!({
            "schemaVersion": DOCK_RESOLUTION_SCHEMA_VERSION,
            "definitions": [{
                "name": "target_def",
                "interfaces": [{
                    "name": "svc",
                    "orderModes": ["new"],
                    "inputs": {
                        "execute": { "source": "alpha", "hook": "main.work#DOCK_ENTER" }
                    },
                    "outputs": {
                        "done": { "signal": "beta::main.work.cmp" }
                    },
                }],
            }]
        });
        let manifest = parse_resolution_manifest(&sides_cross).unwrap();
        let issues = link_dock_routes("local", &sides_cross_unlinked, &manifest).unwrap_err();
        assert!(
            issues
                .iter()
                .any(|issue| issue.code == "D012"
                    && issue.message.contains("single target source seam")),
            "{issues:?}"
        );
    }

    #[test]
    fn route_depth_counts_local_definition_and_targets() {
        let root = "local-def";
        let mut edges = BTreeMap::new();
        for index in 0..7 {
            edges
                .entry(if index == 0 {
                    root.to_string()
                } else {
                    format!("target-{index}")
                })
                .or_insert_with(BTreeSet::new)
                .insert(format!("target-{}", index + 1));
        }
        assert_eq!(max_reachable_route_depth(&edges, root), 8);

        edges
            .entry("target-7".to_string())
            .or_insert_with(BTreeSet::new)
            .insert("target-8".to_string());
        assert_eq!(max_reachable_route_depth(&edges, root), 9);
        assert!(9 > usize::from(MAX_DOCK_DEPTH));
    }

    #[test]
    fn route_depth_ignores_disconnected_manifest_edges() {
        let mut edges = BTreeMap::new();
        edges.insert(
            "unrelated".to_string(),
            BTreeSet::from(["unrelated-child".to_string()]),
        );
        assert_eq!(max_reachable_route_depth(&edges, "local"), 1);
    }

    #[test]
    fn non_zhixu_executor_with_delegation_config_is_rejected() {
        // "既静态执行者又委托对接"的矛盾声明在收集期响亮拒绝（D001 同罪）：
        // 静默放行会把 zhixuExecutorConfig 原文烧进 executorRoutes 承诺。
        let stage = serde_json::from_value::<ZhixuStage>(json!({
            "name": "execute_payment",
            "source": "buyer",
            "receiveSignals": { "EXECUTE": "buyer::task.execute_payment.exec" },
            "sendSignals": ["str"],
            "executor": {
                "supplierType": "organization",
                "supplierID": "payment-gateway",
                "zhixuExecutorConfig": {
                    "target": { "zhixu": "payment_execution" },
                    "interface": "payment_service",
                    "order": { "mode": "new" },
                    "inputMap": { "EXECUTE": "execute" },
                    "signalMap": { "str": "started" }
                }
            }
        }))
        .expect("stage decodes");
        let issues = collect_unlinked_routes(&[("task.execute_payment".to_string(), stage)])
            .expect_err("organization executor carrying zhixuExecutorConfig must be rejected");
        assert!(
            issues.iter().any(|issue| issue.code == "D002"
                && issue.path == "task.execute_payment.executor.zhixuExecutorConfig"
                && issue
                    .message
                    .contains("only valid when supplierType is zhixu")),
            "{issues:?}"
        );

        // 基线：organization executor 不携带委托配置，收集期照常跳过（无 issue）。
        let stage = serde_json::from_value::<ZhixuStage>(json!({
            "name": "plain",
            "source": "buyer",
            "receiveSignals": { "RUN": "buyer::task.plain.run" },
            "sendSignals": ["cmp"],
            "executor": { "supplierType": "organization", "supplierID": "org" }
        }))
        .expect("stage decodes");
        let routes =
            collect_unlinked_routes(&[("task.plain".to_string(), stage)]).expect("clean executor");
        assert!(routes.is_empty());
    }

    #[test]
    fn manifest_rejects_non_object_inputs_and_outputs() {
        // inputs/outputs 类型非法（字符串/数组）是发布方数据错误：响亮拒绝，
        // 不得静默吞成空 map 按"无端口"继续 link。
        let base = json!({
            "schemaVersion": DOCK_RESOLUTION_SCHEMA_VERSION,
            "definitions": [{
                "name": "payment_execution",
                "interfaces": [minimal_interface("svc")],
            }]
        });
        for (label, key, bad) in [
            ("inputs string", "inputs", json!("OOPS-NOT-AN-OBJECT")),
            ("inputs array", "inputs", json!([])),
            ("outputs string", "outputs", json!("OOPS-NOT-AN-OBJECT")),
            ("outputs number", "outputs", json!(7)),
        ] {
            let mut poisoned = base.clone();
            poisoned["definitions"][0]["interfaces"][0][key] = bad;
            let issues = parse_resolution_manifest(&poisoned)
                .err()
                .unwrap_or_else(|| panic!("{label}: must be rejected"));
            assert!(
                issues.iter().any(|issue| issue.code == "D008"
                    && issue.path.ends_with(&format!(".{key}"))
                    && issue.message.contains("must be an object")),
                "{label}: {issues:?}"
            );
        }
        // 基线：键缺席（可选）与对象形态照常解析。
        parse_resolution_manifest(&base).expect("object-shaped inputs/outputs parse");
    }

    #[test]
    fn manifest_rejects_non_string_order_mode_entries() {
        // orderModes 非字符串项不得被 filter_map 静默丢弃：["new", 123] 是
        // 确定性非法输入而不是 ["new"]。
        let manifest = json!({
            "schemaVersion": DOCK_RESOLUTION_SCHEMA_VERSION,
            "definitions": [{
                "name": "payment_execution",
                "interfaces": [{
                    "name": "svc",
                    "orderModes": ["new", 123],
                    "inputs": { "execute": { "source": "buyer", "hook": "main.work#DOCK_ENTER" } },
                    "outputs": {},
                }],
            }]
        });
        let issues = parse_resolution_manifest(&manifest).unwrap_err();
        assert!(
            issues.iter().any(|issue| issue.code == "D008"
                && issue.path.ends_with(".orderModes")
                && issue.message.contains("entries must be strings")),
            "{issues:?}"
        );
        // 非数组形态同样是确定性的 D008。
        let mut not_array = manifest.clone();
        not_array["definitions"][0]["interfaces"][0]["orderModes"] = json!("new");
        let issues = parse_resolution_manifest(&not_array).unwrap_err();
        assert!(
            issues.iter().any(|issue| issue.code == "D008"
                && issue.path.ends_with(".orderModes")
                && issue.message.contains("must be an array of strings")),
            "{issues:?}"
        );
    }

    #[test]
    fn canonical_signal_shapes_reject_empty_segments() {
        // 空段不是合法 canonical 形态：拼出的空 stage/空 signal 是永不匹配
        // 的寻址键（与 Go 镜像 isCanonicalSignalShape 的非空段校验同口径）。
        for signal in [
            "buyer::main..cmp",
            "buyer::main.stage.",
            "buyer::.stage.cmp",
            "::main.stage.cmp",
        ] {
            assert!(
                parse_canonical_signal(signal).is_none(),
                "{signal:?} must not parse as a canonical signal"
            );
        }
        assert_eq!(
            parse_canonical_signal("buyer::main.stage.cmp"),
            Some((
                "buyer".to_string(),
                "main.stage".to_string(),
                "cmp".to_string()
            ))
        );

        // manifest 路径侧同口径：D014/D008 响亮拒绝而不是吞成空段。
        let manifest = json!({
            "schemaVersion": DOCK_RESOLUTION_SCHEMA_VERSION,
            "definitions": [{
                "name": "payment_execution",
                "interfaces": [{
                    "name": "svc",
                    "orderModes": ["new"],
                    "inputs": { "execute": { "source": "buyer", "hook": "main.work#DOCK_ENTER" } },
                    "outputs": { "done": { "signal": "buyer::main..cmp" } },
                }],
            }]
        });
        let issues = parse_resolution_manifest(&manifest).unwrap_err();
        assert!(
            issues.iter().any(|issue| issue.code == "D008"
                && issue
                    .message
                    .contains("must be <source>::<task>.<stage>.<signal>")),
            "{issues:?}"
        );
    }
}
