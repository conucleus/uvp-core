//! 编译产物：hook_plan 与 cloud artifact 两条产物线的组装（含依赖索引与
//! signal capabilities）。

use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet};
use uvp_hook_dsl::DependencyKind;
use uvp_model::ZhixuDefinition;

use crate::docking::{compile_dock_state, dock_entrance_hook_ids};
use crate::lower::{
    build_executor_routes, build_selected_stage_bindings, compile_stage_hooks, flatten_stages,
    normalize_platform_value, parse_hook_for_cloud, value_str, StageEntry,
};
use crate::validate::{
    valid_identifier_part, valid_signal_declaration, validate_mint_anchors,
    validate_onchain_stage_materialization, validate_receive_signal_keys,
    validate_receive_signal_references, validate_stage_executors, validate_subscription_delegation,
    validate_zhixu_shape,
};
use crate::{CompilerError, Result};

/// HookPlan 产物信封版本（TS 权威 uvp-protocol compiler types 的
/// HOOK_PLAN_SCHEMA_VERSION 镜像）。pub 供 uvp-node NAPI 导出
/// hookPlanSchemaVersion：TS 侧兼容门逐字比对两侧常量，防漂移。
pub const HOOK_PLAN_SCHEMA_VERSION: &str = "uvp.hookPlan.v2";
/// cloud 编译产物的信封版本：Go 侧 pkg/version.CloudArtifactSchema 镜像此值，
/// parity 测试按 `pub const` 声明逐字比对，必须保持 pub。
pub const CLOUD_ARTIFACT_SCHEMA_VERSION: &str = "uvp.cloudArtifact.v2";
/// sendSignals 能力表规模上限：UVPPlanMetadataModule 逐条写存储注册
/// signalCapabilities（合约注册边界同值 revert TooManySignalCapabilities），
/// 无上限则注册 gas 随 plan 规模无界增长。信号提交侧的归属读取走
/// metadata 属主索引单键查询，gas 不随表规模变化——上限守护的是注册
/// 循环，不是热路径。TS 侧 onchain-hook-plan.ts 的
/// MAX_SIGNAL_CAPABILITIES 在编译+反序列化两边界同值同文案；Rust 是语义
/// 权威，此值即上限的唯一出处，TS 必须镜像。
pub(crate) const MAX_SIGNAL_CAPABILITIES: usize = 256;

/// hook_plan 产物：中性 plan 壳（模型/校验/编译结果 + dock 声明面）。
/// 哈希承诺（planHash/roots/派生身份）由链轨 TS 在此壳上计算；云轨
/// 身份归 DB——core 不产出任何身份字段。
pub fn compile_zhixu_hook_plan(
    definition_value: &Value,
    resolution_manifest: Option<&Value>,
    allow_unresolved: bool,
) -> Result<Value> {
    let definition: ZhixuDefinition = serde_json::from_value(definition_value.clone())
        .map_err(|err| CompilerError::Message(format!("invalid Zhixu definition: {err}")))?;
    let issues = validate_zhixu_shape(&definition);
    if !issues.is_empty() {
        return Err(CompilerError::Issues(issues.join("; ")));
    }

    let stage_entries = flatten_stages(&definition)?;
    let stage_pairs = stage_entries
        .iter()
        .map(|entry| (entry.stage_identifier.clone(), entry.stage.clone()))
        .collect::<Vec<_>>();
    let stage_ids = stage_entries
        .iter()
        .map(|entry| entry.stage_identifier.clone())
        .collect::<BTreeSet<_>>();
    let selected_stage_bindings = build_selected_stage_bindings(&stage_entries, &stage_ids)?;

    // Dock：目标接口编译 + 调用方 route 收集 + link。
    let dock_state = compile_dock_state(
        &definition,
        &stage_pairs,
        resolution_manifest,
        allow_unresolved,
    )?;

    let mut validation_issues = Vec::new();
    validation_issues.extend(validate_stage_executors(
        &stage_entries,
        &selected_stage_bindings,
    ));
    // 阶段物化门（onchain 目标）：每个阶段声明都必须编译出至少一个
    // 带物化位（order-trigger mint/dock 或 EMIT_READY）的 hook——纯
    // flags=0 watcher 不物化阶段，零 hook 阶段同样不物化，且其 sendSignals
    // 在链上没有钩子可挂（submitSignal 要求源阶段已物化，恒 revert
    // UnknownHook），形态一旦上链即死锁且无恢复路径（executor patch 也不
    // 物化）。dockInterface entrance 端口钩子编译为 dock|emitReady（=6），
    // 是合法物化路径，不得按 watcher 误拒。
    validation_issues.extend(validate_onchain_stage_materialization(
        &stage_entries,
        &dock_entrance_hook_ids(&dock_state),
    ));
    validation_issues.extend(validate_mint_anchors(&stage_entries));
    validation_issues.extend(validate_subscription_delegation(&stage_entries));
    validation_issues.extend(validate_receive_signal_keys(&stage_entries));
    validation_issues.extend(validate_receive_signal_references(
        &stage_entries,
        &dock_state.input_port_hook_ids,
    ));
    if !validation_issues.is_empty() {
        return Err(CompilerError::Issues(validation_issues.join("; ")));
    }

    let platform = normalize_platform_value(&definition.spec.platform)?;

    let mut compiled_hooks = Vec::new();
    for entry in &stage_entries {
        compiled_hooks.extend(compile_stage_hooks(entry, &dock_state)?);
    }
    let dependency_index = build_dependency_index(&compiled_hooks);
    let signal_capabilities = build_signal_capabilities(&stage_entries)?;
    let executor_routes = build_executor_routes(&stage_entries);

    let mut artifact = json!({
        "schemaVersion": HOOK_PLAN_SCHEMA_VERSION,
        "zhixuName": definition.metadata.name,
        "platform": platform,
        "compiledHooks": compiled_hooks,
        "dependencyIndex": dependency_index,
        "executorRoutes": executor_routes,
        "dockRoutes": Value::Array(dock_state.routes_json),
        "selectedStageBindings": selected_stage_bindings,
        "signalCapabilities": signal_capabilities,
    });
    if let Some(interface_json) = dock_state.interface_json {
        artifact["dockInterface"] = interface_json;
    }
    // 空清单不落字段：未解析 route 是动态选择的声明面，目标空缺。
    if !dock_state.unresolved_json.is_empty() {
        artifact["unresolvedDockRoutes"] = Value::Array(dock_state.unresolved_json);
    }
    Ok(artifact)
}

pub fn compile_cloud_artifact(
    definition_value: &Value,
    resolution_manifest: Option<&Value>,
    allow_unresolved: bool,
) -> Result<Value> {
    let definition: ZhixuDefinition = serde_json::from_value(definition_value.clone())
        .map_err(|err| CompilerError::Message(format!("invalid Zhixu definition: {err}")))?;
    let issues = validate_zhixu_shape(&definition);
    if !issues.is_empty() {
        return Err(CompilerError::Issues(issues.join("; ")));
    }

    let stage_entries = flatten_stages(&definition)?;
    let stage_pairs = stage_entries
        .iter()
        .map(|entry| (entry.stage_identifier.clone(), entry.stage.clone()))
        .collect::<Vec<_>>();
    let stage_ids = stage_entries
        .iter()
        .map(|entry| entry.stage_identifier.clone())
        .collect::<BTreeSet<_>>();
    let selected_stage_bindings = build_selected_stage_bindings(&stage_entries, &stage_ids)?;
    let dock_state = compile_dock_state(
        &definition,
        &stage_pairs,
        resolution_manifest,
        allow_unresolved,
    )?;

    let mut validation_issues = Vec::new();
    // Cloud and hook_plan are two artifact profiles over the same definition;
    // both must enforce the static-executor/selectedStages contract.  Without
    // this call cloud could publish a subscription stage that hook_plan would
    // reject (or, worse, a stage that runtime patching can never bind).
    validation_issues.extend(validate_stage_executors(
        &stage_entries,
        &selected_stage_bindings,
    ));
    validation_issues.extend(validate_mint_anchors(&stage_entries));
    validation_issues.extend(validate_subscription_delegation(&stage_entries));
    // 与 hook_plan 目标共用同一组校验：同一份定义不允许"一个 target 收、
    // 另一个放"，否则 Go 主链路会拿到被 hook_plan 拒绝的定义的产物。
    validation_issues.extend(validate_receive_signal_keys(&stage_entries));
    validation_issues.extend(validate_receive_signal_references(
        &stage_entries,
        &dock_state.input_port_hook_ids,
    ));
    // sendSignals capability 同口径（空串/重复在两个 target 一致拒绝）：
    // cloud 产物供 Go 主链路消费，不得放行 hook_plan 已拒绝的声明。
    build_signal_capabilities(&stage_entries)?;
    if !validation_issues.is_empty() {
        return Err(CompilerError::Issues(validation_issues.join("; ")));
    }
    let platform = normalize_platform_value(&definition.spec.platform)?;
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
    }

    let mut artifact = json!({
        "schemaVersion": CLOUD_ARTIFACT_SCHEMA_VERSION,
        "zhixuName": definition.metadata.name,
        "platform": platform,
        "stages": stages,
        "hooks": hooks,
        "orderStageDefaults": stages,
        "dockRoutes": Value::Array(dock_state.routes_json),
    });
    if let Some(interface_json) = dock_state.interface_json {
        artifact["dockInterface"] = interface_json;
    }
    // 空清单不落字段：未解析 route 是动态选择的声明面，目标空缺。
    if !dock_state.unresolved_json.is_empty() {
        artifact["unresolvedDockRoutes"] = Value::Array(dock_state.unresolved_json);
    }
    Ok(artifact)
}

fn cloud_stage_artifact(entry: &StageEntry) -> Result<Value> {
    let mut stage = Map::new();
    stage.insert(
        "stageIdentifier".to_string(),
        Value::String(entry.stage_identifier.clone()),
    );
    if let Some(executor) = &entry.stage.executor {
        stage.insert(
            "executorConfigs".to_string(),
            serde_json::to_value(executor)
                .map_err(|err| CompilerError::Message(err.to_string()))?,
        );
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
    // dependencies 此处只投 signalName/dependencyKind 两维：Go 主链路按
    // (signalName, kind) 消费该结构（uvp.cloudArtifact.v2 冻结面），source
    // 维度不在其中——依赖的真实 source 由 astJson 恢复（普通 hook = 产物
    // sourceZhixuRef/self，ANCHOR 订阅 = subscriptionTarget.source 或 root
    // 订阅节点）。补 source 需改产物 schema 并同步 Go 消费方，属两轨变更，
    // 未裁决前不做单侧扩列。
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
    // E16 镜像（合约 UVPPlanMetadataModule._registerSignalCapabilities 的
    // DuplicateCurrentOrderSignalCapability / TS onchain-hook-plan 的
    // duplicateCurrentOrderFactKeyIssues）：relation=current 的事实键
    // (targetSource, targetSignalName) 在 plan 内唯一属主——跨阶段双属主
    // 会让链上 _currentOrderFactStages 后写覆盖先写、_signalStageId 归属
    // 二义，注册边界 revert；编译期按同一展开后的全名同口径拒绝。
    // triggerOrigin（relation=1）不在此列：合约与 TS 预检都允许跨阶段
    // 声明同一触发源能力。
    let mut current_order_owners: BTreeMap<(String, String), String> = BTreeMap::new();
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
            if value_str(&capability, "targetOrderRelation") == "current" {
                let fact_key = (
                    value_str(&capability, "targetSource").to_string(),
                    value_str(&capability, "targetSignalName").to_string(),
                );
                if let Some(owner) = current_order_owners.get(&fact_key) {
                    if *owner != entry.stage_identifier {
                        return Err(CompilerError::Issues(format!(
                            "{}.sendSignals declares the current-order fact key ({}, {}) already owned by {}: one capability has one owner (UVPPlanMetadataModule reverts DuplicateCurrentOrderSignalCapability at finalizePlan; declare the fact key on a single stage)",
                            entry.stage_identifier, fact_key.0, fact_key.1, owner
                        )));
                    }
                }
                current_order_owners.insert(fact_key, entry.stage_identifier.clone());
            }
            capabilities.push(capability);
        }
    }
    // 上限在去重之后检查：重复声明已被前面拒绝，此处计数即编译产物的
    // signalCapabilities 长度，与 TS signalCapabilityCountIssues 同口径。
    if capabilities.len() > MAX_SIGNAL_CAPABILITIES {
        return Err(CompilerError::Issues(format!(
            "signal capabilities {} exceed the documented limit {} (UVPPlanMetadataModule registers each capability with a storage write; unbounded plan-controlled registration gas)",
            capabilities.len(),
            MAX_SIGNAL_CAPABILITIES
        )));
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

/// sendSignals 声明的单一精确口径：declared 与 capability 携带同一原文，
/// 不 trim（文法"map 键值不 trim"同口径）。capability 侧若做 trim 归一，
/// 产物会出现两种值——引用侧按存储值精确匹配必然失配（死能力），且
/// "str" 与 " str" 会撞 duplicate 误判。空白/非法字符在此响亮拒绝。
fn parse_signal_capability(entry: &StageEntry, declared_signal: &str) -> Result<Value> {
    if declared_signal.is_empty() {
        return Err(CompilerError::Issues(format!(
            "{}.sendSignals cannot contain an empty signal",
            entry.stage_identifier
        )));
    }
    if let Some((target_source, target_signal_name)) = declared_signal.split_once("::") {
        // `<target>::<signal>` 跨源触发形态：目标半段是 source 类，与信号
        // 半段（裸名或 task.stage.signal）共用信号名同款标识符文法——
        // `::` 再现（a::b::c）、空白、非标识符字符在此拒绝。
        if !valid_identifier_part(target_source) || !valid_signal_declaration(target_signal_name) {
            return Err(CompilerError::Issues(format!(
                "{}.sendSignals contains invalid target signal {:?}: <target>::<signal> requires identifier-grammar halves (ASCII letter start, letters/digits/'_'/'-'; signal half may be a bare name or task.stage.signal)",
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
    let target_signal_name = if declared_signal.contains('.') {
        if !valid_signal_declaration(declared_signal) {
            return Err(CompilerError::Issues(format!(
                "{}.sendSignals contains invalid canonical signal {:?}: expected task.stage.signal with identifier-grammar segments",
                entry.stage_identifier, declared_signal
            )));
        }
        // 三段式只是显式自指形态：task.stage 前缀必须落在声明阶段自身。
        // 指向别处命名空间的 canonical 声明会与目标阶段的裸名声明展开成
        // 同一 (targetSource, signal) capability——双属主绕过"一事一能力"，
        // 链上 _signalStageId 归属二义（E16 注册边界 revert
        // DuplicateCurrentOrderSignalCapability），编译期同口径拒绝。
        if !declared_signal.starts_with(&format!("{}.", entry.stage_identifier)) {
            return Err(CompilerError::Issues(format!(
                "{}.sendSignals contains canonical signal {:?} that does not address the declaring stage: expected {}.<signal> (the three-part form is an explicit self-reference; bare names expand to the same capability)",
                entry.stage_identifier, declared_signal, entry.stage_identifier
            )));
        }
        declared_signal.to_string()
    } else {
        if !valid_identifier_part(declared_signal) {
            return Err(CompilerError::Issues(format!(
                "{}.sendSignals contains invalid signal name {:?}: must start with an ASCII letter and contain only ASCII letters, digits, '_' or '-'",
                entry.stage_identifier, declared_signal
            )));
        }
        format!("{}.{}", entry.stage_identifier, declared_signal)
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
