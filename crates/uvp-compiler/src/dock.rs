//! Zhixu Dock 委托协议（PRD_100/PRD_102）DSL 壳的结构性语义。
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
use uvp_hook_dsl::{parse_hook, DependencyKind, HookMode, ParseHookRequest, Profile};
use uvp_model::{DockInterfaceSpec, ZhixuStage};

// ---------------------------------------------------------------------------
// 冻结常量
// ---------------------------------------------------------------------------

pub const DOCK_ROUTE_SCHEMA_VERSION: &str = "uvp.dockRoute.v2";
/// 未解析 route（target:null 动态选择）的声明面产物形态：本地声明完整、
/// 目标空缺，云轨运行时由选择记录补齐（PRD_100 §10.3）。
pub const DOCK_ROUTE_UNRESOLVED_SCHEMA_VERSION: &str = "uvp.dockRoute.unresolved.v1";
pub const DOCK_RESOLUTION_SCHEMA_VERSION: &str = "uvp.dock.resolution.v2";

pub const MAX_DOCK_INPUTS: usize = 8;
pub const MAX_DOCK_OUTPUTS: usize = 16;
/// Maximum number of definitions in a statically linked startup path. Runtime
/// adapters must enforce the same limit against the actual parent instance
/// depth as well; this linker check cannot observe runtime-created orders.
pub const MAX_DOCK_DEPTH: u8 = 8;
/// `^[a-z][a-z0-9_]{0,31}$`：端口名与接口名同规则（PRD_100 §9.3）。
pub const MAX_PORT_NAME_BYTES: usize = 32;

/// signalMap key 上限：运行期 hook 命名空间 = "signalMap." + key（10 字节
/// 前缀）而 hook_name 列宽 36 ⇒ key 上限 26。与 Go 镜像统一口径，避免
/// 27-36 字节 key 在一侧收、另一侧放的分裂。
pub const MAX_SIGNAL_MAP_KEY_LENGTH: usize = 26;

/// canonical 三段式信号名（task.stage.signal）落
/// individual_record.signal_name / hook_dependency.signal_name 的列宽。
/// stage 标识符 + "." + signalMap key 的组合长度按同值钉死。
pub const MAX_SIGNAL_NAME_BYTES: usize = 100;

/// order.mode 与接口 orderModes 的闭集取值（PRD_100 §11）。
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

    // D001：zhixu executor 禁止 supplierID。
    if executor_value
        .get("supplierID")
        .and_then(Value::as_str)
        .is_some_and(|id| !id.trim().is_empty())
    {
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
                    if !crate::is_name_slug(name) {
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
        if !stage.send_signals.contains(signal_name) {
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

    // D019：至少声明一项输入或输出映射（PRD_100 §10.1）。
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
    executor
        .as_ref()
        .is_some_and(|e| e.supplier_type.trim() == "zhixu")
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
    /// 未解析 route（target:null）的声明面产物：本地声明完整、目标空缺
    /// （PRD_100 §10.3）。不携带任何派生字段——目标身份/承诺由各轨在
    /// 选择记录补齐目标后自行计算。
    pub fn unresolved_json(&self) -> Value {
        json!({
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
        })
    }
}

// ---------------------------------------------------------------------------
// 目标接口（spec.dockInterface → 中性接口声明）
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct InterfacePortInput {
    pub port: String,
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

impl InterfaceDeclaration {
    fn to_json(&self) -> Value {
        let inputs = Map::from_iter(
            self.inputs
                .iter()
                .map(|port| (port.port.clone(), json!({ "hook": port.hook }))),
        );
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
/// 哈希或身份字段。
pub fn compile_dock_interface(
    dock: &BTreeMap<String, DockInterfaceSpec>,
    entries: &[(String, ZhixuStage)],
) -> DockResult<Vec<InterfaceDeclaration>> {
    let mut issues = Vec::new();

    let stages_by_identifier: BTreeMap<&str, &ZhixuStage> = entries
        .iter()
        .map(|(identifier, stage)| (identifier.as_str(), stage))
        .collect();
    // mailbox hook 全定义唯一发布：同一物理入口被两个公开端口重复发布会
    // 让外部投递出现两条可寻址路径。
    let mut hooks_claimed: BTreeMap<String, String> = BTreeMap::new();
    let mut interfaces = Vec::new();

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

        let mut inputs = Vec::new();
        let mut outputs = Vec::new();

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
            let single_atom = parsed.mode == HookMode::Normal
                && parsed.dependencies.len() == 1
                && parsed.dependencies[0].kind == DependencyKind::Positive
                && parsed.dependencies[0].delay_seconds.is_none();
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

            inputs.push(InterfacePortInput {
                port: port_name.clone(),
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
            if !stage.send_signals.contains(&signal_name) {
                issues.push(DockIssue::new(
                    "D014",
                    format!("{path}.signal"),
                    format!(
                        "signal {signal_name} is not in stage {stage_identifier} sendSignals"
                    ),
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
    Ok(interfaces)
}

/// 目标定义全部接口的中性声明产物（接口名升序数组）。
pub fn interface_declarations_json(interfaces: &[InterfaceDeclaration]) -> Value {
    Value::Array(interfaces.iter().map(InterfaceDeclaration::to_json).collect())
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
    if parts.len() != 3 || source.is_empty() {
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
}

#[derive(Debug, Clone)]
pub struct ResolutionManifest {
    pub targets: Vec<ResolutionTarget>,
}

/// 解析 resolution manifest（Store/发布系统或离线 lock 文件提供）。
/// manifest 是中性 name→interfaces 目录：manifest 内 name 重名是发布方
/// 数据错误，响亮拒绝；name 到实体的解析权威在各轨。
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
    for (index, entry) in value
        .get("definitions")
        .and_then(Value::as_array)
        .unwrap_or(&Vec::new())
        .iter()
        .enumerate()
    {
        let path = format!("resolutionManifest.definitions[{index}]");
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
        if !crate::is_name_slug(&name) {
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
        targets.push(ResolutionTarget { name, interfaces });
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
    let order_modes = object
        .get("orderModes")
        .and_then(Value::as_array)
        .map(|modes| {
            modes
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if !valid_order_modes(&order_modes) {
        issues.push(DockIssue::new(
            "D008",
            "orderModes",
            "orderModes must be a non-empty subset of {new, existing} without duplicates",
        ));
    }

    let mut inputs = Vec::new();
    for (port_name, port) in object
        .get("inputs")
        .and_then(Value::as_object)
        .unwrap_or(&Map::new())
    {
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
                "input port must be an object {hook: <task>.<stage>#<hookName>}",
            ));
            continue;
        };
        for key in port_object.keys() {
            if key != "hook" {
                issues.push(DockIssue::new(
                    "D008",
                    format!("{port_path}.{key}"),
                    format!("unknown field {key:?}; allowed: [\"hook\"]"),
                ));
            }
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
            hook: hook.to_string(),
        });
    }

    let mut outputs = Vec::new();
    for (port_name, port) in object
        .get("outputs")
        .and_then(Value::as_object)
        .unwrap_or(&Map::new())
    {
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
                format!(
                    "signal must be <source>::<task>.<stage>.<signal>, found {signal:?}"
                ),
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
/// （D008-D012、D015-D016、D020）。纯函数，无网络、无 I/O。
/// 目标按 name 查找；跨定义的内容校验（身份重算/哈希比对）是各轨解析
/// 面的内务，不在 core。
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
                "target is null (dynamic selection): a statically linked compilation cannot resolve this route — cloud runtimes fill dynamic targets from selection records (PRD_100 §10.3)",
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
                    config.interface_name,
                    interface.order_modes,
                    config.order_mode
                ),
            ));
            issues.extend(route_issues);
            continue;
        };

        // D009（端口存在 + 方向）。
        let mut resolved_inputs = Vec::new();
        for (local_hook, port_name) in &config.input_map {
            if interface
                .inputs
                .iter()
                .all(|port| &port.port != port_name)
            {
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
            if interface
                .outputs
                .iter()
                .all(|port| &port.port != port_name)
            {
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

        // D012：被绑定端口同一 source seam。input 端口的 hook 引用不含
        // source 维度（声明面就无此信息），seam 只能从 output 端口的
        // canonical signal 前缀观测：绑定 output 时必须全部落在同一 seam。
        let mut seams = BTreeSet::new();
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
                    "all output ports bound by one route must share a single target source seam, found {seams:?}"
                ),
            ));
            issues.extend(route_issues);
            continue;
        }

        // D016：binding 数量上限。
        if resolved_inputs.len() > MAX_DOCK_INPUTS {
            route_issues.push(DockIssue::new(
                "D016",
                &path,
                format!(
                    "route references {} input bindings, limit is {MAX_DOCK_INPUTS}",
                    resolved_inputs.len()
                ),
            ));
        }
        if resolved_outputs.len() > MAX_DOCK_OUTPUTS {
            route_issues.push(DockIssue::new(
                "D016",
                &path,
                format!(
                    "route references {} output bindings, limit is {MAX_DOCK_OUTPUTS}",
                    resolved_outputs.len()
                ),
            ));
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

    // D015：route 启动图无环且深度受限。linker 只观测得到本地定义的出边
    //（manifest 不携带目标的下游边），跨定义环检测由持有完整图的各轨
    // 解析面承担；本地可见的自环（name 解析键回指自身）在此拒绝。
    let mut edges: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let local_node = local_name.to_string();
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

    fn minimal_interface(name: &str) -> Value {
        json!({
            "name": name,
            "orderModes": ["new"],
            "inputs": { "execute": { "hook": "main.work#DOCK_ENTER" } },
            "outputs": {},
        })
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
}
