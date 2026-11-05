//! 中立分析入口：共享校验 + hook IR + dock state + EvmStrict capabilities/admissions，
//! 供链轨 TS 接管 hook_plan lowering（不生成链装饰、dependencyIndex、executorRoutes 与 shell）。

use serde_json::{json, Map, Value};
use uvp_hook_dsl::Profile;
use uvp_model::ZhixuDefinition;

use crate::artifact::{build_signal_admissions, build_signal_capabilities};
use crate::docking::{compile_dock_state, dock_entrance_hook_ids};
use crate::lower::{
    build_selected_stage_bindings, flatten_stages, implicit_entry_hook_expression,
    normalize_platform_value, parse_hook_for_compiler, StageEntry, IMPLICIT_ENTRY_HOOK_NAME,
};
use crate::validate::{
    validate_implicit_entry, validate_mint_anchors, validate_onchain_stage_materialization,
    validate_receive_signal_keys, validate_receive_signal_references, validate_stage_executors,
    validate_subscription_delegation, validate_zhixu_shape,
};
use crate::{join_issues_bounded, CompilerError, Result};

pub(crate) const ZHIXU_ANALYSIS_SCHEMA_VERSION: &str = "uvp.zhixuAnalysis.v1";

pub fn compile_zhixu_analysis(
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
        .collect::<std::collections::BTreeSet<_>>();
    let selected_stage_bindings = build_selected_stage_bindings(&stage_entries, &stage_ids)?;
    let dock_state = compile_dock_state(&definition, &stage_pairs, dock_targets, allow_unresolved)?;
    let (implicit_entry, entry_issues) = validate_implicit_entry(&stage_entries);

    let mut validation_issues = Vec::new();
    validation_issues.extend(entry_issues);
    validation_issues.extend(validate_stage_executors(
        &stage_entries,
        &selected_stage_bindings,
    ));
    validation_issues.extend(validate_onchain_stage_materialization(
        &stage_entries,
        &dock_entrance_hook_ids(&dock_state),
        implicit_entry
            .as_ref()
            .map(|entry| entry.stage_identifier.as_str()),
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

    let mut stages = Vec::with_capacity(stage_entries.len());
    for entry in &stage_entries {
        stages.push(stage_analysis(entry)?);
    }

    let entry_hook = match &implicit_entry {
        Some(implicit) => {
            let entry_stage = stage_entries
                .iter()
                .find(|entry| entry.stage_identifier == implicit.stage_identifier)
                .ok_or_else(|| {
                    CompilerError::Message(format!(
                        "implicit entry stage {} is missing from the flattened definition",
                        implicit.stage_identifier
                    ))
                })?;
            Some(entry_hook_analysis(entry_stage)?)
        }
        None => None,
    };

    let signal_capabilities = build_signal_capabilities(&stage_entries)?;
    let admissions = build_signal_admissions(&stage_entries, &dock_state, Profile::EvmStrict)?;
    let dock_entrance_ids = dock_entrance_hook_ids(&dock_state)
        .into_iter()
        .collect::<Vec<_>>();
    let dock_interface = dock_state.interface_json.unwrap_or(Value::Null);
    let dock_routes = Value::Array(dock_state.routes_json);
    let unresolved_dock_routes = Value::Array(dock_state.unresolved_json);

    Ok(json!({
        "schemaVersion": ZHIXU_ANALYSIS_SCHEMA_VERSION,
        "zhixuName": definition.metadata.name,
        "platform": platform,
        "stages": stages,
        "implicitEntry": implicit_entry.as_ref().map(|entry| entry.stage_identifier.as_str()),
        "entryHook": entry_hook,
        "dockEntranceHookIds": dock_entrance_ids,
        "selectedStageBindings": selected_stage_bindings,
        "dockInterface": dock_interface,
        "dockRoutes": dock_routes,
        "unresolvedDockRoutes": unresolved_dock_routes,
        "signalCapabilities": signal_capabilities,
        "admissions": admissions,
    }))
}

fn stage_analysis(entry: &StageEntry) -> Result<Value> {
    let mut stage = Map::new();
    stage.insert(
        "stageIdentifier".to_string(),
        Value::String(entry.stage_identifier.clone()),
    );
    stage.insert("source".to_string(), Value::String(entry.stage.source.clone()));
    stage.insert(
        "mint".to_string(),
        match &entry.stage.mint {
            Some(mint) => Value::String(mint.clone()),
            None => Value::Null,
        },
    );
    stage.insert(
        "executor".to_string(),
        match &entry.stage.executor {
            Some(executor) => serialized(executor)?,
            None => Value::Null,
        },
    );
    stage.insert(
        "selectedStages".to_string(),
        serialized(&entry.stage.selected_stages)?,
    );
    stage.insert(
        "fileResources".to_string(),
        serialized(&entry.stage.file_resources)?,
    );

    let mut send_signals = Vec::with_capacity(entry.stage.send_signals.len());
    for declared in &entry.stage.send_signals {
        let valid_when = match &declared.valid_when {
            Some(items) => serialized(items)?,
            None => Value::Null,
        };
        send_signals.push(json!({
            "name": declared.name,
            "validWhen": valid_when,
        }));
    }
    stage.insert("sendSignals".to_string(), Value::Array(send_signals));

    let mut hooks = Vec::with_capacity(entry.stage.receive_signals.len());
    for (hook_name, raw_expression) in &entry.stage.receive_signals {
        hooks.push(hook_analysis(hook_name, raw_expression)?);
    }
    stage.insert("hooks".to_string(), Value::Array(hooks));

    Ok(Value::Object(stage))
}

fn hook_analysis(hook_name: &str, raw_expression: &str) -> Result<Value> {
    let parsed = parse_hook_for_compiler(hook_name, raw_expression)?;
    Ok(json!({
        "hookName": hook_name,
        "rawExpression": raw_expression,
        "normalizedExpression": parsed.normalized_expression,
        "ast": parsed.ast,
        "dependencies": serialized(&parsed.dependencies)?,
    }))
}

fn entry_hook_analysis(entry: &StageEntry) -> Result<Value> {
    let raw_expression = implicit_entry_hook_expression(entry);
    let parsed = parse_hook_for_compiler(IMPLICIT_ENTRY_HOOK_NAME, &raw_expression)?;
    Ok(json!({
        "rawExpression": raw_expression,
        "normalizedExpression": parsed.normalized_expression,
        "ast": parsed.ast,
        "dependencies": serialized(&parsed.dependencies)?,
    }))
}

fn serialized<T: serde::Serialize>(value: &T) -> Result<Value> {
    serde_json::to_value(value).map_err(|err| CompilerError::Message(err.to_string()))
}
