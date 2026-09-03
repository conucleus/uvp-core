use serde::Deserialize;
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use thiserror::Error;
use uvp_hook_dsl::{
    parse_hook, Compatibility, DependencyKind, HookMode, ParseHookOutput, ParseHookRequest, Profile,
};
use uvp_ir::hash_canonical;
use uvp_model::{ZhixuDefinition, ZhixuStage};

const COMPILER_NAME: &str = "uvp-eth-compiler";
const COMPILER_VERSION: &str = "0.1.0";
const HOOK_PLAN_SCHEMA_VERSION: &str = "uvp.hookPlan.v1";
// hook_plan 目标的 hook_name = "signalMap." + key（10 字节前缀），hook_name
// 上限 36 字节 ⇒ key 上限 26。cloud 目标曾放行到 36，同一份定义一个 target
// 收一个放；两 target 统一按 26 收口。
const MAX_SIGNAL_MAP_KEY_LENGTH: usize = 26;

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
    validation_issues.extend(validate_receive_signal_keys(&stage_entries));
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
    validation_issues.extend(validate_receive_signal_keys(&stage_entries));
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
        if has_static_executor(entry.stage.executor.as_ref()) {
            continue;
        }
        // 模-1 同族裁决（audit3 DEPLOY-01）：订阅阶段的投递目标编译期定死、
        // 运行时禁止 executor patch。selectedStages 可达只对可 patch 的普通
        // 阶段构成绑定——订阅阶段被 selector 指到也不豁免，否则定义可编译
        // 却没有 executor route、又禁补绑，永远无法形成可执行静态绑定。
        if stage_is_subscription(&entry.stage) {
            issues.push(format!(
                "{} is a subscription stage and requires its own static executor; selectedStages reachability cannot bind it because subscription stages reject runtime executor patches",
                entry.stage_identifier
            ));
            continue;
        }
        if anchored.contains(&entry.stage_identifier) {
            continue;
        }
        issues.push(format!(
            "{} has no static executor and is not reachable from a static executor through selectedStages",
            entry.stage_identifier
        ));
    }
    issues
}

// stage_is_subscription 报告阶段是否声明了 ANCHOR 订阅入口。解析失败的
// hook 不算订阅形态：语法错误由引用存在性校验统一上报。
fn stage_is_subscription(stage: &ZhixuStage) -> bool {
    stage.receive_signals.values().any(|raw| {
        parse_hook_for_compiler("HOOK", raw)
            .map(|parsed| parsed.mode == uvp_hook_dsl::HookMode::Subscription)
            .unwrap_or(false)
    })
}

fn validate_mint_anchors(entries: &[StageEntry]) -> Vec<String> {
    let mut issues = Vec::new();
    // 编译期固定五件事（模-1/模-2 裁决）：
    // 1) mint 取值合法（当前仅 per-fact）；
    // 2) mint 阶段必须编译期静态绑定非委托执行者（运行时 patch 对出生阶段
    //    一律拒绝）；
    // 3) 出生入口只能是 ANCHOR 订阅（跨类事实携带溯源进入；"附加单正普通
    //    hook"形态已废除——普通 hook 在铸单前没有可求值的订单上下文）；
    // 4) 防无界代铸链：mint 阶段的订阅目标不得指向本阶段自己的 source 类；
    // 5) 防跨源代铸环：全部 mint 阶段的订阅目标 source 类构成的有向图
    //    （含委托 signalMap 对译后的最终远端类）不得存在可达环——见
    //    validate_mint_subscription_cycles。
    // 其余阶段是否"有锚"由运行时按对接记录路由自然裁决：存在订单实例则
    // 按单投递（域内血缘/dock 边），不存在实例则按类扇入。执行器自发 str
    // 出的订单编译期不可见，因此不在此做静态锚定判断。
    for entry in entries {
        if let Some(mint) = &entry.stage.mint {
            // 与 Go 侧口径一致：精确比较，不接受带空白的变体。
            if mint != "per-fact" {
                issues.push(format!(
                    "{}.mint only supports per-fact: {}",
                    entry.stage_identifier, mint
                ));
            }
            // 模-1 裁决：mint 出生阶段必须编译期静态绑定非委托执行者。
            // 运行时 patch 对订阅/出生阶段一律拒绝，没有静态执行者的出生
            // 阶段是"出生即死"的代铸死锁。
            if !has_static_executor(entry.stage.executor.as_ref()) {
                issues.push(format!(
                    "{}.mint stage requires a static executor (subscription/birth stages cannot be patched at runtime)",
                    entry.stage_identifier
                ));
            }
            if entry.stage.executor.is_some() {
                let executor_type = entry
                    .stage
                    .executor
                    .as_ref()
                    .and_then(|value| value.get("supplierType"))
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("");
                if executor_type == "zhixu" {
                    // mint 出生 + 委托执行器：signalMap hook 是同单 normal AST，
                    // 注入 mint 后无法解码，委托阶段永不完成——组合直接拒绝。
                    issues.push(format!(
                        "{}.mint stage cannot use a zhixu delegation executor",
                        entry.stage_identifier
                    ));
                }
            }
            let subscription_count = entry
                .stage
                .receive_signals
                .values()
                .filter_map(|raw| parse_hook_for_compiler("HOOK", raw).ok())
                .filter(|parsed| parsed.mode == uvp_hook_dsl::HookMode::Subscription)
                .count();
            if subscription_count == 0 {
                issues.push(format!(
                    "{}.mint stage must declare at least one ANCHOR(@…) subscription",
                    entry.stage_identifier
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
                    Ok(_) => {
                        // 模-2 裁决：出生入口只能是 ANCHOR 订阅。"订阅之外附加
                        // 单正普通 hook"的形态废除——出生事实一律走订阅通道
                        // 携带溯源进入。
                        issues.push(format!(
                            "{}.receiveSignals.{hook_name}: mint stage accepts ANCHOR(@…) subscription entries only; plain birth-entry hooks are retired",
                            entry.stage_identifier
                        ));
                    }
                }
            }
        }
    }
    // 5) 防跨源代铸环（源类级统一环检测，直连自环已在上面按条上报）。
    issues.extend(validate_mint_subscription_cycles(entries));
    issues
}

/// mint 跨源代铸环检测（源类级）：收集全部 mint 阶段的订阅目标 source 类，
/// 构建有向边（本 stage.source → 订阅目标 source；订阅目标阶段若经 zhixu
/// 委托 signalMap 对译，则追加最终远端 source 类边），检测可达环
/// （A→B→A、A→B→C→A）。成环意味着代铸事实在源类之间互相触发、永不收敛
/// ——per-fact mint 构成无界代铸环，编译期直接拒绝。
fn validate_mint_subscription_cycles(entries: &[StageEntry]) -> Vec<String> {
    let delegation_remotes = delegation_remote_sources(entries);
    let mut edges: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for entry in entries {
        if entry.stage.mint.is_none() {
            continue;
        }
        let source = entry.stage.source.clone();
        for raw_expression in entry.stage.receive_signals.values() {
            // 解析失败的条目不构边：语法错误由引用存在性校验统一上报。
            let Ok(parsed) = parse_hook_for_compiler("HOOK", raw_expression) else {
                continue;
            };
            // 非订阅条目由 validate_mint_anchors 上报；直连自环（订阅自身
            // source 类）也已按条上报，这里不重复构边。
            let Some(target) = &parsed.subscription_target else {
                continue;
            };
            if target.source != source {
                edges
                    .entry(source.clone())
                    .or_default()
                    .insert(target.source.clone());
            }
            // 委托对译：订阅目标阶段经 signalMap 把信号映射到远端 source 类
            // 时，注入 mint 的事实的最终来源是远端类——按远端类补边（含
            // 映射回本类形成的自环，统一由环检测上报）。
            if let Some((stage_identifier, _)) = parse_signal_reference(&target.signal_name) {
                for remote in delegation_remotes.get(&stage_identifier).into_iter().flatten() {
                    edges
                        .entry(source.clone())
                        .or_default()
                        .insert(remote.clone());
                }
            }
        }
    }
    match find_first_cycle(&edges) {
        Some(cycle) => vec![format!(
            "mint subscriptions form an unbounded re-mint cycle (mint 订阅构成无界代铸环): {}",
            cycle.join(" -> ")
        )],
        None => Vec::new(),
    }
}

/// stage_identifier → 该阶段 zhixu 委托 signalMap 对译出的远端 source 类
/// 集合。signalMap 是跨域委托接缝上的对译表：hook 的 source 即被委托域。
/// 解析失败的条目不计（错误由 validate_signal_maps 统一上报）。
fn delegation_remote_sources(entries: &[StageEntry]) -> BTreeMap<String, BTreeSet<String>> {
    let mut remotes: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for entry in entries {
        let signal_map = stage_signal_map(entry.stage.executor.as_ref());
        let Some(signal_map) = signal_map else {
            continue;
        };
        let mut sources = BTreeSet::new();
        for raw in signal_map.values() {
            let Some(raw_expression) = raw.as_str() else {
                continue;
            };
            if let Ok(hook) = parse_hook_for_compiler("HOOK", raw_expression) {
                sources.insert(hook.source);
            }
        }
        if !sources.is_empty() {
            remotes.insert(entry.stage_identifier.clone(), sources);
        }
    }
    remotes
}

/// 在 source 类有向图中找第一个可达环并回溯出完整路径（BFS + 父指针，
/// 节点遍历顺序确定保证诊断确定；迭代实现避免毒定义撑爆调用栈）。
fn find_first_cycle(edges: &BTreeMap<String, BTreeSet<String>>) -> Option<Vec<String>> {
    for start in edges.keys() {
        let mut parents: BTreeMap<String, String> = BTreeMap::new();
        let mut queue: VecDeque<&String> = VecDeque::new();
        for next in edges.get(start).into_iter().flatten() {
            if next == start {
                return Some(vec![start.clone(), start.clone()]);
            }
            if parents.insert(next.clone(), start.clone()).is_none() {
                queue.push_back(next);
            }
        }
        while let Some(current) = queue.pop_front() {
            for next in edges.get(current).into_iter().flatten() {
                if *next == *start {
                    // 回到起点：start -> … -> current -> start
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
                    queue.push_back(next);
                }
            }
        }
    }
    None
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

// receiveSignals key 即阶段内 hook_name，落 hook_name 列（VARCHAR(36)）。
// 与 signalMap key 同构约束（语法手册 §7.4："key 不可为空且不能含 '.'"）：
// '.' 是信号名分隔符（task.stage.signal）；'#' 是 hookId 分隔符
// （stage#hook_name）——key 携带任一分隔符都会让 hookId 命名空间含混，
// 且不含 '.' 已保证 receive key 不可能拼出 signalMap hook 的 hookId
// （stage#signalMap.<key>）。key 与本阶段 signalMap key 同名时，cloud
// 产物会对同一 (stageIdentifier, hookName) 产出两条 hook 记录，直接拒绝。
fn validate_receive_signal_keys(entries: &[StageEntry]) -> Vec<String> {
    let mut issues = Vec::new();
    for entry in entries {
        let signal_map_keys: BTreeSet<&str> = stage_signal_map(entry.stage.executor.as_ref())
            .map(|signal_map| signal_map.keys().map(String::as_str).collect())
            .unwrap_or_default();
        for hook_name in entry.stage.receive_signals.keys() {
            if hook_name.trim().is_empty() {
                issues.push(format!(
                    "{}.receiveSignals contains an empty hook name",
                    entry.stage_identifier
                ));
                continue;
            }
            if hook_name.contains('.') || hook_name.contains('#') || hook_name.len() > 36 {
                issues.push(format!(
                    "{}.receiveSignals.{hook_name} is invalid: key must be 1-36 bytes and must not contain '.' or '#'",
                    entry.stage_identifier
                ));
                continue;
            }
            if signal_map_keys.contains(hook_name.as_str()) {
                issues.push(format!(
                    "{}.receiveSignals.{hook_name} is invalid: key must not collide with executor.zhixuExecutorConfig.signalMap key of the same stage",
                    entry.stage_identifier
                ));
            }
        }
    }
    issues
}

fn stage_signal_map(executor: Option<&Value>) -> Option<&Map<String, Value>> {
    executor
        .filter(|executor| executor.get("supplierType").and_then(Value::as_str) == Some("zhixu"))
        .and_then(|executor| executor.get("zhixuExecutorConfig"))
        .and_then(|value| value.get("signalMap"))
        .and_then(Value::as_object)
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
                Ok(hook) => {
                    if hook.mode == HookMode::Subscription {
                        issues.push(format!(
                            "{}.executor.zhixuExecutorConfig.signalMap.{signal} is invalid: signalMap entries must not be subscription entries",
                            entry.stage_identifier
                        ));
                        continue;
                    }
                    parsed.push((signal.clone(), hook))
                }
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
        if !referenced_stage.stage.send_signals.contains(&signal_name) {
            // 目标 stage 未声明 sendSignals 时任何引用都是悬空引用：文档要求
            // 引用存在，放行会把死依赖从编译期推迟为运行期静默 init。
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

    #[test]
    fn rejects_subscription_stage_bound_only_through_selected_stages() {
        // audit3 DEPLOY-01：订阅阶段的投递目标编译期定死、运行时禁止
        // executor patch。selectedStages 可达只对可 patch 的普通阶段构成
        // 绑定——被 selector 指到的订阅阶段仍必须有自身静态 executor，
        // 否则编译放行却没有 executor route、又禁补绑，投递必败。
        let mut definition = demo_definition();
        let execution = &mut definition["spec"]["taskPatterns"][1]["stages"][0];
        let object = execution.as_object_mut().expect("stage is an object");
        object.insert("executor".to_string(), serde_json::Value::Null);
        object.insert(
            "receiveSignals".to_string(),
            json!({ "OBS": "::ANCHOR(@buyer::selector.assign.executor_selected)" }),
        );
        object.remove("sendSignals");

        let error = compile_zhixu_hook_plan(&definition)
            .expect_err("subscription stage without its own static executor must fail");
        assert!(
            error
                .to_string()
                .contains("requires its own static executor"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn rejects_receive_signal_keys_with_separators_or_signal_map_collisions() {
        // 含 '.' 的 key：'.' 是信号名分隔符（task.stage.signal），receiveSignals
        // key 承载阶段内通道名（语法手册 §7.4）。
        let mut definition = demo_definition();
        definition["spec"]["taskPatterns"][1]["stages"][0]["receiveSignals"]["BAD.KEY"] =
            json!("buyer::selector.assign.executor_selected");
        let error = compile_zhixu_hook_plan(&definition)
            .expect_err("receiveSignals key containing '.' must fail");
        let message = error.to_string();
        assert!(
            message.contains("receiveSignals.BAD.KEY") && message.contains("must not contain '.' or '#'"),
            "unexpected error: {message}"
        );
        // cloud 目标同口径（与 hook_plan 共用同一组校验）。
        let error = compile_cloud_artifact(&definition)
            .expect_err("receiveSignals key containing '.' must fail for cloud too");
        assert!(
            error.to_string().contains("must not contain '.' or '#'"),
            "unexpected error: {error}"
        );

        // 含 '#' 的 key：'#' 是 hookId 分隔符（stage#hook_name）。
        let mut definition = demo_definition();
        definition["spec"]["taskPatterns"][1]["stages"][0]["receiveSignals"]["BAD#KEY"] =
            json!("buyer::selector.assign.executor_selected");
        let error = compile_zhixu_hook_plan(&definition)
            .expect_err("receiveSignals key containing '#' must fail");
        assert!(
            error.to_string().contains("must not contain '.' or '#'"),
            "unexpected error: {error}"
        );

        // 与本阶段 signalMap key 同名：cloud 产物会对同一
        // (stageIdentifier, hookName) 产出两条 hook 记录——直接拒绝。
        let mut definition = demo_definition();
        definition["spec"]["taskPatterns"][1]["stages"][0]["receiveSignals"]["cmp"] =
            json!("buyer::selector.assign.executor_selected");
        let error = compile_zhixu_hook_plan(&definition).expect_err(
            "receiveSignals key colliding with a signalMap key of the same stage must fail",
        );
        assert!(
            error
                .to_string()
                .contains("must not collide with executor.zhixuExecutorConfig.signalMap"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn rejects_mutual_mint_subscription_cycle() {
        // A↔B 互订：A 铸出的事实触发 B 铸、B 铸出的事实又触发 A 铸——
        // 无界代铸环，编译期拒绝（含 cloud/hook_plan 两个 target）。
        let definition = mint_definitions(&[
            (
                "dispatch",
                mint_stage_value(
                    "dispatch",
                    "main",
                    "producer",
                    json!({ "SPAWN": "::ANCHOR(@buyer::orchard.retail.ack)" }),
                    &["smart_contract"],
                ),
            ),
            (
                "orchard",
                mint_stage_value(
                    "orchard",
                    "retail",
                    "buyer",
                    json!({ "SPAWN": "::ANCHOR(@producer::dispatch.main.smart_contract)" }),
                    &["ack"],
                ),
            ),
        ]);
        let error = compile_zhixu_hook_plan(&definition)
            .expect_err("mutual mint subscription cycle must fail");
        let message = error.to_string();
        assert!(
            message.contains("unbounded re-mint cycle") && message.contains("buyer -> producer -> buyer"),
            "unexpected error: {message}"
        );
        let error =
            compile_cloud_artifact(&definition).expect_err("cloud target must reject the cycle too");
        assert!(
            error.to_string().contains("unbounded re-mint cycle"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn rejects_three_stage_mint_cycle() {
        // A→B→C→A 三节环：每个 mint 阶段都只订阅下一类，仍构成无界代铸环。
        let definition = mint_definitions(&[
            (
                "dispatch",
                mint_stage_value(
                    "dispatch",
                    "main",
                    "producer",
                    json!({ "SPAWN": "::ANCHOR(@maker::workshop.assemble.part)" }),
                    &["smart_contract"],
                ),
            ),
            (
                "orchard",
                mint_stage_value(
                    "orchard",
                    "retail",
                    "buyer",
                    json!({ "SPAWN": "::ANCHOR(@producer::dispatch.main.smart_contract)" }),
                    &["ack"],
                ),
            ),
            (
                "workshop",
                mint_stage_value(
                    "workshop",
                    "assemble",
                    "maker",
                    json!({ "SPAWN": "::ANCHOR(@buyer::orchard.retail.ack)" }),
                    &["part"],
                ),
            ),
        ]);
        let error =
            compile_zhixu_hook_plan(&definition).expect_err("three-stage mint cycle must fail");
        let message = error.to_string();
        assert!(
            message.contains("unbounded re-mint cycle")
                && message.contains("buyer -> producer -> maker -> buyer"),
            "unexpected error: {message}"
        );
    }

    #[test]
    fn allows_acyclic_mint_subscription_chain() {
        // A→B 不回指：出生订阅链无环时必须照常放行。
        let definition = mint_definitions(&[
            (
                "dispatch",
                emitter_stage_value("dispatch", "main", "producer", &["smart_contract"]),
            ),
            (
                "orchard",
                mint_stage_value(
                    "orchard",
                    "retail",
                    "buyer",
                    json!({ "SPAWN": "::ANCHOR(@producer::dispatch.main.smart_contract)" }),
                    &["ack"],
                ),
            ),
        ]);
        compile_zhixu_hook_plan(&definition)
            .expect("acyclic mint subscription chain must compile for hook_plan");
        compile_cloud_artifact(&definition)
            .expect("acyclic mint subscription chain must compile for cloud");
    }

    #[test]
    fn still_rejects_mint_stage_subscribing_its_own_source() {
        // 直连自环（mint 阶段订阅自身 source 类）保持既有按条报错口径。
        let definition = mint_definitions(&[
            (
                "dispatch",
                emitter_stage_value("dispatch", "main", "producer", &["smart_contract"]),
            ),
            (
                "orchard",
                mint_stage_value(
                    "orchard",
                    "retail",
                    "producer",
                    json!({ "SPAWN": "::ANCHOR(@producer::dispatch.main.smart_contract)" }),
                    &["ack"],
                ),
            ),
        ]);
        let error = compile_zhixu_hook_plan(&definition)
            .expect_err("mint stage subscribing its own source class must fail");
        assert!(
            error
                .to_string()
                .contains("must not subscribe its own source class"),
            "unexpected error: {error}"
        );
    }

    fn mint_stage_value(
        task: &str,
        name: &str,
        source: &str,
        receive_signals: Value,
        send_signals: &[&str],
    ) -> Value {
        json!({
            "name": name,
            "source": source,
            "receiveSignals": receive_signals,
            "sendSignals": send_signals,
            "mint": "per-fact",
            "executor": {
                "supplierType": "organization",
                "supplierID": format!("{task}-{name}-executor")
            }
        })
    }

    fn emitter_stage_value(task: &str, name: &str, source: &str, send_signals: &[&str]) -> Value {
        json!({
            "name": name,
            "source": source,
            "sendSignals": send_signals,
            "executor": {
                "supplierType": "organization",
                "supplierID": format!("{task}-{name}-executor")
            }
        })
    }

    fn mint_definitions(stages: &[(&str, Value)]) -> Value {
        json!({
            "apiVersion": "uvp/v0",
            "kind": "Zhixu",
            "metadata": {
                "name": "mint_cycle",
                "uid": "zhixu-mint-cycle-001",
                "annotations": { "version": "1" }
            },
            "spec": {
                "platform": { "type": "cloud" },
                "nucleation": { "id": "core" },
                "taskPatterns": stages
                    .iter()
                    .map(|(task, stage)| json!({ "name": task, "stages": [stage] }))
                    .collect::<Vec<_>>()
            }
        })
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
