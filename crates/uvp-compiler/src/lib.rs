use serde::Deserialize;
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use thiserror::Error;
use uvp_hook_dsl::{
    parse_hook, Compatibility, DependencyKind, ParseHookOutput, ParseHookRequest, Profile,
};
use uvp_ir::hash_canonical;
use uvp_model::{ZhixuDefinition, ZhixuStage};

const COMPILER_NAME: &str = "uvp-eth-compiler";
const COMPILER_VERSION: &str = "0.1.0";
const HOOK_PLAN_SCHEMA_VERSION: &str = "uvp.hookPlan.v1";
const MAX_SIGNAL_MAP_KEY_LENGTH: usize = 36;

#[derive(Debug, Error)]
pub enum CompilerError {
    #[error("{0}")]
    Message(String),
    #[error("compilation failed: {0}")]
    Issues(String),
}

type Result<T> = std::result::Result<T, CompilerError>;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompileRequest {
    #[serde(default = "default_target")]
    pub target: String,
    #[serde(alias = "zhixu")]
    pub definition: Value,
}

fn default_target() -> String {
    "hook_plan".to_string()
}

pub fn compile_json(input: &str) -> String {
    let result = serde_json::from_str::<CompileRequest>(input)
        .map_err(|err| CompilerError::Message(format!("invalid compile request: {err}")))
        .and_then(|req| compile_request(&req));
    envelope_json(result)
}

pub fn compile_request(req: &CompileRequest) -> Result<Value> {
    match req.target.as_str() {
        "hook_plan" | "evm" => compile_zhixu_hook_plan(&req.definition),
        "cloud" | "cloud_db" => compile_cloud_artifact(&req.definition),
        other => Err(CompilerError::Message(format!(
            "unsupported compile target {other:?}"
        ))),
    }
}

pub fn compile_zhixu_hook_plan(definition_value: &Value) -> Result<Value> {
    let definition: ZhixuDefinition = serde_json::from_value(definition_value.clone())
        .map_err(|err| CompilerError::Message(format!("invalid Zhixu definition: {err}")))?;
    let issues = validate_zhixu_shape(&definition);
    if !issues.is_empty() {
        return Err(CompilerError::Issues(issues.join("; ")));
    }

    let zhixu_id = definition
        .metadata
        .uid
        .clone()
        .unwrap_or_else(|| definition.metadata.name.clone());
    let version = definition
        .metadata
        .annotations
        .get("version")
        .map(String::as_str)
        .filter(|version| !version.is_empty())
        .ok_or_else(|| {
            CompilerError::Message(
                "metadata.annotations.version is required: missing or empty version".to_string(),
            )
        })?
        .to_string();
    let platform = normalize_platform_value(&definition.spec.platform)?;
    let stage_entries = flatten_stages(&definition)?;
    let stage_ids = stage_entries
        .iter()
        .map(|entry| entry.stage_identifier.clone())
        .collect::<BTreeSet<_>>();
    let selected_stage_bindings = build_selected_stage_bindings(&stage_entries, &stage_ids)?;
    let executor_routes = build_executor_routes(&stage_entries);

    let mut validation_issues = Vec::new();
    validation_issues.extend(validate_stage_executors(
        &stage_entries,
        &selected_stage_bindings,
    ));
    validation_issues.extend(validate_mint_anchors(&stage_entries));
    validation_issues.extend(validate_receive_signal_references(&stage_entries));
    validation_issues.extend(validate_signal_maps(&stage_entries));
    if !validation_issues.is_empty() {
        return Err(CompilerError::Issues(validation_issues.join("; ")));
    }

    let mut compiled_hooks = Vec::new();
    for entry in &stage_entries {
        compiled_hooks.extend(compile_stage_hooks(entry)?);
    }
    let dependency_index = build_dependency_index(&compiled_hooks);
    let signal_capabilities = build_signal_capabilities(&stage_entries)?;
    let plan_id = hash_canonical(
        "uvp:hook-plan-id:v1",
        &json!({
            "compiler": { "name": COMPILER_NAME, "version": COMPILER_VERSION },
            "platform": platform,
            "version": version,
            "zhixuId": zhixu_id,
            "zhixuName": definition.metadata.name,
        }),
    )
    .map_err(|err| CompilerError::Message(err.to_string()))?;

    let payload = json!({
        "schemaVersion": HOOK_PLAN_SCHEMA_VERSION,
        "planId": plan_id,
        "zhixuId": zhixu_id,
        "version": version,
        "zhixuName": definition.metadata.name,
        "platform": platform,
        "compiledHooks": compiled_hooks,
        "dependencyIndex": dependency_index,
        "executorRoutes": executor_routes,
        "selectedStageBindings": selected_stage_bindings,
        "signalCapabilities": signal_capabilities,
        "source": uvp_ir::canonicalize(definition_value).map_err(|err| CompilerError::Message(err.to_string()))?,
    });
    let plan_hash = hash_canonical("uvp:hook-plan-artifact:v1", &payload)
        .map_err(|err| CompilerError::Message(err.to_string()))?;

    Ok(json!({
        "schemaVersion": HOOK_PLAN_SCHEMA_VERSION,
        "planId": plan_id,
        "zhixuId": payload["zhixuId"].clone(),
        "version": payload["version"].clone(),
        "zhixuName": payload["zhixuName"].clone(),
        "platform": payload["platform"].clone(),
        "compiledHooks": payload["compiledHooks"].clone(),
        "dependencyIndex": payload["dependencyIndex"].clone(),
        "executorRoutes": payload["executorRoutes"].clone(),
        "selectedStageBindings": payload["selectedStageBindings"].clone(),
        "signalCapabilities": payload["signalCapabilities"].clone(),
        "planHash": plan_hash,
    }))
}

pub fn compile_cloud_artifact(definition_value: &Value) -> Result<Value> {
    let definition: ZhixuDefinition = serde_json::from_value(definition_value.clone())
        .map_err(|err| CompilerError::Message(format!("invalid Zhixu definition: {err}")))?;
    let issues = validate_zhixu_shape(&definition);
    if !issues.is_empty() {
        return Err(CompilerError::Issues(issues.join("; ")));
    }

    let stage_entries = flatten_stages(&definition)?;
    let mut validation_issues = Vec::new();
    validation_issues.extend(validate_mint_anchors(&stage_entries));
    // 与 hook_plan 目标共用同一组校验：同一份定义不允许"一个 target 收、
    // 另一个放"，否则 Go 主链路会拿到被 hook_plan 拒绝的定义的产物。
    validation_issues.extend(validate_signal_maps(&stage_entries));
    validation_issues.extend(validate_receive_signal_references(&stage_entries));
    if !validation_issues.is_empty() {
        return Err(CompilerError::Issues(validation_issues.join("; ")));
    }
    let mut stages = Vec::new();
    let mut hooks = Vec::new();

    for entry in &stage_entries {
        stages.push(cloud_stage_artifact(entry)?);
        for (hook_name, raw_expression) in &entry.stage.receive_signals {
            hooks.push(cloud_hook_artifact(
                entry,
                hook_name,
                raw_expression,
                "self",
                None,
            )?);
        }

        if entry
            .stage
            .executor
            .as_ref()
            .and_then(|executor| executor.get("supplierType"))
            .and_then(Value::as_str)
            == Some("zhixu")
        {
            let supplier_id = entry
                .stage
                .executor
                .as_ref()
                .and_then(|executor| executor.get("supplierID"))
                .and_then(Value::as_str)
                .filter(|supplier_id| !supplier_id.is_empty())
                .ok_or_else(|| {
                    CompilerError::Message(format!(
                        "{}.executor: zhixu executor missing supplierID",
                        entry.stage_identifier
                    ))
                })?;
            if let Some(signal_map) = entry
                .stage
                .executor
                .as_ref()
                .and_then(|executor| executor.get("zhixuExecutorConfig"))
                .and_then(|value| value.get("signalMap"))
                .and_then(Value::as_object)
            {
                for (hook_name, raw) in signal_map {
                    let Some(raw_expression) = raw.as_str() else {
                        return Err(CompilerError::Message(format!(
                            "{}.executor.zhixuExecutorConfig.signalMap.{hook_name} is invalid: expected string",
                            entry.stage_identifier
                        )));
                    };
                    hooks.push(cloud_hook_artifact(
                        entry,
                        hook_name,
                        raw_expression,
                        "executor",
                        Some(supplier_id),
                    )?);
                }
            }
        }
    }

    Ok(json!({
        "schemaVersion": "uvp.cloudArtifact.v1",
        "zhixuName": definition.metadata.name,
        "stages": stages,
        "hooks": hooks,
        "orderStageDefaults": stages,
    }))
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct Envelope {
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    value: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    diagnostics: Option<Vec<Diagnostic>>,
}

#[derive(Debug, serde::Serialize)]
struct Diagnostic {
    message: String,
}

fn envelope_json(result: Result<Value>) -> String {
    let envelope = match result {
        Ok(value) => Envelope {
            ok: true,
            value: Some(value),
            diagnostics: None,
        },
        Err(err) => Envelope {
            ok: false,
            value: None,
            diagnostics: Some(vec![Diagnostic {
                message: err.to_string(),
            }]),
        },
    };
    serde_json::to_string(&envelope).expect("compile envelope should serialize")
}

#[derive(Debug, Clone)]
struct StageEntry {
    stage: ZhixuStage,
    stage_identifier: String,
}

fn validate_zhixu_shape(definition: &ZhixuDefinition) -> Vec<String> {
    let mut issues = Vec::new();
    if definition.api_version != "uvp/v0" {
        issues.push("apiVersion must be uvp/v0".to_string());
    }
    if definition.kind != "Zhixu" {
        issues.push("kind must be Zhixu".to_string());
    }
    if definition.metadata.name.is_empty() {
        issues.push("metadata.name is required".to_string());
    }
    if definition.spec.platform.platform_type.trim().is_empty() {
        issues.push("spec.platform must be an object with a non-empty type".to_string());
    }
    if definition.spec.task_patterns.is_empty() {
        issues.push("spec.taskPatterns must contain at least one task pattern".to_string());
    }
    for (task_index, task) in definition.spec.task_patterns.iter().enumerate() {
        if !valid_identifier_part(&task.name) {
            issues.push(format!(
                "spec.taskPatterns[{task_index}].name must start with an ASCII letter and contain only ASCII letters, digits, '_' or '-': {}",
                task.name
            ));
        }
        for (stage_index, stage) in task.stages.iter().enumerate() {
            if !valid_identifier_part(&stage.name) {
                issues.push(format!(
                    "spec.taskPatterns[{task_index}].stages[{stage_index}].name must start with an ASCII letter and contain only ASCII letters, digits, '_' or '-': {}",
                    stage.name
                ));
            }
        }
    }
    issues
}

fn valid_identifier_part(value: &str) -> bool {
    let mut chars = value.chars();
    matches!(chars.next(), Some(ch) if ch.is_ascii_alphabetic())
        && chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
}

fn flatten_stages(definition: &ZhixuDefinition) -> Result<Vec<StageEntry>> {
    let mut entries = Vec::new();
    let mut task_names = BTreeSet::new();
    for task in &definition.spec.task_patterns {
        if !task_names.insert(task.name.clone()) {
            return Err(CompilerError::Issues(format!(
                "duplicate task pattern {}",
                task.name
            )));
        }
        let mut stage_names = BTreeSet::new();
        for stage in &task.stages {
            if !stage_names.insert(stage.name.clone()) {
                return Err(CompilerError::Issues(format!(
                    "duplicate stage {}.{}",
                    task.name, stage.name
                )));
            }
            entries.push(StageEntry {
                stage: stage.clone(),
                stage_identifier: format!("{}.{}", task.name, stage.name),
            });
        }
    }
    Ok(entries)
}

fn normalize_platform_value(platform: &uvp_model::ZhixuPlatform) -> Result<Value> {
    let mut map = Map::new();
    map.insert(
        "type".to_string(),
        Value::String(platform.platform_type.clone()),
    );
    if let Some(provider) = &platform.provider {
        map.insert("provider".to_string(), Value::String(provider.clone()));
    }
    if let Some(network) = &platform.network {
        map.insert("network".to_string(), Value::String(network.clone()));
    }
    if let Some(version) = &platform.version {
        map.insert("version".to_string(), Value::String(version.clone()));
    }
    if !platform.params.is_empty() {
        map.insert(
            "params".to_string(),
            serde_json::to_value(&platform.params)
                .map_err(|err| CompilerError::Message(err.to_string()))?,
        );
    }
    Ok(Value::Object(map))
}

fn build_selected_stage_bindings(
    entries: &[StageEntry],
    stage_ids: &BTreeSet<String>,
) -> Result<Vec<Value>> {
    let mut bindings = Vec::new();
    let mut issues = Vec::new();
    let mut seen = BTreeSet::new();
    for entry in entries {
        for target in &entry.stage.selected_stages {
            if !stage_ids.contains(target) {
                issues.push(format!(
                    "{}.selectedStages references unknown stage {}",
                    entry.stage_identifier, target
                ));
                continue;
            }
            let key = format!("{}->{target}", entry.stage_identifier);
            if !seen.insert(key) {
                issues.push(format!(
                    "{}.selectedStages contains duplicate target {}",
                    entry.stage_identifier, target
                ));
                continue;
            }
            bindings.push(json!({
                "selectorStageIdentifier": entry.stage_identifier,
                "targetStageIdentifier": target,
            }));
        }
    }
    if !issues.is_empty() {
        return Err(CompilerError::Issues(issues.join("; ")));
    }
    bindings.sort_by(|left, right| {
        value_str(left, "selectorStageIdentifier")
            .cmp(value_str(right, "selectorStageIdentifier"))
            .then(
                value_str(left, "targetStageIdentifier")
                    .cmp(value_str(right, "targetStageIdentifier")),
            )
    });
    Ok(bindings)
}

fn build_executor_routes(entries: &[StageEntry]) -> Value {
    let mut routes = Map::new();
    for entry in entries {
        if entry.stage.executor.is_some() {
            routes.insert(entry.stage_identifier.clone(), route_for_stage(entry));
        }
    }
    Value::Object(routes)
}

fn validate_stage_executors(entries: &[StageEntry], bindings: &[Value]) -> Vec<String> {
    let mut issues = Vec::new();
    let mut targets_by_selector: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for binding in bindings {
        targets_by_selector
            .entry(value_str(binding, "selectorStageIdentifier").to_string())
            .or_default()
            .push(value_str(binding, "targetStageIdentifier").to_string());
    }
    let mut anchored = BTreeSet::new();
    let mut queue = VecDeque::new();
    for entry in entries {
        if has_static_executor(entry.stage.executor.as_ref()) {
            anchored.insert(entry.stage_identifier.clone());
            queue.push_back(entry.stage_identifier.clone());
        }
    }
    while let Some(selector) = queue.pop_front() {
        for target in targets_by_selector.get(&selector).into_iter().flatten() {
            if anchored.insert(target.clone()) {
                queue.push_back(target.clone());
            }
        }
    }
    for entry in entries {
        if has_static_executor(entry.stage.executor.as_ref())
            || anchored.contains(&entry.stage_identifier)
        {
            continue;
        }
        issues.push(format!(
            "{} has no static executor and is not reachable from a static executor through selectedStages",
            entry.stage_identifier
        ));
    }
    issues
}

fn validate_mint_anchors(entries: &[StageEntry]) -> Vec<String> {
    let mut issues = Vec::new();
    // 编译期只固定三件事：
    // 1) mint 取值合法（当前仅 per-fact）；
    // 2) mint 阶段是出生阶段：接受订阅通道（跨类事实的 per-fact 出生）与
    //    单信号普通 hook（外部提交事实的出生入口；链上即 isTrigger 钩子，
    //    triggerOrderFrom* 的提交者按"现实成立后任意持有人签名提交"开放）；
    //    布尔/否定/延时组合在铸单前没有可求值的订单上下文，仍拒绝。
    // 3) 防无界代铸链：mint 阶段的订阅目标不得指向本阶段自己的 source 类。
    // 其余阶段是否"有锚"由运行时按对接记录路由自然裁决：存在订单实例则
    // 按单投递（域内血缘/dock 边），不存在实例则按类扇入。执行器自发 str
    // 出的订单编译期不可见，因此不在此做静态锚定判断。
    for entry in entries {
        if let Some(mint) = &entry.stage.mint {
            if mint.trim() != "per-fact" {
                issues.push(format!(
                    "{}.mint only supports per-fact: {}",
                    entry.stage_identifier, mint
                ));
            }
            for (hook_name, raw_expression) in &entry.stage.receive_signals {
                match parse_hook_for_compiler("HOOK", raw_expression) {
                    Err(_) => {} // 语法错误由引用存在性校验统一上报
                    Ok(parsed) if parsed.mode == uvp_hook_dsl::HookMode::Subscription => {
                        if let Some(target) = &parsed.subscription_target {
                            if target.source == entry.stage.source {
                                issues.push(format!(
                                    "{}.receiveSignals.{hook_name}: mint stage must not subscribe its own source class {}; per-fact mint would chain without bound",
                                    entry.stage_identifier, target.source
                                ));
                            }
                        }
                    }
                    Ok(parsed) => {
                        // 出生入口 hook 必须是单正信号（无组合/否定/延时）：
                        // 出生事实本身即判定，isTrigger 计划就是一条 SIGNAL。
                        let deps = &parsed.dependencies;
                        let single_positive = deps.len() == 1
                            && deps[0].kind == uvp_hook_dsl::DependencyKind::Positive;
                        if !single_positive {
                            issues.push(format!(
                                "{}.receiveSignals.{hook_name}: mint stage birth entries must be a single plain signal (no boolean/negation/delay composition)",
                                entry.stage_identifier
                            ));
                        }
                    }
                }
            }
        }
    }
    issues
}

fn validate_receive_signal_references(entries: &[StageEntry]) -> Vec<String> {
    let mut issues = Vec::new();
    let catalog = SignalReferenceCatalog::new(entries);
    for entry in entries {
        for (hook_name, raw_expression) in &entry.stage.receive_signals {
            match parse_hook_for_compiler("HOOK", raw_expression) {
                Ok(parsed) => issues.extend(validate_hook_dependency_references(
                    &parsed,
                    &format!("{}.receiveSignals.{hook_name}", entry.stage_identifier),
                    &catalog,
                    false,
                )),
                Err(err) => issues.push(format!(
                    "{}.receiveSignals.{hook_name} is invalid: {err}",
                    entry.stage_identifier
                )),
            }
        }
    }
    issues
}

fn validate_signal_maps(entries: &[StageEntry]) -> Vec<String> {
    let mut issues = Vec::new();
    let catalog = SignalReferenceCatalog::new(entries);
    for entry in entries {
        let Some(executor) = &entry.stage.executor else {
            continue;
        };
        if executor.get("supplierType").and_then(Value::as_str) != Some("zhixu") {
            continue;
        }
        let signal_map = executor
            .get("zhixuExecutorConfig")
            .and_then(|value| value.get("signalMap"))
            .and_then(Value::as_object);
        let Some(signal_map) = signal_map else {
            issues.push(format!(
                "{}.executor.zhixuExecutorConfig.signalMap is required",
                entry.stage_identifier
            ));
            continue;
        };
        if !signal_map.contains_key("str") || !signal_map.contains_key("cmp") {
            issues.push(format!(
                "{}.signalMap must contain str and cmp",
                entry.stage_identifier
            ));
            continue;
        }
        let mut parsed = Vec::new();
        for (signal, raw) in signal_map {
            if signal.contains('.') || signal.len() > MAX_SIGNAL_MAP_KEY_LENGTH {
                issues.push(format!(
                    "{}.executor.zhixuExecutorConfig.signalMap.{signal} is invalid: key must not contain '.' and must be at most {MAX_SIGNAL_MAP_KEY_LENGTH} bytes",
                    entry.stage_identifier
                ));
                continue;
            }
            let Some(raw_expression) = raw.as_str() else {
                issues.push(format!(
                    "{}.executor.zhixuExecutorConfig.signalMap.{signal} is invalid: expected string",
                    entry.stage_identifier
                ));
                continue;
            };
            match parse_hook_for_compiler("HOOK", raw_expression) {
                Ok(hook) => parsed.push((signal.clone(), hook)),
                Err(err) => issues.push(format!(
                    "{}.executor.zhixuExecutorConfig.signalMap.{signal} is invalid: {err}",
                    entry.stage_identifier
                )),
            }
        }
        if parsed.len() != signal_map.len() {
            continue;
        }
        let sources = parsed
            .iter()
            .map(|(_, hook)| hook.source.clone())
            .collect::<BTreeSet<_>>();
        if sources.len() != 1 {
            issues.push(format!(
                "{}.signalMap must reference one source",
                entry.stage_identifier
            ));
        }
        for (signal, hook) in parsed {
            issues.extend(validate_hook_dependency_references(
                &hook,
                &format!(
                    "{}.executor.zhixuExecutorConfig.signalMap.{signal}",
                    entry.stage_identifier
                ),
                &catalog,
                true,
            ));
        }
    }
    issues
}

struct SignalReferenceCatalog {
    local_sources: BTreeSet<String>,
    stages_by_identifier: BTreeMap<String, StageEntry>,
}

impl SignalReferenceCatalog {
    fn new(entries: &[StageEntry]) -> Self {
        Self {
            local_sources: entries
                .iter()
                .map(|entry| entry.stage.source.clone())
                .collect(),
            stages_by_identifier: entries
                .iter()
                .map(|entry| (entry.stage_identifier.clone(), entry.clone()))
                .collect(),
        }
    }
}

fn validate_hook_dependency_references(
    hook: &ParseHookOutput,
    path: &str,
    catalog: &SignalReferenceCatalog,
    is_signal_map: bool,
) -> Vec<String> {
    let mut issues = Vec::new();
    let mut seen = BTreeSet::new();
    for dependency in &hook.dependencies {
        let key = format!("{}::{}", dependency.source, dependency.signal_name);
        if !seen.insert(key) {
            continue;
        }
        if !catalog.local_sources.contains(&dependency.source) {
            // 订阅寻址只在本域解析（subscription-mint-spec §2.1）：receive
            // 钩子的依赖 source 必须 ∈ 本域 source 类集合；signalMap 是跨域
            // 委托接缝上的对译表，依赖 source 来自被委托域，保留跳过。
            if !is_signal_map {
                issues.push(format!(
                    "{path} subscription source {} is not a declared source in this zhixu",
                    dependency.source
                ));
            }
            continue;
        }
        let Some((stage_identifier, signal_name)) = parse_signal_reference(&dependency.signal_name)
        else {
            continue;
        };
        let Some(referenced_stage) = catalog.stages_by_identifier.get(&stage_identifier) else {
            issues.push(format!(
                "{path} references unknown stage {stage_identifier}"
            ));
            continue;
        };
        if referenced_stage.stage.source != dependency.source {
            issues.push(format!(
                "{path} references {stage_identifier} under source {}, but stage source is {}",
                dependency.source, referenced_stage.stage.source
            ));
            continue;
        }
        if !referenced_stage.stage.send_signals.is_empty()
            && !referenced_stage.stage.send_signals.contains(&signal_name)
        {
            issues.push(format!(
                "{path} references unknown signal {stage_identifier}.{signal_name}"
            ));
        }
    }
    issues
}

fn parse_signal_reference(signal_name: &str) -> Option<(String, String)> {
    let parts = signal_name.split('.').collect::<Vec<_>>();
    if parts.len() != 3 {
        return None;
    }
    Some((format!("{}.{}", parts[0], parts[1]), parts[2].to_string()))
}

fn compile_stage_hooks(entry: &StageEntry) -> Result<Vec<Value>> {
    let mut hooks = Vec::new();
    let is_mint_stage = entry.stage.mint.is_some();
    for (hook_name, raw_expression) in &entry.stage.receive_signals {
        hooks.push(compile_hook(
            "receive",
            &entry.stage_identifier,
            hook_name,
            is_mint_stage,
            raw_expression,
            entry
                .stage
                .executor
                .as_ref()
                .map(|_| route_for_stage(entry)),
        )?);
    }
    if entry
        .stage
        .executor
        .as_ref()
        .and_then(|executor| executor.get("supplierType"))
        .and_then(Value::as_str)
        == Some("zhixu")
    {
        if let Some(signal_map) = entry
            .stage
            .executor
            .as_ref()
            .and_then(|executor| executor.get("zhixuExecutorConfig"))
            .and_then(|value| value.get("signalMap"))
            .and_then(Value::as_object)
        {
            for (signal_name, raw) in signal_map {
                if let Some(raw_expression) = raw.as_str() {
                    hooks.push(compile_hook(
                        "signalMap",
                        &entry.stage_identifier,
                        &format!("signalMap.{signal_name}"),
                        false,
                        raw_expression,
                        entry
                            .stage
                            .executor
                            .as_ref()
                            .map(|_| route_for_stage(entry)),
                    )?);
                }
            }
        }
    }
    hooks.sort_by_key(|hook| value_str(hook, "hookId").to_lowercase());
    Ok(hooks)
}

fn compile_hook(
    kind: &str,
    stage_identifier: &str,
    hook_name: &str,
    is_trigger: bool,
    raw_expression: &str,
    route: Option<Value>,
) -> Result<Value> {
    let parsed = parse_hook_for_compiler(hook_name, raw_expression)?;
    let mut hook = Map::new();
    hook.insert(
        "hookId".to_string(),
        Value::String(format!("{stage_identifier}#{hook_name}")),
    );
    hook.insert("kind".to_string(), Value::String(kind.to_string()));
    hook.insert(
        "stageIdentifier".to_string(),
        Value::String(stage_identifier.to_string()),
    );
    hook.insert("hookName".to_string(), Value::String(hook_name.to_string()));
    hook.insert("isTrigger".to_string(), Value::Bool(is_trigger));
    hook.insert(
        "rawExpression".to_string(),
        Value::String(raw_expression.to_string()),
    );
    hook.insert(
        "normalizedExpression".to_string(),
        Value::String(parsed.normalized_expression.clone()),
    );
    hook.insert("ast".to_string(), parsed.ast.clone());
    hook.insert(
        "dependencies".to_string(),
        serde_json::to_value(&parsed.dependencies)
            .map_err(|err| CompilerError::Message(err.to_string()))?,
    );
    if let Some(route) = route {
        hook.insert("route".to_string(), route);
    }
    Ok(Value::Object(hook))
}

fn parse_hook_for_compiler(hook_name: &str, raw_expression: &str) -> Result<ParseHookOutput> {
    let parsed = parse_hook(ParseHookRequest {
        profile: Profile::EvmStrict,
        hook_name: hook_name.to_string(),
        hook: raw_expression.to_string(),
    })
    .map_err(|err| CompilerError::Message(err.to_string()))?;
    if parsed.compatibility != Compatibility::Portable {
        return Err(CompilerError::Message(
            "hook expression is not portable".to_string(),
        ));
    }
    Ok(parsed)
}

fn parse_hook_for_cloud(hook_name: &str, raw_expression: &str) -> Result<ParseHookOutput> {
    parse_hook(ParseHookRequest {
        profile: Profile::CloudCompat,
        hook_name: hook_name.to_string(),
        hook: raw_expression.to_string(),
    })
    .map_err(|err| CompilerError::Message(err.to_string()))
}

fn cloud_stage_artifact(entry: &StageEntry) -> Result<Value> {
    let mut stage = Map::new();
    stage.insert(
        "stageIdentifier".to_string(),
        Value::String(entry.stage_identifier.clone()),
    );
    if let Some(executor) = &entry.stage.executor {
        stage.insert("executorConfigs".to_string(), executor.clone());
    }
    if !entry.stage.file_resources.is_empty() {
        stage.insert(
            "fileResources".to_string(),
            serde_json::to_value(&entry.stage.file_resources)
                .map_err(|err| CompilerError::Message(err.to_string()))?,
        );
    }
    if let Some(mint) = &entry.stage.mint {
        stage.insert("mint".to_string(), Value::String(mint.trim().to_string()));
    }
    Ok(Value::Object(stage))
}

fn cloud_hook_artifact(
    entry: &StageEntry,
    hook_name: &str,
    raw_expression: &str,
    source_zhixu_ref: &str,
    source_zhixu_id: Option<&str>,
) -> Result<Value> {
    let parsed = parse_hook_for_cloud(hook_name, raw_expression)?;
    let mut hook = Map::new();
    hook.insert(
        "stageIdentifier".to_string(),
        Value::String(entry.stage_identifier.clone()),
    );
    hook.insert("hookName".to_string(), Value::String(hook_name.to_string()));
    hook.insert(
        "rawExpression".to_string(),
        Value::String(parsed.raw_hook.clone()),
    );
    hook.insert(
        "logicExpression".to_string(),
        Value::String(parsed.runtime_condition.clone()),
    );
    hook.insert("astJson".to_string(), parsed.cloud_ast.clone());
    hook.insert(
        "sourceZhixuRef".to_string(),
        Value::String(source_zhixu_ref.to_string()),
    );
    if let Some(source_zhixu_id) = source_zhixu_id {
        hook.insert(
            "sourceZhixuId".to_string(),
            Value::String(source_zhixu_id.to_string()),
        );
    }
    hook.insert(
        "dependencies".to_string(),
        Value::Array(
            parsed
                .dependencies
                .iter()
                .filter(|dependency| dependency.kind != DependencyKind::Timer)
                .map(|dependency| {
                    json!({
                        "signalName": dependency.signal_name,
                        "dependencyKind": dependency.kind,
                    })
                })
                .collect(),
        ),
    );
    Ok(Value::Object(hook))
}

fn build_dependency_index(compiled_hooks: &[Value]) -> Value {
    let mut index: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for hook in compiled_hooks {
        let hook_id = value_str(hook, "hookId").to_string();
        for dependency in hook["dependencies"].as_array().into_iter().flatten() {
            let key = format!(
                "{}::{}",
                value_str(dependency, "source"),
                value_str(dependency, "signalName")
            );
            index.entry(key).or_default().insert(hook_id.clone());
        }
    }
    let mut out = Map::new();
    for (key, hook_ids) in index {
        out.insert(
            key,
            Value::Array(hook_ids.into_iter().map(Value::String).collect()),
        );
    }
    Value::Object(out)
}

fn build_signal_capabilities(entries: &[StageEntry]) -> Result<Vec<Value>> {
    let mut capabilities = Vec::new();
    let mut seen = BTreeSet::new();
    for entry in entries {
        for declared_signal in &entry.stage.send_signals {
            let capability = parse_signal_capability(entry, declared_signal)?;
            let key = format!(
                "{}\0{}\0{}\0{}",
                value_str(&capability, "stageIdentifier"),
                value_str(&capability, "targetSource"),
                value_str(&capability, "targetSignalName"),
                value_str(&capability, "targetOrderRelation")
            );
            if !seen.insert(key) {
                return Err(CompilerError::Issues(format!(
                    "{}.sendSignals contains duplicate capability {}",
                    entry.stage_identifier, declared_signal
                )));
            }
            capabilities.push(capability);
        }
    }
    capabilities.sort_by(|left, right| {
        value_str(left, "stageIdentifier")
            .cmp(value_str(right, "stageIdentifier"))
            .then(value_str(left, "targetSource").cmp(value_str(right, "targetSource")))
            .then(value_str(left, "targetSignalName").cmp(value_str(right, "targetSignalName")))
            .then(
                value_str(left, "targetOrderRelation").cmp(value_str(right, "targetOrderRelation")),
            )
    });
    Ok(capabilities)
}

fn parse_signal_capability(entry: &StageEntry, declared_signal: &str) -> Result<Value> {
    let signal = declared_signal.trim();
    if signal.is_empty() {
        return Err(CompilerError::Issues(format!(
            "{}.sendSignals cannot contain an empty signal",
            entry.stage_identifier
        )));
    }
    if let Some((target_source, target_signal_name)) = signal.split_once("::") {
        let target_source = target_source.trim();
        let target_signal_name = target_signal_name.trim();
        if target_source.is_empty() || target_signal_name.is_empty() {
            return Err(CompilerError::Issues(format!(
                "{}.sendSignals contains invalid target signal {}",
                entry.stage_identifier, declared_signal
            )));
        }
        return Ok(json!({
            "stageIdentifier": entry.stage_identifier,
            "source": entry.stage.source,
            "declaredSignal": declared_signal,
            "targetSource": target_source,
            "targetSignalName": target_signal_name,
            "targetOrderRelation": "triggerOrigin",
        }));
    }
    let target_signal_name = if signal.contains('.') {
        signal.to_string()
    } else {
        format!("{}.{}", entry.stage_identifier, signal)
    };
    Ok(json!({
        "stageIdentifier": entry.stage_identifier,
        "source": entry.stage.source,
        "declaredSignal": declared_signal,
        "targetSource": entry.stage.source,
        "targetSignalName": target_signal_name,
        "targetOrderRelation": "current",
    }))
}

fn route_for_stage(entry: &StageEntry) -> Value {
    let mut route = Map::new();
    route.insert(
        "stageIdentifier".to_string(),
        Value::String(entry.stage_identifier.clone()),
    );
    if let Some(executor) = &entry.stage.executor {
        route.insert("executor".to_string(), executor.clone());
    }
    if !entry.stage.file_resources.is_empty() {
        route.insert(
            "fileResources".to_string(),
            serde_json::to_value(&entry.stage.file_resources)
                .expect("fileResources should serialize"),
        );
    }
    Value::Object(route)
}

fn has_static_executor(executor: Option<&Value>) -> bool {
    executor
        .and_then(|value| value.get("supplierID"))
        .and_then(Value::as_str)
        .is_some_and(|value| !value.is_empty())
}

fn value_str<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn compiles_stable_hook_plan_for_demo() {
        let definition = demo_definition();
        let plan = compile_zhixu_hook_plan(&definition).unwrap();
        assert_eq!(plan["schemaVersion"], "uvp.hookPlan.v1");
        assert_eq!(plan["zhixuId"], "zhixu-demo-001");
        assert_eq!(
            plan["planId"],
            "0x472081189619bb006814fed697f3d53ff187b5a852131ba1924bde825b0b9d6d"
        );
        assert_eq!(
            plan["planHash"],
            "0x8ce322fcd43821fffe3f0144838d0b23e657a7d20acf6ee6de7bdb071a340752"
        );
        assert_eq!(plan["compiledHooks"].as_array().unwrap().len(), 4);
        assert_eq!(
            plan["compiledHooks"]
                .as_array()
                .unwrap()
                .iter()
                .map(|hook| hook["hookId"].as_str().unwrap())
                .collect::<Vec<_>>(),
            vec![
                "execution.main#signalMap.cmp",
                "execution.main#signalMap.str",
                "execution.main#START",
                "execution.main#TIMEOUT",
            ]
        );
        assert_eq!(
            plan["dependencyIndex"]["buyer::selector.assign.executor_selected"],
            json!(["execution.main#START", "execution.main#TIMEOUT"])
        );
    }

    #[test]
    fn compiles_cloud_artifact_with_go_signal_map_names() {
        let artifact = compile_cloud_artifact(&demo_definition()).unwrap();
        assert_eq!(artifact["schemaVersion"], "uvp.cloudArtifact.v1");
        assert_eq!(artifact["stages"].as_array().unwrap().len(), 2);
        let hooks = artifact["hooks"].as_array().unwrap();
        assert!(hooks.iter().any(|hook| {
            hook["stageIdentifier"] == "execution.main"
                && hook["hookName"] == "cmp"
                && hook["sourceZhixuRef"] == "executor"
                && hook["sourceZhixuId"] == "payment-zhixu"
        }));
        assert!(hooks.iter().any(|hook| {
            hook["stageIdentifier"] == "execution.main"
                && hook["hookName"] == "TIMEOUT"
                && hook["dependencies"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .all(|dep| dep["dependencyKind"] != "timer")
        }));
    }

    #[test]
    fn rejects_invalid_task_or_stage_identifier_parts() {
        for (field, value) in [
            ("taskPatterns[0].name", "execution.main"),
            ("stages[0].name", "1main"),
        ] {
            let mut definition = demo_definition();
            if field.starts_with("taskPatterns") {
                definition["spec"]["taskPatterns"][0]["name"] = json!(value);
            } else {
                definition["spec"]["taskPatterns"][1]["stages"][0]["name"] = json!(value);
            }
            let error =
                compile_zhixu_hook_plan(&definition).expect_err("invalid identifier must fail");
            assert!(error
                .to_string()
                .contains("must start with an ASCII letter"));
        }
    }

    fn demo_definition() -> Value {
        json!({
            "apiVersion": "uvp/v0",
            "kind": "Zhixu",
            "metadata": { "name": "demo_zhixu", "uid": "zhixu-demo-001", "annotations": { "version": "7" } },
            "spec": {
                "platform": { "type": "cloud" },
                "nucleation": { "id": "core" },
                "taskPatterns": [
                    { "name": "selector", "stages": [
                        {
                            "name": "assign",
                            "source": "buyer",
                            "selectedStages": ["execution.main"],
                            "sendSignals": ["executor_selected"],
                            "executor": { "supplierType": "organization", "supplierID": "selector-org" }
                        }
                    ]},
                    { "name": "execution", "stages": [
                        {
                            "name": "main",
                            "source": "buyer",
                            "receiveSignals": {
                                "START": "buyer::selector.assign.executor_selected",
                                "TIMEOUT": "buyer::(selector.assign.executor_selected +5s) & ~execution.main.cmp"
                            },
                            "sendSignals": ["str", "cmp", "err"],
                            "executor": {
                                "supplierType": "zhixu",
                                "supplierID": "payment-zhixu",
                                "zhixuExecutorConfig": {
                                    "signalMap": {
                                        "str": "payment::payment_flow.init.str",
                                        "cmp": "payment::payment_flow.settle.cmp"
                                    }
                                }
                            }
                        }
                    ]}
                ]
            }
        })
    }
}
