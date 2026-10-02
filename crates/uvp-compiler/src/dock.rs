//! Zhixu Dock 委托协议 DSL 壳的结构性语义。

use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use uvp_hook_dsl::{parse_hook, Gate, ParseHookRequest, Profile};
use uvp_model::{DockInterfaceSpec, ZhixuStage};

pub const DOCK_ROUTE_SCHEMA_VERSION: &str = "uvp.dockRoute.v3";
pub const DOCK_ROUTE_UNRESOLVED_SCHEMA_VERSION: &str = "uvp.dockRoute.unresolved.v1";

pub const MAX_DOCK_INPUTS: usize = 8;
pub const MAX_DOCK_OUTPUTS: usize = 16;
pub const MAX_DOCK_DEPTH: u8 = 8;
pub const MAX_PORT_NAME_BYTES: usize = 32;

pub const MAX_SIGNAL_MAP_KEY_LENGTH: usize = 26;

pub const MAX_SIGNAL_NAME_BYTES: usize = 100;

pub const MAX_INTERFACE_PORTS: usize = 64;

pub const MAX_MANIFEST_DEFINITIONS: usize = 256;
pub const ORDER_MODE_NEW: &str = "new";
pub const ORDER_MODE_EXISTING: &str = "existing";
pub const ORDER_MODES: [&str; 2] = [ORDER_MODE_NEW, ORDER_MODE_EXISTING];

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

#[derive(Debug, Clone)]
pub struct ZhixuExecutorConfig {
    pub target_uid: Option<String>,
    pub interface_name: String,
    pub order_mode: String,
    pub input_map: BTreeMap<String, String>,
    pub signal_map: BTreeMap<String, String>,
}

pub fn parse_zhixu_executor_config(
    executor_value: &Value,
    stage: &ZhixuStage,
    stage_identifier: &str,
) -> DockResult<ZhixuExecutorConfig> {
    let path = format!("{stage_identifier}.executor.zhixuExecutorConfig");
    let mut issues = Vec::new();

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

    if parsed_input.is_empty() && parsed_signal.is_empty() {
        issues.push(DockIssue::new(
            "D019",
            &path,
            "at least one of inputMap/signalMap must bind a port (a route maps an input or an output; business str/cmp are not required)",
        ));
    }

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
        .is_some_and(|e| e.supplier_type == "zhixu")
}

#[derive(Debug, Clone)]
pub struct UnlinkedDockRoute {
    pub stage_identifier: String,
    pub stage_source: String,
    pub config: ZhixuExecutorConfig,
}

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

#[derive(Debug, Clone)]
pub struct InterfacePortInput {
    pub port: String,
    pub source: String,
    pub hook: String,
}

#[derive(Debug, Clone)]
pub struct InterfacePortOutput {
    pub port: String,
    pub signal: String,
}

#[derive(Debug, Clone)]
pub struct InterfaceDeclaration {
    pub name: String,
    pub order_modes: Vec<String>,
    pub inputs: Vec<InterfacePortInput>,
    pub outputs: Vec<InterfacePortOutput>,
}

#[derive(Debug, Clone)]
pub struct CompiledInterfaces {
    pub declarations: Vec<InterfaceDeclaration>,
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

pub fn compile_dock_interface(
    dock: &BTreeMap<String, DockInterfaceSpec>,
    entries: &[(String, ZhixuStage)],
) -> DockResult<CompiledInterfaces> {
    let mut issues = Vec::new();

    let stages_by_identifier: BTreeMap<&str, &ZhixuStage> = entries
        .iter()
        .map(|(identifier, stage)| (identifier.as_str(), stage))
        .collect();
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
    interfaces.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(CompiledInterfaces {
        declarations: interfaces,
        entrance_fact_keys,
    })
}

pub fn interface_declarations_json(interfaces: &[InterfaceDeclaration]) -> Value {
    Value::Array(
        interfaces
            .iter()
            .map(InterfaceDeclaration::to_json)
            .collect(),
    )
}

pub fn input_port_hook_ids(interfaces: &[InterfaceDeclaration]) -> BTreeSet<String> {
    interfaces
        .iter()
        .flat_map(|interface| interface.inputs.iter())
        .map(|port| port.hook.clone())
        .collect()
}

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
    if parts.len() != 3 || source.is_empty() || parts.iter().any(|part| part.is_empty()) {
        return None;
    }
    Some((
        source.to_string(),
        format!("{}.{}", parts[0], parts[1]),
        parts[2].to_string(),
    ))
}

#[derive(Debug, Clone)]
pub struct DockTarget {
    pub uid: String,
    pub interfaces: Vec<InterfaceDeclaration>,
    pub dock_edges: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct DockTargets {
    pub targets: Vec<DockTarget>,
}

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

pub fn parse_dock_targets(value: &Value) -> DockResult<DockTargets> {
    let mut issues = Vec::new();
    let mut targets = Vec::new();
    let entries = value.as_array();
    let mut uids_seen = BTreeSet::new();
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

#[derive(Debug, Clone)]
pub struct DockRouteInput {
    pub hook_id: String,
    pub target_port: String,
}

#[derive(Debug, Clone)]
pub struct DockRouteOutput {
    pub signal: String,
    pub target_port: String,
}

#[derive(Debug, Clone)]
pub struct DockRoute {
    pub stage_identifier: String,
    pub target_uid: String,
    pub target_interface_name: String,
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

    const TARGET_UID: &str = "zx-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const EDGE_UID: &str = "zx-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

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

    #[test]
    fn dock_target_input_source_survives_duplicate_bare_stage_names() {
        let targets = json!([target_entry(two_pattern_document_with_same_stage_names())]);
        let parsed = parse_dock_targets(&targets).expect("duplicate bare stage names parse");
        let inputs = &parsed.targets[0].interfaces[0].inputs;
        assert_eq!(inputs.len(), 1);
        assert_eq!(
            inputs[0].source, "wwt",
            "seam must follow the qualified owning stage"
        );
    }

    fn plain_stage(name: &str, source: &str) -> Value {
        json!({ "name": name, "source": source })
    }

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
        parse_dock_targets(&json!([target_entry(minimal_definition())]))
            .expect("minimal definition parses");
    }

    #[test]
    fn dock_targets_reject_unknown_entry_fields() {
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
    fn link_rejects_cross_source_seams_from_both_sides() {
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
    fn canonical_signal_shapes_reject_empty_segments() {
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
