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

pub const HOOK_PLAN_SCHEMA_VERSION: &str = "uvp.hookPlan.v4";
pub const CLOUD_ARTIFACT_SCHEMA_VERSION: &str = "uvp.cloudArtifact.v5";

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

    let dock_state = compile_dock_state(&definition, &stage_pairs, dock_targets, allow_unresolved)?;

    let mut validation_issues = Vec::new();
    validation_issues.extend(validate_stage_executors(
        &stage_entries,
        &selected_stage_bindings,
    ));
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
    let dock_state = compile_dock_state(&definition, &stage_pairs, dock_targets, allow_unresolved)?;

    let mut validation_issues = Vec::new();
    validation_issues.extend(validate_stage_executors(
        &stage_entries,
        &selected_stage_bindings,
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
    build_signal_capabilities(&stage_entries)?;
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
    capabilities.sort_by(|left, right| {
        value_str(left, "stageIdentifier")
            .cmp(value_str(right, "stageIdentifier"))
            .then(value_str(left, "targetSource").cmp(value_str(right, "targetSource")))
            .then(value_str(left, "targetSignalName").cmp(value_str(right, "targetSignalName")))
    });
    Ok(capabilities)
}

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
                "{}.sendSignals contains invalid canonical signal {:?}: expected stage.signal with identifier-grammar segments",
                entry.stage_identifier, declared_signal
            )));
        }
        if !declared_signal.starts_with(&format!("{}.", entry.stage_identifier)) {
            return Err(CompilerError::Issues(format!(
                "{}.sendSignals contains canonical signal {:?} that does not address the declaring stage: expected {}.<signal> (the two-part form is an explicit self-reference; bare names expand to the same capability)",
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
        let anchorless_channel = stage_is_subscription(&entry.stage)
            && !minted_sources.contains(entry.stage.source.as_str());
        for declared in &entry.stage.send_signals {
            let Some(valid_when) = &declared.valid_when else {
                continue;
            };
            if valid_when.is_empty() {
                return Err(CompilerError::Issues(format!(
                    "D027 {}.sendSignals[{}].validWhen: must be a non-empty array; drop the key to declare unconditional admission",
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
            if dock_state.output_relay_signals.contains(&full_name) {
                return Err(CompilerError::Issues(format!(
                    "D029 {}.sendSignals[{}].validWhen: {} is a signalMap relay target (dock output relay writes the fact through the engine-internal ingress, which bypasses the admission face; a validWhen here is dead code)",
                    entry.stage_identifier, declared.name, full_name
                )));
            }
            let mut parsed_items = Vec::with_capacity(valid_when.len());
            for (index, item) in valid_when.iter().enumerate() {
                if item.trim().is_empty() {
                    return Err(CompilerError::Issues(format!(
                        "D027 {}.sendSignals[{}].validWhen[{index}]: must be a non-blank item; drop the key to declare unconditional admission",
                        entry.stage_identifier, declared.name
                    )));
                }
                let parsed = parse_hook(ParseHookRequest {
                    profile,
                    gate: Gate::Filter,
                    hook_name: "ADMIT".to_string(),
                    hook: item.clone(),
                })
                .map_err(|err| {
                    CompilerError::Issues(format!(
                        "{}.sendSignals[{}].validWhen[{index}] is invalid: {err}",
                        entry.stage_identifier, declared.name
                    ))
                })?;
                if parsed.mode == HookMode::Subscription {
                    return Err(CompilerError::Issues(format!(
                        "D030 {}.sendSignals[{}].validWhen[{index}]: admission is a per-order state judgment and must not contain subscription atoms (ANCHOR(@…))",
                        entry.stage_identifier, declared.name
                    )));
                }
                if parsed
                    .dependencies
                    .iter()
                    .any(|dependency| dependency.signal_name == full_name)
                {
                    return Err(CompilerError::Issues(format!(
                        "D028 {}.sendSignals[{}].validWhen[{index}]: expression addresses the declaring signal itself ({}); admission judges the pre-state, which never contains the emission being judged",
                        entry.stage_identifier, declared.name, full_name
                    )));
                }
                let admission_issues = crate::validate::validate_signal_references(
                    &parsed,
                    &format!(
                        "{}.sendSignals[{}].validWhen[{index}]",
                        entry.stage_identifier, declared.name
                    ),
                    entries,
                );
                if !admission_issues.is_empty() {
                    return Err(CompilerError::Issues(admission_issues.join("; ")));
                }
                parsed_items.push(parsed);
            }
            let header = parsed_items[0].source.clone();
            if let Some((index, divergent)) =
                parsed_items
                    .iter()
                    .enumerate()
                    .skip(1)
                    .find_map(|(index, parsed)| {
                        (parsed.source != header).then(|| (index, parsed.source.clone()))
                    })
            {
                return Err(CompilerError::Issues(format!(
                    "D032 {}.sendSignals[{}].validWhen: every item must address the same header source ({header}); item {index} addresses {divergent} (the admission artifact carries a single source tag)",
                    entry.stage_identifier, declared.name
                )));
            }
            let mut admission = Map::new();
            admission.insert(
                "stageIdentifier".to_string(),
                Value::String(entry.stage_identifier.clone()),
            );
            admission.insert("signalName".to_string(), Value::String(full_name));
            admission.insert(
                "rawExpression".to_string(),
                Value::Array(
                    valid_when
                        .iter()
                        .map(|item| Value::String(item.clone()))
                        .collect(),
                ),
            );
            if profile == Profile::CloudCompat {
                let mut dependencies = Vec::new();
                let mut seen: BTreeSet<(String, DependencyKind)> = BTreeSet::new();
                for parsed in &parsed_items {
                    for dependency in &parsed.dependencies {
                        if dependency.kind == DependencyKind::Timer {
                            continue;
                        }
                        if !seen.insert((dependency.signal_name.clone(), dependency.kind)) {
                            continue;
                        }
                        dependencies.push(json!({
                            "signalName": dependency.signal_name,
                            "dependencyKind": dependency.kind,
                        }));
                    }
                }
                admission.insert(
                    "cloudAst".to_string(),
                    compose_cloud_admission_ast(&parsed_items),
                );
                admission.insert("dependencies".to_string(), Value::Array(dependencies));
            } else {
                let mut dependencies = Vec::new();
                let mut seen: BTreeSet<(String, String, DependencyKind, Option<i64>)> =
                    BTreeSet::new();
                for parsed in &parsed_items {
                    for dependency in &parsed.dependencies {
                        if !seen.insert((
                            dependency.source.clone(),
                            dependency.signal_name.clone(),
                            dependency.kind,
                            dependency.delay_seconds,
                        )) {
                            continue;
                        }
                        dependencies.push(
                            serde_json::to_value(dependency)
                                .map_err(|err| CompilerError::Message(err.to_string()))?,
                        );
                    }
                }
                admission.insert(
                    "normalizedExpression".to_string(),
                    Value::String(compose_normalized_admission(&parsed_items)),
                );
                admission.insert(
                    "ast".to_string(),
                    compose_ast_admission(&parsed_items, valid_when),
                );
                admission.insert("dependencies".to_string(), Value::Array(dependencies));
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

fn compose_cloud_admission_ast(parsed_items: &[uvp_hook_dsl::ParseHookOutput]) -> Value {
    let roots: Vec<Value> = parsed_items
        .iter()
        .map(|parsed| parsed.cloud_ast["root"].clone())
        .collect();
    json!({
        "schemaVersion": parsed_items[0].cloud_ast["schemaVersion"],
        "source": parsed_items[0].source,
        "mode": HookMode::Normal,
        "root": fold_admission_roots(&roots),
    })
}

fn fold_admission_roots(roots: &[Value]) -> Value {
    match roots.len() {
        1 => roots[0].clone(),
        len => {
            let mid = len / 2;
            json!({
                "type": "and",
                "left": fold_admission_roots(&roots[..mid]),
                "right": fold_admission_roots(&roots[mid..]),
            })
        }
    }
}

fn compose_normalized_admission(parsed_items: &[uvp_hook_dsl::ParseHookOutput]) -> String {
    if parsed_items.len() == 1 {
        return parsed_items[0].normalized_expression.clone();
    }
    let conditions: Vec<String> = parsed_items
        .iter()
        .map(|parsed| {
            let condition = parsed
                .normalized_expression
                .split_once("::")
                .map(|(_, condition)| condition)
                .unwrap_or(parsed.normalized_expression.as_str());
            if has_top_level_or(condition) {
                format!("({condition})")
            } else {
                condition.to_string()
            }
        })
        .collect();
    format!("{}::{}", parsed_items[0].source, conditions.join("&"))
}

fn has_top_level_or(condition: &str) -> bool {
    let mut depth = 0i32;
    for character in condition.chars() {
        match character {
            '(' => depth += 1,
            ')' => depth -= 1,
            '|' if depth == 0 => return true,
            _ => {}
        }
    }
    false
}

fn compose_ast_admission(
    parsed_items: &[uvp_hook_dsl::ParseHookOutput],
    raw_items: &[String],
) -> Value {
    if parsed_items.len() == 1 {
        return parsed_items[0].ast.clone();
    }
    let terms: Vec<Value> = parsed_items
        .iter()
        .map(|parsed| parsed.ast["condition"].clone())
        .collect();
    json!({
        "raw": raw_items.join(" & "),
        "source": parsed_items[0].source,
        "condition": {"kind": "and", "terms": terms},
    })
}

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
