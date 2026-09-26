//! 编译产物：hook_plan 与 cloud artifact 两条产物线的组装（含依赖索引与
//! signal capabilities）。

use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet};
use uvp_hook_dsl::{parse_hook, DependencyKind, Gate, HookMode, ParseHookRequest, Profile};
use uvp_model::{ZhixuDefinition, ZhixuSendSignal};

use crate::docking::{compile_dock_state, dock_entrance_hook_ids};
use crate::lower::{
    build_executor_routes, build_selected_stage_bindings, compile_stage_hooks, flatten_stages,
    normalize_platform_value, parse_hook_for_cloud, value_str, StageEntry,
};
use crate::validate::{
    stage_is_subscription, valid_identifier_part, valid_signal_declaration, validate_mint_anchors,
    validate_onchain_stage_materialization, validate_receive_signal_keys,
    validate_receive_signal_references, validate_stage_executors, validate_subscription_delegation,
    validate_zhixu_shape,
};
use crate::{join_issues_bounded, CompilerError, Result};

/// HookPlan 产物信封版本（TS 权威 uvp-protocol compiler types 的
/// HOOK_PLAN_SCHEMA_VERSION 镜像）。pub 供 uvp-node NAPI 导出
/// hookPlanSchemaVersion：TS 侧兼容门逐字比对两侧常量，防漂移。
/// v4：dockRoutes 元素目标寻址单键 uid（target.uid）。
pub const HOOK_PLAN_SCHEMA_VERSION: &str = "uvp.hookPlan.v4";
/// cloud 编译产物的信封版本：Go 侧 pkg/version.CloudArtifactSchema 镜像此值，
/// parity 测试按 `pub const` 声明逐字比对，必须保持 pub。
/// v4：dockRoutes 元素目标寻址单键 uid（target.uid）。
pub const CLOUD_ARTIFACT_SCHEMA_VERSION: &str = "uvp.cloudArtifact.v4";
// 能力表无规模上限（Merkle 化）：链上不逐条注册 signalCapabilities，
// 由链下 TS 编译器建树以 capabilitiesRoot 承诺；Rust 按架构契约保持
// 中性语义权威、不产哈希、不建树，仅保留逐条语义校验（空串/重复/
// relation=current 事实键唯一属主）。

/// hook_plan 产物：中性 plan 壳（模型/校验/编译结果 + dock 声明面）。
/// 哈希承诺（planHash/roots/派生身份）由链轨 TS 在此壳上计算；云轨
/// 身份归 DB——core 不产出任何身份字段。
pub fn compile_zhixu_hook_plan(
    definition_value: &Value,
    dock_targets: Option<&Value>,
    allow_unresolved: bool,
) -> Result<Value> {
    let definition: ZhixuDefinition = serde_json::from_value(definition_value.clone())
        .map_err(|err| CompilerError::Message(format!("invalid Zhixu definition: {err}")))?;
    let issues = validate_zhixu_shape(&definition);
    if !issues.is_empty() {
        return Err(CompilerError::Issues(join_issues_bounded(&issues)));
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
        dock_targets,
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
    validation_issues.extend(validate_mint_anchors(
        &stage_entries,
        &dock_state.entrance_fact_keys,
    ));
    validation_issues.extend(validate_subscription_delegation(&stage_entries));
    validation_issues.extend(validate_receive_signal_keys(&stage_entries));
    validation_issues.extend(validate_receive_signal_references(
        &stage_entries,
        &dock_state.input_port_hook_ids,
    ));
    if !validation_issues.is_empty() {
        return Err(CompilerError::Issues(join_issues_bounded(
            &validation_issues,
        )));
    }

    let platform = normalize_platform_value(&definition.spec.platform)?;

    let mut compiled_hooks = Vec::new();
    for entry in &stage_entries {
        compiled_hooks.extend(compile_stage_hooks(entry, &dock_state)?);
    }
    let dependency_index = build_dependency_index(&compiled_hooks);
    let signal_capabilities = build_signal_capabilities(&stage_entries)?;
    let admissions = build_signal_admissions(&stage_entries, &dock_state, Profile::EvmStrict)?;
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
        "admissions": admissions,
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
    dock_targets: Option<&Value>,
    allow_unresolved: bool,
) -> Result<Value> {
    let definition: ZhixuDefinition = serde_json::from_value(definition_value.clone())
        .map_err(|err| CompilerError::Message(format!("invalid Zhixu definition: {err}")))?;
    let issues = validate_zhixu_shape(&definition);
    if !issues.is_empty() {
        return Err(CompilerError::Issues(join_issues_bounded(&issues)));
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
        dock_targets,
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
    validation_issues.extend(validate_mint_anchors(
        &stage_entries,
        &dock_state.entrance_fact_keys,
    ));
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
    // 适格面同口径：admissions 的编译期拒绝（自引用/出生锚/订阅原子）
    // 在两个 target 一致生效。
    let admissions = build_signal_admissions(&stage_entries, &dock_state, Profile::CloudCompat)?;
    if !validation_issues.is_empty() {
        return Err(CompilerError::Issues(join_issues_bounded(
            &validation_issues,
        )));
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
        "admissions": admissions,
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
    // (signalName, kind) 消费该结构（uvp.cloudArtifact.v4 冻结面），source
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
    // 镜像 TS onchain-hook-plan 的 duplicateCurrentOrderFactKeyIssues：
    // 事实键 (targetSource, targetSignalName) 在 plan 内唯一属主——双属主
    // 会让携证解析无法唯一定位属主阶段（一个事实键只能落一棵能力叶）；
    // 链上能力表以整棵 Merkle root 一次承诺、无逐条注册循环，编译期按
    // 同一展开后的全名同口径拒绝。
    let mut current_order_owners: BTreeMap<(String, String), String> = BTreeMap::new();
    for entry in entries {
        for declared_signal in &entry.stage.send_signals {
            let capability = parse_signal_capability(entry, declared_signal)?;
            let key = format!(
                "{}\0{}\0{}",
                value_str(&capability, "stageIdentifier"),
                value_str(&capability, "targetSource"),
                value_str(&capability, "targetSignalName")
            );
            if !seen.insert(key) {
                return Err(CompilerError::Issues(format!(
                    "D031 {}.sendSignals contains duplicate capability {}",
                    entry.stage_identifier, declared_signal.name
                )));
            }
            let fact_key = (
                value_str(&capability, "targetSource").to_string(),
                value_str(&capability, "targetSignalName").to_string(),
            );
            if let Some(owner) = current_order_owners.get(&fact_key) {
                if *owner != entry.stage_identifier {
                    return Err(CompilerError::Issues(format!(
                        "{}.sendSignals declares the current-order fact key ({}, {}) already owned by {}: one capability has one owner (declare the fact key on a single stage)",
                        entry.stage_identifier, fact_key.0, fact_key.1, owner
                    )));
                }
            }
            current_order_owners.insert(fact_key, entry.stage_identifier.clone());
            capabilities.push(capability);
        }
    }
    // 无规模上限：链上能力表由 capabilitiesRoot 一次性承诺，不存在
    // 逐条注册循环。语义校验（空串/重复/relation=current 事实键唯一
    // 属主）见上，规模不受限。
    capabilities.sort_by(|left, right| {
        value_str(left, "stageIdentifier")
            .cmp(value_str(right, "stageIdentifier"))
            .then(value_str(left, "targetSource").cmp(value_str(right, "targetSource")))
            .then(value_str(left, "targetSignalName").cmp(value_str(right, "targetSignalName")))
    });
    Ok(capabilities)
}

/// sendSignals 声明的单一精确口径：declared 与 capability 携带同一原文，
/// 不 trim（文法"map 键值不 trim"同口径）。capability 侧若做 trim 归一，
/// 产物会出现两种值——引用侧按存储值精确匹配必然失配（死能力），且
/// "str" 与 " str" 会撞 duplicate 误判。空白/非法字符在此响亮拒绝。
fn parse_signal_capability(entry: &StageEntry, declared_signal: &ZhixuSendSignal) -> Result<Value> {
    if declared_signal.name.is_empty() {
        return Err(CompilerError::Issues(format!(
            "D026 {}.sendSignals: name is required and must be non-empty",
            entry.stage_identifier
        )));
    }
    let declared_signal = declared_signal.name.as_str();
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
        // 携证解析无法唯一定位属主阶段，编译期同口径拒绝。
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

/// 发射适格面（admissions）编译：逐条解析 sendSignals 条目的 validWhen
/// （过滤档），产出两个 target 共用的条目集。编译期拒绝（D028 自引用/
/// D029 出生锚与 signalMap 回传目标/D030 订阅原子）与 D027 空白表达式
/// 在此收口；缺省 validWhen 的条目完全绕过适格面（行为与无条件发射
/// 等价），不产出条目。
fn build_signal_admissions(
    entries: &[StageEntry],
    dock_state: &crate::docking::DockState,
    profile: Profile,
) -> Result<Vec<Value>> {
    let minted_sources: BTreeSet<&str> = entries
        .iter()
        .filter(|entry| entry.stage.mint.is_some())
        .map(|entry| entry.stage.source.as_str())
        .collect();
    let birth_anchors = collect_birth_anchor_signals(entries, &dock_state.entrance_fact_keys);
    let mut admissions = Vec::new();
    for entry in entries {
        // 无锚通道阶段（本域 source 类无 mint 声明的订阅阶段）：扇入投递
        // 落通道维度（order_id=''），适格是按单状态判定——无单可判。
        let anchorless_channel = stage_is_subscription(&entry.stage)
            && !minted_sources.contains(entry.stage.source.as_str());
        for declared in &entry.stage.send_signals {
            let Some(valid_when) = &declared.valid_when else {
                continue;
            };
            // 适格表达式是确定性必填面：声明了键却留空白是笔误形态，
            // 静默按无条件放行会把"想设闸没设成"伪装成"没想设闸"。
            if valid_when.trim().is_empty() {
                return Err(CompilerError::Issues(format!(
                    "D027 {}.sendSignals[{}].validWhen: must be a non-blank expression; drop the key to declare unconditional admission",
                    entry.stage_identifier, declared.name
                )));
            }
            if anchorless_channel {
                return Err(CompilerError::Issues(format!(
                    "D029 {}.sendSignals[{}].validWhen: {} is an anchorless channel stage (fan-in subscription delivery has no order to judge; the admission face is a per-order state judgment)",
                    entry.stage_identifier, declared.name, entry.stage_identifier
                )));
            }
            let full_name = if declared.name.contains('.') {
                declared.name.clone()
            } else {
                format!("{}.{}", entry.stage_identifier, declared.name)
            };
            if birth_anchors.contains(&full_name) {
                return Err(CompilerError::Issues(format!(
                    "D029 {}.sendSignals[{}].validWhen: {} is a birth-anchor signal (mint SPAWN birth target or dock order.mode=new birth-anchor input; birth writes bypass the admission face, a validWhen here is dead code)",
                    entry.stage_identifier, declared.name, full_name
                )));
            }
            // signalMap 回传落点：目标输出按绑定回写父侧事实，走引擎内部
            // 事务入口（ownsTx=false），整段绕过适格筛——声明即死代码。
            if dock_state.output_relay_signals.contains(&full_name) {
                return Err(CompilerError::Issues(format!(
                    "D029 {}.sendSignals[{}].validWhen: {} is a signalMap relay target (dock output relay writes the fact through the engine-internal ingress, which bypasses the admission face; a validWhen here is dead code)",
                    entry.stage_identifier, declared.name, full_name
                )));
            }
            let parsed = parse_hook(ParseHookRequest {
                profile,
                gate: Gate::Filter,
                // 适格面没有 hook 通道名：ADMIT 只是过名字闸的占位
                // （产物不携带 hookName，normalizedExpression 与之无关）。
                hook_name: "ADMIT".to_string(),
                hook: valid_when.clone(),
            })
            .map_err(|err| {
                CompilerError::Issues(format!(
                    "{}.sendSignals[{}].validWhen is invalid: {err}",
                    entry.stage_identifier, declared.name
                ))
            })?;
            if parsed.mode == HookMode::Subscription {
                return Err(CompilerError::Issues(format!(
                    "D030 {}.sendSignals[{}].validWhen: admission is a per-order state judgment and must not contain subscription atoms (ANCHOR(@…))",
                    entry.stage_identifier, declared.name
                )));
            }
            // 自引用：求值吃 pre-state（不含本发），引用本信号自身是
            // 自证无效——事实键维度上该原子永不可满足（或恒绕过）。
            if parsed
                .dependencies
                .iter()
                .any(|dependency| dependency.signal_name == full_name)
            {
                return Err(CompilerError::Issues(format!(
                    "D028 {}.sendSignals[{}].validWhen: expression addresses the declaring signal itself ({}); admission judges the pre-state, which never contains the emission being judged",
                    entry.stage_identifier, declared.name, full_name
                )));
            }
            // 引用存在性与 receiveSignals 同口径：标头 source 必须是本域
            // 声明的 source 类，每个 task.stage.signal 必须落在真实存在且
            // source 一致的阶段、并在其 sendSignals 中声明。放行悬空引用
            // 会把死依赖从编译期推迟为运行期静默 init（正锚永不 Ready）或
            // 静默失活的负门。
            let admission_issues = crate::validate::validate_signal_references(
                &parsed,
                &format!(
                    "{}.sendSignals[{}].validWhen",
                    entry.stage_identifier, declared.name
                ),
                entries,
            );
            if !admission_issues.is_empty() {
                return Err(CompilerError::Issues(admission_issues.join("; ")));
            }
            let mut admission = Map::new();
            admission.insert(
                "stageIdentifier".to_string(),
                Value::String(entry.stage_identifier.clone()),
            );
            admission.insert("signalName".to_string(), Value::String(full_name));
            admission.insert(
                "rawExpression".to_string(),
                Value::String(valid_when.clone()),
            );
            if profile == Profile::CloudCompat {
                // 云侧依赖与 hook 条目同源：只投 signalName/dependencyKind
                // 两维（timer 是调度维度，不进云侧消费面）。
                let dependencies: Vec<Value> = parsed
                    .dependencies
                    .iter()
                    .filter(|dependency| dependency.kind != DependencyKind::Timer)
                    .map(|dependency| {
                        json!({
                            "signalName": dependency.signal_name,
                            "dependencyKind": dependency.kind,
                        })
                    })
                    .collect();
                admission.insert("cloudAst".to_string(), parsed.cloud_ast.clone());
                admission.insert("dependencies".to_string(), Value::Array(dependencies));
            } else {
                admission.insert(
                    "normalizedExpression".to_string(),
                    Value::String(parsed.normalized_expression.clone()),
                );
                admission.insert("ast".to_string(), parsed.ast.clone());
                admission.insert(
                    "dependencies".to_string(),
                    serde_json::to_value(&parsed.dependencies)
                        .map_err(|err| CompilerError::Message(err.to_string()))?,
                );
            }
            admissions.push(Value::Object(admission));
        }
    }
    admissions.sort_by(|left, right| {
        value_str(left, "stageIdentifier")
            .cmp(value_str(right, "stageIdentifier"))
            .then(value_str(left, "signalName").cmp(value_str(right, "signalName")))
    });
    Ok(admissions)
}

/// 出生锚信号集（编译可见）：mint 阶段 ANCHOR 订阅的 SPAWN 出生目标
/// （task.stage.signal 全名）∪ dockInterface entrance 端口（orderModes 含
/// new）交付的出生锚 atom 信号。全名在 plan 内钉死唯一属主（stage 标识符
/// 唯一），source 维度不另比——同全名不同 source 的声明在摊平命名空间里
/// 不可能存在。
fn collect_birth_anchor_signals(
    entries: &[StageEntry],
    entrance_fact_keys: &BTreeMap<(String, String), Vec<String>>,
) -> BTreeSet<String> {
    let mut birth: BTreeSet<String> = BTreeSet::new();
    for entry in entries {
        if entry.stage.mint.is_none() {
            continue;
        }
        for raw_expression in entry.stage.receive_signals.values() {
            // 解析失败的条目不构成出生目标：语法错误由引用存在性校验统一上报。
            let Ok(parsed) = crate::lower::parse_hook_for_compiler("HOOK", raw_expression) else {
                continue;
            };
            if let Some(target) = &parsed.subscription_target {
                birth.insert(target.signal_name.clone());
            }
        }
    }
    birth.extend(entrance_fact_keys.keys().map(|(_, signal)| signal.clone()));
    birth
}
