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
        .cloned()
        .unwrap_or_else(|| "1".to_string());
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
    validation_issues.extend(validate_trigger_references(&stage_entries));
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
                .unwrap_or_default();
            if let Some(signal_map) = entry
                .stage
                .executor
                .as_ref()
                .and_then(|executor| executor.get("zhixuExecutorConfig"))
                .and_then(|value| value.get("signalMap"))
                .and_then(Value::as_object)
            {
                for (hook_name, raw) in signal_map {
                    if let Some(raw_expression) = raw.as_str() {
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
    issues
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

fn validate_trigger_references(entries: &[StageEntry]) -> Vec<String> {
    let mut issues = Vec::new();
    for entry in entries {
        let trigger_keys = normalize_trigger_keys(&entry.stage.trigger);
        if trigger_keys.is_empty() {
            issues.push(format!(
                "{}.trigger must contain at least one receiveSignals key",
                entry.stage_identifier
            ));
            continue;
        }
        for trigger_key in trigger_keys {
            if !entry.stage.receive_signals.contains_key(&trigger_key) {
                issues.push(format!(
                    "{}.trigger references missing receiveSignals key {}",
                    entry.stage_identifier, trigger_key
                ));
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
) -> Vec<String> {
    let mut issues = Vec::new();
    let mut seen = BTreeSet::new();
    for dependency in &hook.dependencies {
        let key = format!("{}::{}", dependency.source, dependency.signal_name);
        if !seen.insert(key) {
            continue;
        }
        if dependency.signal_name == "OUTSIDE" || dependency.signal_name == "OUTSOURCE" {
            continue;
        }
        if !catalog.local_sources.contains(&dependency.source) {
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
    let trigger_keys = normalize_trigger_keys(&entry.stage.trigger)
        .into_iter()
        .collect::<BTreeSet<_>>();
    for (hook_name, raw_expression) in &entry.stage.receive_signals {
        hooks.push(compile_hook(
            "receive",
            &entry.stage_identifier,
            hook_name,
            trigger_keys.contains(hook_name),
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

fn normalize_trigger_keys(trigger: &[String]) -> Vec<String> {
    trigger
        .iter()
        .map(|item| item.trim())
        .filter(|item| !item.is_empty())
        .map(ToOwned::to_owned)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
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
            "0x4964b6a9999d90aca565c1c555db99d428a606868439ecad7b4d8debde338a64"
        );
        assert_eq!(plan["compiledHooks"].as_array().unwrap().len(), 5);
        assert_eq!(
            plan["compiledHooks"]
                .as_array()
                .unwrap()
                .iter()
                .map(|hook| hook["hookId"].as_str().unwrap())
                .collect::<Vec<_>>(),
            vec![
                "selector.assign#TRIGGER",
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
                            "trigger": ["TRIGGER"],
                            "receiveSignals": { "TRIGGER": "::OUTSIDE" },
                            "selectedStages": ["execution.main"],
                            "sendSignals": ["executor_selected"],
                            "executor": { "supplierType": "organization", "supplierID": "selector-org" }
                        }
                    ]},
                    { "name": "execution", "stages": [
                        {
                            "name": "main",
                            "source": "buyer",
                            "trigger": ["START"],
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
