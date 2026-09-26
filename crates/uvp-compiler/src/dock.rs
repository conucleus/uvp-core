//! Zhixu Dock 委托协议 DSL 壳的结构性语义。
//!
//! 本模块固定：
//! - 调用方 `executor.zhixuExecutorConfig`（键闭集 {target, interface,
//!   order, inputMap, signalMap}）与目标 `spec.dockInterface`（具名接口
//!   map）的 source 语义；
//! - 跨定义 linker（dockTargets 注册表输入，纯函数，无网络；目标按
//!   uid 解析）；
//! - 中性 route/接口声明产物形状；
//! - 编译期错误码 D001-D016、D019-D020、D025、接口形状错误码
//!   D021/D022；发射适格面 D026-D031（artifact/validate）。
//! - D007/D011/D017/D018/D023/D024 不在本协议编号面内，永久空缺不复用。
//!
//! 哈希承诺与身份派生是各轨权威的内务（链轨 TS、云轨 DB），core 不计算
//! 任何 hash/uid：目标引用消费已派生的内容身份 uid（`zx-<32hex>`），产物
//! 不携带 hash/root 字段。

use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use uvp_hook_dsl::{parse_hook, Gate, ParseHookRequest, Profile};
use uvp_model::{DockInterfaceSpec, ZhixuStage};

// ---------------------------------------------------------------------------
// 冻结常量
// ---------------------------------------------------------------------------

pub const DOCK_ROUTE_SCHEMA_VERSION: &str = "uvp.dockRoute.v3";
/// 未解析 route 的声明面产物形态：本地声明完整——
/// target:null 动态选择的目标空缺（云轨运行时由选择记录补齐）；静态
/// 目标 route 在 parse-only 产物中同面携带作者声明的 target.zhixu。
pub const DOCK_ROUTE_UNRESOLVED_SCHEMA_VERSION: &str = "uvp.dockRoute.unresolved.v1";

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
/// 端口计数无闸时声明面规模随 plan 输入无界增长（计数闸）。
pub const MAX_INTERFACE_PORTS: usize = 64;

/// dockTargets 注册表条目上限：注册表是注入数据，计数无闸时毒载荷可让
/// link 期的图规模无界增长（计数闸）。
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
    /// `Some(uid)` = 目标定义的内容派生身份 uid。
    pub target_uid: Option<String>,
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

    // D003：target 必填键——{zhixu: <目标定义 uid>} 或显式 null（动态
    // 选择）。引用按内容精确指向，壳上不携带展示名。
    let target_uid = match config_object.get("target") {
        None => {
            issues.push(DockIssue::new(
                "D003",
                format!("{path}.target"),
                "target is required: {zhixu: <target definition uid>} for a static target, or null for runtime selection",
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
                Some(uid) => {
                    let uid = uid.trim();
                    if !crate::validate::is_definition_uid(uid) {
                        issues.push(DockIssue::new(
                            "D003",
                            format!("{path}.target.zhixu"),
                            format!(
                                "target.zhixu must be the target definition's content-derived uid, matching ^zx-[0-9a-f]{{32}}$, found {uid:?}; {UNSUPPORTED_HINT}"
                            ),
                        ));
                    }
                    Some(uid.to_string())
                }
                None => {
                    issues.push(DockIssue::new(
                        "D003",
                        format!("{path}.target.zhixu"),
                        "target.zhixu is required when target is an object (the target definition uid)",
                    ));
                    None
                }
            }
        }
        Some(_) => {
            issues.push(DockIssue::new(
                "D003",
                format!("{path}.target"),
                "target must be an object {zhixu: <target definition uid>} or null (dynamic selection)",
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
    // 命名空间：'.' 是信号名分隔符、组合长度受 signal_name 列宽约束。
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
        target_uid,
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
    /// target.zhixu（目标定义 uid 引用），与动态目标对称进声明面。
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
        if let Some(target_uid) = &self.config.target_uid {
            route["target"] = json!({ "zhixu": target_uid });
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

/// 一个具名接口的中性声明（与 dockTargets 条目的接口元素同形状）。
#[derive(Debug, Clone)]
pub struct InterfaceDeclaration {
    pub name: String,
    pub order_modes: Vec<String>,
    pub inputs: Vec<InterfacePortInput>,
    pub outputs: Vec<InterfacePortOutput>,
}

/// 编译本地 `spec.dockInterface` 的产物：中性接口声明 + entrance 出生
/// 事实键（出生通道键并集查重的 dock 侧输入）。entrance 键只从本地
/// 接口编译收集——dockTargets 侧接口是远端目标的声明面，不参与本 plan 的
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
                gate: Gate::Hook,
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
// Dock targets + linker
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct DockTarget {
    /// 目标定义身份 uid（内容派生，`zx-<32hex>`）。编译入口从注册表
    /// （云侧 DB / 链侧 resolution）按 uid 读出目标定义——引用按内容
    /// 精确指向，目标内容变即新 uid，旧引用不静默跟随。
    pub uid: String,
    pub interfaces: Vec<InterfaceDeclaration>,
    /// 该定义声明的静态 dock 出边（目标定义 uid）。linker 用它做 D015
    /// 环/深度检测；缺省 = 无出边，运行时建立的边由各轨解析面兜底。
    pub dock_edges: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct DockTargets {
    pub targets: Vec<DockTarget>,
}

/// 目标定义的静态 dock 出边：全部 executor 的静态 target uid。D015 启动
/// 图的边集来源（定义声明的静态边；运行时建立的边由各轨解析面兜底）。
fn extract_static_target_uids(definition: &uvp_model::ZhixuDefinition) -> Vec<String> {
    let mut edges = Vec::new();
    for pattern in &definition.spec.task_patterns {
        for stage in &pattern.stages {
            let Some(executor) = &stage.executor else {
                continue;
            };
            let Some(config) = &executor.zhixu_executor_config else {
                continue;
            };
            if let Some(uid) = config
                .get("target")
                .and_then(|target| target.get("zhixu"))
                .and_then(Value::as_str)
            {
                edges.push(uid.to_string());
            }
        }
    }
    edges
}

/// 解析编译入口注入的 dock 目标注册表（云侧按 uid 直读注册表、链侧从
/// resolution 收集）。条目 {uid, definition}：目标定义原文入场，接口声明
/// 与静态出边由 core 从原文提取——调用方不再自报接口清单，提取规则单源。
/// uid 是解析键：条目重复即注入数据错误，响亮拒绝。
pub fn parse_dock_targets(value: &Value) -> DockResult<DockTargets> {
    let mut issues = Vec::new();
    let mut targets = Vec::new();
    let entries = value.as_array();
    let mut uids_seen = BTreeSet::new();
    // D008（计数闸）：条目数上限——注入数据错误的规模面在解析期收口，
    // 不给 link 期留下无界图。闸按引用取 len 先行、过闸后才逐条消费。
    let entries_len = entries.map_or(0, Vec::len);
    if entries_len > MAX_MANIFEST_DEFINITIONS {
        issues.push(DockIssue::new(
            "D008",
            "dockTargets",
            format!(
                "dockTargets carries {entries_len} definitions, limit is {MAX_MANIFEST_DEFINITIONS}"
            ),
        ));
        return Err(issues);
    }
    for (index, entry) in entries.into_iter().flatten().enumerate() {
        let path = format!("dockTargets[{index}]");
        // 条目级键闭集与 interface/dockEdges 闸口同口径：拼错的字段被静默
        // 吸收会让注入数据错误以缺省语义参与 link。
        if let Some(entry_object) = entry.as_object() {
            for key in entry_object.keys() {
                if !matches!(key.as_str(), "uid" | "definition") {
                    issues.push(DockIssue::new(
                        "D008",
                        format!("{path}.{key}"),
                        format!("unknown field {key:?}; allowed: [\"uid\", \"definition\"]"),
                    ));
                }
            }
        }
        let uid = entry
            .get("uid")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_string();
        if uid.is_empty() {
            issues.push(DockIssue::new(
                "D008",
                format!("{path}.uid"),
                "uid is required (the target definition content-derived identity)",
            ));
            continue;
        }
        if !crate::validate::is_definition_uid(&uid) {
            issues.push(DockIssue::new(
                "D008",
                format!("{path}.uid"),
                format!(
                    "uid must be the target definition content-derived identity, matching ^zx-[0-9a-f]{{32}}$, found {uid:?}"
                ),
            ));
            continue;
        }
        if !uids_seen.insert(uid.clone()) {
            issues.push(DockIssue::new(
                "D008",
                &path,
                format!("duplicate definition uid {uid:?}: uid is the resolution key and must be unique"),
            ));
            continue;
        }
        // 目标定义原文：接口声明与静态出边由 core 从原文单源提取——
        // 调用方自报接口清单的时代已退役。
        let definition_value = entry.get("definition").cloned().unwrap_or(Value::Null);
        let definition: uvp_model::ZhixuDefinition =
            match serde_json::from_value(definition_value.clone()) {
                Ok(definition) => definition,
                Err(err) => {
                    issues.push(DockIssue::new(
                        "D008",
                        format!("{path}.definition"),
                        format!("target definition is not a valid Zhixu document: {err}"),
                    ));
                    continue;
                }
            };

        // 静态出边：目标定义自身全部静态 dock target uid（D015 启动图）。
        // 边值来自原文 executor config 的 target.zhixu，uid 形态与 D003
        // 同口径——形态非法的边进入 D015 图会成为永不匹配的寻址键。
        let dock_edges = extract_static_target_uids(&definition);
        for edge in &dock_edges {
            if !crate::validate::is_definition_uid(edge) {
                issues.push(DockIssue::new(
                    "D008",
                    format!("{path}.definition"),
                    format!(
                        "static dock edge target must be a definition uid, matching ^zx-[0-9a-f]{{32}}$, found {edge:?}"
                    ),
                ));
            }
        }
        if !issues.is_empty() {
            continue;
        }

        // 接口声明：spec.dockInterface 的端口值 {hook}/{signal} 与中性
        // 形态同构；input 端口的 source 单源推导自 hook 引用所属 stage
        // 的 source 字段（文法 §8.2——source 是 owning stage 的因果身份）。
        // 以限定标识 <task>.<stage> 为键：裸 stage 名跨 task pattern 可合法
        // 重名（treat.main 与 bc.main），裸名索引会后写覆盖并把 seam 推错。
        let mut stages_by_identifier: BTreeMap<String, &ZhixuStage> = BTreeMap::new();
        for pattern in &definition.spec.task_patterns {
            for stage in &pattern.stages {
                stages_by_identifier.insert(format!("{}.{}", pattern.name, stage.name), stage);
            }
        }
        let mut interfaces = Vec::new();
        let mut interfaces_valid = true;
        for (interface_name, spec) in &definition.spec.dock_interface {
            let interface_path = format!("{path}.definition.spec.dockInterface.{interface_name}");
            // 接口名与端口名同规则（同 compile_dock_interface 的 D021 口径）：
            // 调用方按名引用接口，非法名是不可寻址的承诺面。
            if !valid_port_name(interface_name) {
                issues.push(DockIssue::new(
                    "D008",
                    &interface_path,
                    format!(
                        "interface name must match ^[a-z][a-z0-9_]{{0,31}}$, found {interface_name:?}"
                    ),
                ));
                interfaces_valid = false;
                continue;
            }
            let mut order_modes = spec.order_modes.clone();
            order_modes.sort();
            if !valid_order_modes(&order_modes) {
                issues.push(DockIssue::new(
                    "D008",
                    format!("{interface_path}.orderModes"),
                    "orderModes must be a non-empty subset of {new, existing} without duplicates",
                ));
                interfaces_valid = false;
                continue;
            }
            let mut inputs = Vec::new();
            for (port_name, port) in &spec.inputs {
                if !valid_port_name(port_name) {
                    issues.push(DockIssue::new(
                        "D008",
                        format!("{interface_path}.inputs.{port_name}"),
                        "port name must match ^[a-z][a-z0-9_]{0,31}$",
                    ));
                    interfaces_valid = false;
                    continue;
                }
                let Some((stage_identifier, _hook)) = parse_hook_reference(&port.hook) else {
                    issues.push(DockIssue::new(
                        "D008",
                        format!("{interface_path}.inputs.{port_name}.hook"),
                        format!(
                            "hook must be <task>.<stage>#<receiveHookName>, found {:?}",
                            port.hook
                        ),
                    ));
                    interfaces_valid = false;
                    continue;
                };
                let Some(owning_stage) = stages_by_identifier.get(stage_identifier.as_str()) else {
                    issues.push(DockIssue::new(
                        "D008",
                        format!("{interface_path}.inputs.{port_name}.hook"),
                        format!("hook {:?} references stage {:?} which does not exist in the target definition", port.hook, stage_identifier),
                    ));
                    interfaces_valid = false;
                    continue;
                };
                inputs.push(InterfacePortInput {
                    port: port_name.clone(),
                    source: owning_stage.source.clone(),
                    hook: port.hook.clone(),
                });
            }
            let mut outputs = Vec::new();
            for (port_name, port) in &spec.outputs {
                if !valid_port_name(port_name) {
                    issues.push(DockIssue::new(
                        "D008",
                        format!("{interface_path}.outputs.{port_name}"),
                        "port name must match ^[a-z][a-z0-9_]{0,31}$",
                    ));
                    interfaces_valid = false;
                    continue;
                }
                // output 形状：canonical 三段式寻址键（与 D014 同口径）——
                // 空段拼出的键永不匹配，seam 前缀观测同样失真。
                if parse_canonical_signal(&port.signal).is_none() {
                    issues.push(DockIssue::new(
                        "D008",
                        format!("{interface_path}.outputs.{port_name}.signal"),
                        format!(
                            "signal must be <source>::<task>.<stage>.<signal>, found {:?}",
                            port.signal
                        ),
                    ));
                    interfaces_valid = false;
                    continue;
                }
                outputs.push(InterfacePortOutput {
                    port: port_name.clone(),
                    signal: port.signal.clone(),
                });
            }
            interfaces.push(InterfaceDeclaration {
                name: interface_name.clone(),
                order_modes,
                inputs,
                outputs,
            });
        }
        if !interfaces_valid {
            continue;
        }
        if interfaces.is_empty() {
            issues.push(DockIssue::new(
                "D008",
                format!("{path}.definition.spec.dockInterface"),
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
        targets.push(DockTarget {
            uid,
            interfaces,
            dock_edges,
        });
    }
    if !issues.is_empty() {
        return Err(issues);
    }
    Ok(DockTargets { targets })
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
    pub target_uid: String,
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
                "uid": self.target_uid,
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

/// Link：本地未链接 routes + 目标注册表 → 已解析 DockRoute 列表
/// （D008-D012、D015、D020）。纯函数，无网络、无 I/O。
/// 目标按 uid 查找（引用按内容精确指向）；binding 数量上限（D016）在
/// parse 期钉死——target:null 的 route 不经此处，link 期不重复设闸。
pub fn link_dock_routes(
    local_name: &str,
    unlinked: &[UnlinkedDockRoute],
    targets: &DockTargets,
) -> DockResult<Vec<DockRoute>> {
    let mut issues = Vec::new();

    let find_target = |uid: &str| -> Option<&DockTarget> {
        targets.targets.iter().find(|target| target.uid == uid)
    };

    let mut routes = Vec::new();
    for route in unlinked {
        // issues 按 route 独立收集再合并：若共享一个累积 vec，首个出错
        // route 的残留会让后续 route 在收尾闸处被整体跳过，错误一次报不全。
        let mut route_issues = Vec::new();
        let config = &route.config;
        let path = format!("{}.executor.zhixuExecutorConfig", route.stage_identifier);
        let Some(target_uid) = &config.target_uid else {
            route_issues.push(DockIssue::new(
                "D008",
                format!("{path}.target"),
                "target is null (dynamic selection): a statically linked compilation cannot resolve this route — cloud runtimes fill dynamic targets from selection records",
            ));
            issues.extend(route_issues);
            continue;
        };
        let Some(target) = find_target(target_uid) else {
            route_issues.push(DockIssue::new(
                "D008",
                format!("{path}.target"),
                format!("no published definition with uid {target_uid:?}: static targets require the target to be registered at compile time"),
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
                    "target {target_uid:?} has no interface {:?}",
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
                    "interface {:?} of target {target_uid:?} allows orderModes {:?}, found {:?}",
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
                        "interface {:?} of target {target_uid:?} has no input port {port_name:?}",
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
                        "interface {:?} of target {target_uid:?} has no output port {port_name:?}",
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
            target_uid: target_uid.clone(),
            target_interface_name: interface.name.clone(),
            order_mode: config.order_mode.clone(),
            inputs: resolved_inputs,
            outputs: resolved_outputs,
        });
    }

    if !issues.is_empty() {
        return Err(issues);
    }

    // D015：route 启动图无环且深度受限。节点为定义 uid，边 = 本地
    // resolved route + 各目标声明的静态 dockEdges；缺 dockEdges 的定义
    // 视为无出边（运行时建立的边由各轨解析面对实际父实例深度兜底）。
    // 环拒绝先于深度计算，保证最长路径有限且确定。
    let mut edges: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let local_node = local_name.to_string();
    for target in &targets.targets {
        for edge in &target.dock_edges {
            edges
                .entry(target.uid.clone())
                .or_default()
                .insert(edge.clone());
        }
    }
    for route in &routes {
        edges
            .entry(local_node.clone())
            .or_default()
            .insert(route.target_uid.clone());
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

    /// uid 形态的测试占位（^zx-[0-9a-f]{32}$）：测试语义只依赖形态与
    /// 相等性，不依赖派生过程（派生是各轨内务）。
    const TARGET_UID: &str = "zx-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const EDGE_UID: &str = "zx-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    /// 最小合法 Zhixu 文档壳：单 task，stages 与 dockInterface 由用例给。
    /// 注册表条目的 definition 必须是模型层可反序列化的完整文档。
    fn zhixu_document(stages: Value, dock_interface: Value) -> Value {
        json!({
            "apiVersion": "uvp/v0",
            "kind": "Zhixu",
            "metadata": { "name": "target_definition" },
            "spec": {
                "platform": { "type": "cloud" },
                "nucleation": { "id": "target-core" },
                "taskPatterns": [{ "name": "main", "stages": stages }],
                "dockInterface": dock_interface,
            }
        })
    }

    fn target_entry(definition: Value) -> Value {
        json!({ "uid": TARGET_UID, "definition": definition })
    }

    /// 双 task pattern 文档：两个 pattern 各带一个同名裸 stage（main），
    /// source 不同——限定标识键控的回归面。
    fn two_pattern_document_with_same_stage_names() -> Value {
        json!({
            "apiVersion": "uvp/v0",
            "kind": "Zhixu",
            "metadata": { "name": "wwt_definition" },
            "spec": {
                "platform": { "type": "cloud" },
                "nucleation": { "id": "wwt-core" },
                "taskPatterns": [
                    { "name": "treat", "stages": [plain_stage("main", "wwt")] },
                    { "name": "bc", "stages": [plain_stage("main", "dyeing")] },
                ],
                "dockInterface": { "svc": interface_spec(
                    json!(["existing"]),
                    json!({ "amend": { "hook": "treat.main#DOCK_AMEND" } }),
                    json!({}),
                ) },
            }
        })
    }

    /// 裸 stage 名跨 pattern 重名（treat.main 与 bc.main、source 不同）时，
    /// input 端口 seam 必须按限定标识推导出 owning stage 的 source——
    /// 裸名索引会后写覆盖，把 treat 端口错推成 bc 的 source。
    #[test]
    fn dock_target_input_source_survives_duplicate_bare_stage_names() {
        let targets = json!([target_entry(two_pattern_document_with_same_stage_names())]);
        let parsed = parse_dock_targets(&targets).expect("duplicate bare stage names parse");
        let inputs = &parsed.targets[0].interfaces[0].inputs;
        assert_eq!(inputs.len(), 1);
        assert_eq!(inputs[0].source, "wwt", "seam must follow the qualified owning stage");
    }

    /// 无 executor 的最小 stage（source 供 input 端口单源推导）。
    fn plain_stage(name: &str, source: &str) -> Value {
        json!({ "name": name, "source": source })
    }

    /// 带静态 dock target 的 zhixu executor stage（出边提取源；
    /// target:null = 动态选择，无静态出边）。
    fn zhixu_stage(name: &str, source: &str, target_uid: Option<&str>) -> Value {
        json!({
            "name": name,
            "source": source,
            "executor": {
                "supplierType": "zhixu",
                "zhixuExecutorConfig": {
                    "target": target_uid
                        .map(|uid| json!({ "zhixu": uid }))
                        .unwrap_or(Value::Null),
                    "interface": "svc",
                    "order": { "mode": "existing" },
                    "signalMap": { "cmp": "done" },
                }
            }
        })
    }

    fn interface_spec(order_modes: Value, inputs: Value, outputs: Value) -> Value {
        json!({ "orderModes": order_modes, "inputs": inputs, "outputs": outputs })
    }

    /// 单接口 svc 的最小目标定义：execute 端口引用 stage work。
    fn minimal_definition() -> Value {
        zhixu_document(
            json!([plain_stage("work", "buyer")]),
            json!({
                "svc": interface_spec(
                    json!(["new"]),
                    json!({ "execute": { "hook": "main.work#DOCK_ENTER" } }),
                    json!({}),
                )
            }),
        )
    }

    #[test]
    fn dock_targets_reject_duplicate_definition_uids() {
        // uid 是 linker 的解析键：注册表内重复 uid 会让按内容查找二义，
        // 必须在解析期响亮拒绝而不是静默取第一条。
        let definition = minimal_definition();
        let targets = json!([
            { "uid": TARGET_UID, "definition": definition.clone() },
            { "uid": TARGET_UID, "definition": definition },
        ]);
        let issues = parse_dock_targets(&targets).unwrap_err();
        assert!(
            issues
                .iter()
                .any(|issue| issue.code == "D008" && issue.message.contains("duplicate")),
            "{issues:?}"
        );
    }

    #[test]
    fn dock_targets_reject_malformed_uids() {
        // uid 是内容派生身份（^zx-[0-9a-f]{32}$）：缺失/空白/非 uid 形态
        // 都是确定性 D008，name 形态的旧寻址不再被接受。
        for (label, uid) in [
            ("missing uid", None),
            ("blank uid", Some("  ")),
            ("slug-shaped uid", Some("payment_execution")),
            ("short uid", Some("zx-aaaa")),
            ("uppercase uid", Some("zx-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA")),
        ] {
            let mut entry = target_entry(minimal_definition());
            match uid {
                Some(uid) => entry["uid"] = json!(uid),
                None => {
                    entry.as_object_mut().unwrap().remove("uid");
                }
            }
            let issues = parse_dock_targets(&json!([entry]))
                .expect_err(&format!("{label}: must be rejected"));
            assert!(
                issues
                    .iter()
                    .any(|issue| issue.code == "D008" && issue.path.ends_with(".uid")),
                "{label}: {issues:?}"
            );
        }
    }

    #[test]
    fn dock_targets_reject_invalid_definitions() {
        // definition 必须是完整合法 Zhixu 文档（deny_unknown_fields）：
        // 缺失/非对象/缺必填键/未知键都是确定性 D008，不得以零值语义入场。
        for (label, entry) in [
            ("missing definition", json!([{ "uid": TARGET_UID }])),
            (
                "definition not an object",
                json!([target_entry(json!("OOPS"))]),
            ),
            (
                "definition missing spec",
                json!([target_entry(json!({
                    "apiVersion": "uvp/v0",
                    "kind": "Zhixu",
                    "metadata": { "name": "target_definition" },
                }))]),
            ),
            (
                "definition unknown field",
                json!([target_entry(json!({
                    "apiVersion": "uvp/v0",
                    "kind": "Zhixu",
                    "metadata": { "name": "target_definition", "uid": "zx-0" },
                    "spec": {
                        "platform": { "type": "cloud" },
                        "nucleation": { "id": "target-core" },
                    },
                }))]),
            ),
        ] {
            let issues =
                parse_dock_targets(&entry).expect_err(&format!("{label}: must be rejected"));
            assert!(
                issues.iter().any(|issue| issue.code == "D008"
                    && issue.path.ends_with(".definition")
                    && issue.message.contains("not a valid Zhixu document")),
                "{label}: {issues:?}"
            );
        }
        // 基线：最小合法文档照常解析。
        parse_dock_targets(&json!([target_entry(minimal_definition())]))
            .expect("minimal definition parses");
    }

    #[test]
    fn dock_targets_reject_unknown_entry_fields() {
        // 条目级键闭集 {uid, definition}：调用方预提取时代的残留键
        // （interfaces/dockEdges）按未知字段拒绝——注入数据错误不得以
        // 缺省语义参与 link。
        let targets = json!([{
            "uid": TARGET_UID,
            "definition": minimal_definition(),
            "interfaces": [],
            "dockEdges": [],
        }]);
        let issues = parse_dock_targets(&targets).unwrap_err();
        assert!(
            issues.iter().any(|issue| issue.code == "D008"
                && issue.message.contains("unknown field")
                && issue.message.contains("\"interfaces\"")),
            "{issues:?}"
        );
        assert!(
            issues.iter().any(|issue| issue.code == "D008"
                && issue.message.contains("unknown field")
                && issue.message.contains("\"dockEdges\"")),
            "{issues:?}"
        );
        assert!(
            issues
                .iter()
                .all(|issue| issue.message.contains("allowed: [\"uid\", \"definition\"]")),
            "{issues:?}"
        );
    }

    #[test]
    fn dock_targets_extract_dock_edges_from_definition() {
        // 出边不再由调用方声明：core 从 definition 全部 executor 的静态
        // target.zhixu 单源提取（按 taskPattern 声明顺序）；缺省=无出边。
        let with_edge = json!([target_entry(zhixu_document(
            json!([zhixu_stage("work", "buyer", Some(EDGE_UID))]),
            json!({
                "svc": interface_spec(
                    json!(["new"]),
                    json!({ "execute": { "hook": "main.work#DOCK_ENTER" } }),
                    json!({}),
                )
            }),
        ))]);
        let parsed = parse_dock_targets(&with_edge).expect("edge extraction parses");
        assert_eq!(parsed.targets[0].dock_edges, vec![EDGE_UID.to_string()]);

        // 多 executor 多出边，按声明顺序。
        let multi_edge = json!([target_entry(zhixu_document(
            json!([
                zhixu_stage("work", "buyer", Some(EDGE_UID)),
                zhixu_stage("audit", "buyer", Some(TARGET_UID)),
            ]),
            json!({
                "svc": interface_spec(
                    json!(["existing"]),
                    json!({
                        "execute": { "hook": "main.work#DOCK_ENTER" },
                        "audit": { "hook": "main.audit#DOCK_AUDIT" },
                    }),
                    json!({ "done": { "signal": "buyer::main.work.cmp" } }),
                )
            }),
        ))]);
        let parsed = parse_dock_targets(&multi_edge).expect("multi-edge extraction parses");
        assert_eq!(
            parsed.targets[0].dock_edges,
            vec![EDGE_UID.to_string(), TARGET_UID.to_string()]
        );

        // target:null（动态选择）与无 executor 的 stage 均无出边。
        let no_edge = json!([target_entry(zhixu_document(
            json!([
                zhixu_stage("work", "buyer", None),
                plain_stage("plain", "buyer")
            ]),
            json!({
                "svc": interface_spec(
                    json!(["existing"]),
                    json!({ "execute": { "hook": "main.work#DOCK_ENTER" } }),
                    json!({}),
                )
            }),
        ))]);
        let parsed = parse_dock_targets(&no_edge).expect("executor-less definition parses");
        assert!(parsed.targets[0].dock_edges.is_empty());

        // 形态非法的边值（name slug）按 D008 拒绝：进入 D015 图会成为
        // 永不匹配的寻址键。
        let bad_edge = json!([target_entry(zhixu_document(
            json!([zhixu_stage("work", "buyer", Some("Not-A-Uid"))]),
            json!({
                "svc": interface_spec(
                    json!(["new"]),
                    json!({ "execute": { "hook": "main.work#DOCK_ENTER" } }),
                    json!({}),
                )
            }),
        ))]);
        let issues = parse_dock_targets(&bad_edge).unwrap_err();
        assert!(
            issues.iter().any(|issue| issue.code == "D008"
                && issue
                    .message
                    .contains("static dock edge target must be a definition uid")),
            "{issues:?}"
        );
    }

    #[test]
    fn dock_targets_require_at_least_one_interface() {
        // 目标无 dockInterface（键缺席或空 map）＝无可引用的承诺面：D008 拒绝。
        let missing = json!([{
            "uid": TARGET_UID,
            "definition": {
                "apiVersion": "uvp/v0",
                "kind": "Zhixu",
                "metadata": { "name": "target_definition" },
                "spec": {
                    "platform": { "type": "cloud" },
                    "nucleation": { "id": "target-core" },
                    "taskPatterns": [
                        { "name": "main", "stages": [{ "name": "work", "source": "buyer" }] }
                    ],
                }
            }
        }]);
        let empty = json!([target_entry(zhixu_document(
            json!([plain_stage("work", "buyer")]),
            json!({}),
        ))]);
        for (label, targets) in [
            ("missing dockInterface", missing),
            ("empty dockInterface", empty),
        ] {
            let issues =
                parse_dock_targets(&targets).expect_err(&format!("{label}: must be rejected"));
            assert!(
                issues.iter().any(|issue| issue.code == "D008"
                    && issue.path.ends_with(".definition.spec.dockInterface")
                    && issue
                        .message
                        .contains("target must publish at least one named interface")),
                "{label}: {issues:?}"
            );
        }
    }

    #[test]
    fn dock_targets_reject_invalid_interface_names() {
        // 接口名与端口名同规则（^[a-z][a-z0-9_]{0,31}$）：调用方按名引用
        // 接口，非法名是不可寻址的承诺面。
        let mut definition = minimal_definition();
        let svc = definition["spec"]["dockInterface"]["svc"].clone();
        let dock = definition["spec"]["dockInterface"].as_object_mut().unwrap();
        dock.remove("svc");
        dock.insert("Bad-Name".to_string(), svc);
        let issues = parse_dock_targets(&json!([target_entry(definition)])).unwrap_err();
        assert!(
            issues.iter().any(|issue| issue.code == "D008"
                && issue
                    .message
                    .contains("interface name must match ^[a-z][a-z0-9_]{0,31}$")),
            "{issues:?}"
        );
    }

    #[test]
    fn dock_targets_reject_invalid_port_names() {
        // 端口名闸保留（input/output 两侧）：拼错的端口名不可被 route 绑定。
        let mut definition = minimal_definition();
        definition["spec"]["dockInterface"]["svc"]["inputs"]["Bad-Port"] =
            json!({ "hook": "main.work#DOCK_ENTER" });
        let issues = parse_dock_targets(&json!([target_entry(definition)])).unwrap_err();
        assert!(
            issues.iter().any(|issue| issue.code == "D008"
                && issue.path.contains(".inputs.Bad-Port")
                && issue
                    .message
                    .contains("port name must match ^[a-z][a-z0-9_]{0,31}$")),
            "{issues:?}"
        );

        let mut definition = minimal_definition();
        definition["spec"]["dockInterface"]["svc"]["outputs"]["Bad-Port"] =
            json!({ "signal": "buyer::main.work.cmp" });
        let issues = parse_dock_targets(&json!([target_entry(definition)])).unwrap_err();
        assert!(
            issues.iter().any(|issue| issue.code == "D008"
                && issue.path.contains(".outputs.Bad-Port")
                && issue
                    .message
                    .contains("port name must match ^[a-z][a-z0-9_]{0,31}$")),
            "{issues:?}"
        );
    }

    #[test]
    fn dock_targets_reject_unknown_interface_fields() {
        // 接口 spec 键闭集 {orderModes, inputs, outputs}（模型层）：
        // 残留拼错键让原文不是合法 Zhixu 文档，按 D008 拒绝。
        let mut definition = minimal_definition();
        definition["spec"]["dockInterface"]["svc"]["artifactHash"] = json!("0x00");
        let issues = parse_dock_targets(&json!([target_entry(definition)])).unwrap_err();
        assert!(
            issues.iter().any(|issue| issue.code == "D008"
                && issue.message.contains("not a valid Zhixu document")),
            "{issues:?}"
        );
    }

    #[test]
    fn dock_targets_reject_invalid_order_mode_entries() {
        // orderModes 形状（数组/字符串项）在模型反序列化即拒绝；
        // {new, existing} 闭集与无重复由 core 校验（D025 同口径）。
        let with_modes = |order_modes: Value| {
            let mut definition = minimal_definition();
            definition["spec"]["dockInterface"]["svc"]["orderModes"] = order_modes;
            json!([target_entry(definition)])
        };
        for (label, modes) in [
            ("non-string entry", json!(["new", 123])),
            ("not an array", json!("new")),
        ] {
            let issues = parse_dock_targets(&with_modes(modes))
                .expect_err(&format!("{label}: must be rejected"));
            assert!(
                issues.iter().any(|issue| issue.code == "D008"
                    && issue.path.ends_with(".definition")
                    && issue.message.contains("not a valid Zhixu document")),
                "{label}: {issues:?}"
            );
        }
        for (label, modes) in [
            ("closed-set violation", json!(["reused"])),
            ("empty modes", json!([])),
            ("duplicate modes", json!(["new", "new"])),
        ] {
            let issues = parse_dock_targets(&with_modes(modes))
                .expect_err(&format!("{label}: must be rejected"));
            assert!(
                issues.iter().any(|issue| issue.code == "D008"
                    && issue.path.ends_with(".orderModes")
                    && issue
                        .message
                        .contains("non-empty subset of {new, existing}")),
                "{label}: {issues:?}"
            );
        }
    }

    #[test]
    fn dock_target_input_source_is_derived_from_owning_stage() {
        // input 端口不再自报 source：core 从 hook 引用末段 stage 的 source
        // 单源推导；source 键成了调用方不可写字段（模型闭集拒绝）。
        let base = json!([target_entry(zhixu_document(
            json!([plain_stage("work", "buyer")]),
            json!({
                "svc": interface_spec(
                    json!(["new"]),
                    json!({ "execute": { "hook": "main.work#DOCK_ENTER" } }),
                    json!({}),
                )
            }),
        ))]);
        let parsed = parse_dock_targets(&base).expect("derivable source parses");
        let input = &parsed.targets[0].interfaces[0].inputs[0];
        assert_eq!(input.source, "buyer");
        assert_eq!(input.hook, "main.work#DOCK_ENTER");

        // hook 引用不存在的 stage：推导无主，D008 响亮拒绝。
        let mut ghost = minimal_definition();
        ghost["spec"]["dockInterface"]["svc"]["inputs"]["execute"]["hook"] =
            json!("main.ghost#DOCK_ENTER");
        let issues = parse_dock_targets(&json!([target_entry(ghost)])).unwrap_err();
        assert!(
            issues.iter().any(|issue| issue.code == "D008"
                && issue.path.ends_with(".hook")
                && issue.message.contains("references stage")
                && issue
                    .message
                    .contains("does not exist in the target definition")),
            "{issues:?}"
        );

        // hook 形状非法。
        let mut malformed = minimal_definition();
        malformed["spec"]["dockInterface"]["svc"]["inputs"]["execute"]["hook"] = json!("main.work");
        let issues = parse_dock_targets(&json!([target_entry(malformed)])).unwrap_err();
        assert!(
            issues.iter().any(|issue| issue.code == "D008"
                && issue
                    .message
                    .contains("hook must be <task>.<stage>#<receiveHookName>")),
            "{issues:?}"
        );

        // source 兄弟键不再是调用方字段：出现即未知字段（无效文档）。
        let mut with_source = minimal_definition();
        with_source["spec"]["dockInterface"]["svc"]["inputs"]["execute"]["source"] = json!("buyer");
        let issues = parse_dock_targets(&json!([target_entry(with_source)])).unwrap_err();
        assert!(
            issues.iter().any(|issue| issue.code == "D008"
                && issue.message.contains("not a valid Zhixu document")),
            "{issues:?}"
        );
    }

    #[test]
    fn dock_targets_reject_invalid_interface_shapes() {
        // inputs/outputs 类型非法（字符串/数组）在模型反序列化即拒绝：
        // 原文不是合法 Zhixu 文档，不得静默吞成空 map 按"无端口"继续 link。
        for (label, key, bad) in [
            ("inputs string", "inputs", json!("OOPS-NOT-AN-OBJECT")),
            ("inputs array", "inputs", json!([])),
            ("outputs string", "outputs", json!("OOPS-NOT-AN-OBJECT")),
            ("outputs number", "outputs", json!(7)),
        ] {
            let mut definition = minimal_definition();
            definition["spec"]["dockInterface"]["svc"][key] = bad;
            let issues = parse_dock_targets(&json!([target_entry(definition)]))
                .err()
                .unwrap_or_else(|| panic!("{label}: must be rejected"));
            assert!(
                issues.iter().any(|issue| issue.code == "D008"
                    && issue.path.ends_with(".definition")
                    && issue.message.contains("not a valid Zhixu document")),
                "{label}: {issues:?}"
            );
        }
        // 基线：键缺席（可选）与对象形态照常解析。
        parse_dock_targets(&json!([target_entry(minimal_definition())]))
            .expect("object-shaped inputs/outputs parse");
    }

    #[test]
    fn link_rejects_cross_source_seams_from_both_sides() {
        // D012 从 input 端口 source（core 由 owning stage 推导）与 output
        // signal 前缀双侧观测 seam——被绑定接口的任一侧跨源即拒绝。
        let unlinked = vec![UnlinkedDockRoute {
            stage_identifier: "local.stage".to_string(),
            stage_source: "local-src".to_string(),
            config: ZhixuExecutorConfig {
                target_uid: Some(TARGET_UID.to_string()),
                interface_name: "svc".to_string(),
                order_mode: ORDER_MODE_NEW.to_string(),
                input_map: BTreeMap::from([("ENTER".to_string(), "execute".to_string())]),
                signal_map: BTreeMap::new(),
            },
        }];

        // 基线：双侧同源放行。
        let single_seam_targets = json!([target_entry(zhixu_document(
            json!([plain_stage("work", "alpha")]),
            json!({
                "svc": interface_spec(
                    json!(["new"]),
                    json!({ "execute": { "hook": "main.work#DOCK_ENTER" } }),
                    json!({}),
                )
            }),
        ))]);
        let targets = parse_dock_targets(&single_seam_targets).unwrap();
        link_dock_routes("local", &unlinked, &targets)
            .expect("single-seam interface on both sides links");

        // 未绑定的跨源 input 端口不参与 seam（文法 §8.4：seam 只覆盖本
        // route 引用的端口）：接口另有一个 beta 源端口，但本 route 只绑定
        // execute——照常链接。旧行为（被绑定接口的全部 input 端口都参与
        // 观测）会把该形态误拒。
        let unbound_cross = json!([target_entry(zhixu_document(
            json!([plain_stage("work", "alpha"), plain_stage("audit", "beta")]),
            json!({
                "svc": interface_spec(
                    json!(["new", "existing"]),
                    json!({
                        "execute": { "hook": "main.work#DOCK_ENTER" },
                        "audit": { "hook": "main.audit#DOCK_AUDIT" },
                    }),
                    json!({}),
                )
            }),
        ))]);
        let targets = parse_dock_targets(&unbound_cross).unwrap();
        link_dock_routes("local", &unlinked, &targets)
            .expect("unbound cross-source input port must not join the seam");

        // route 绑定的两个 input 端口跨源（existing 模式允许 0..N 条 input
        // 绑定）：同一执行通道内混入两个 source 类，D012 拒绝。
        let inputs_cross_unlinked = vec![UnlinkedDockRoute {
            stage_identifier: "local.stage".to_string(),
            stage_source: "local-src".to_string(),
            config: ZhixuExecutorConfig {
                target_uid: Some(TARGET_UID.to_string()),
                interface_name: "svc".to_string(),
                order_mode: ORDER_MODE_EXISTING.to_string(),
                input_map: BTreeMap::from([
                    ("ENTER".to_string(), "execute".to_string()),
                    ("AUDIT".to_string(), "audit".to_string()),
                ]),
                signal_map: BTreeMap::new(),
            },
        }];
        let targets = parse_dock_targets(&unbound_cross).unwrap();
        let issues = link_dock_routes("local", &inputs_cross_unlinked, &targets).unwrap_err();
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
                target_uid: Some(TARGET_UID.to_string()),
                interface_name: "svc".to_string(),
                order_mode: ORDER_MODE_NEW.to_string(),
                input_map: BTreeMap::from([("ENTER".to_string(), "execute".to_string())]),
                signal_map: BTreeMap::from([("done_sig".to_string(), "done".to_string())]),
            },
        }];
        let sides_cross = json!([target_entry(zhixu_document(
            json!([plain_stage("work", "alpha")]),
            json!({
                "svc": interface_spec(
                    json!(["new"]),
                    json!({ "execute": { "hook": "main.work#DOCK_ENTER" } }),
                    json!({ "done": { "signal": "beta::main.work.cmp" } }),
                )
            }),
        ))]);
        let targets = parse_dock_targets(&sides_cross).unwrap();
        let issues = link_dock_routes("local", &sides_cross_unlinked, &targets).unwrap_err();
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
    fn route_depth_ignores_disconnected_dock_edges() {
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
            "sendSignals": [{ "name": "str" }],
            "executor": {
                "supplierType": "organization",
                "supplierID": "payment-gateway",
                "zhixuExecutorConfig": {
                    "target": { "zhixu": TARGET_UID },
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
            "sendSignals": [{ "name": "cmp" }],
            "executor": { "supplierType": "organization", "supplierID": "org" }
        }))
        .expect("stage decodes");
        let routes =
            collect_unlinked_routes(&[("task.plain".to_string(), stage)]).expect("clean executor");
        assert!(routes.is_empty());
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

        // dockTargets 目标侧同口径：output signal 空段是确定性 D008。
        let targets = json!([target_entry(zhixu_document(
            json!([plain_stage("work", "buyer")]),
            json!({
                "svc": interface_spec(
                    json!(["new"]),
                    json!({ "execute": { "hook": "main.work#DOCK_ENTER" } }),
                    json!({ "done": { "signal": "buyer::main..cmp" } }),
                )
            }),
        ))]);
        let issues = parse_dock_targets(&targets).unwrap_err();
        assert!(
            issues.iter().any(|issue| issue.code == "D008"
                && issue
                    .message
                    .contains("must be <source>::<task>.<stage>.<signal>")),
            "{issues:?}"
        );
    }
}
