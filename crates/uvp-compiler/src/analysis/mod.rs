//! 中立分析入口：共享校验 + hook IR + trigger 集 + EvmStrict
//! capabilities/admissions，供链轨 TS 接管 hook_plan lowering（不生成链装饰、
//! dependencyIndex、executorRoutes 与 shell）。

use serde_json::{json, Map, Value};
use uvp_hook_dsl::Profile;
use uvp_model::ZhixuDefinition;

use crate::artifact::{build_signal_admissions, build_signal_capabilities};
use crate::lower::{
    build_selected_stage_bindings, flatten_stages, normalize_platform_value,
    parse_hook_for_compiler, StageEntry,
};
use crate::validate::{
    validate_onchain_stage_materialization, validate_receive_signal_keys,
    validate_receive_signal_references, validate_stage_executors, validate_stage_triggers,
    validate_zhixu_shape,
};
use crate::{join_issues_bounded, CompilerError, Result};

pub(crate) const ZHIXU_ANALYSIS_SCHEMA_VERSION: &str = "uvp.zhixuAnalysis.v4";

pub fn compile_zhixu_analysis(definition_value: &Value) -> Result<Value> {
    let definition: ZhixuDefinition = serde_json::from_value(definition_value.clone())
        .map_err(|err| CompilerError::Message(format!("invalid Zhixu definition: {err}")))?;
    let issues = validate_zhixu_shape(&definition);
    if !issues.is_empty() {
        return Err(CompilerError::Issues(join_issues_bounded(&issues)));
    }

    let stage_entries = flatten_stages(&definition)?;
    let stage_ids = stage_entries
        .iter()
        .map(|entry| entry.stage_identifier.clone())
        .collect::<std::collections::BTreeSet<_>>();
    let selected_stage_bindings = build_selected_stage_bindings(&stage_entries, &stage_ids)?;

    let mut validation_issues = Vec::new();
    validation_issues.extend(validate_stage_executors(
        &stage_entries,
        &selected_stage_bindings,
    ));
    validation_issues.extend(validate_onchain_stage_materialization(&stage_entries));
    validation_issues.extend(validate_stage_triggers(&stage_entries));
    validation_issues.extend(validate_receive_signal_keys(&stage_entries));
    validation_issues.extend(validate_receive_signal_references(&stage_entries));
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

    let signal_capabilities = build_signal_capabilities(&stage_entries)?;
    let admissions = build_signal_admissions(&stage_entries, Profile::EvmStrict)?;

    Ok(json!({
        "schemaVersion": ZHIXU_ANALYSIS_SCHEMA_VERSION,
        "zhixuName": definition.metadata.name,
        "platform": platform,
        "stages": stages,
        "selectedStageBindings": selected_stage_bindings,
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
    stage.insert(
        "source".to_string(),
        Value::String(entry.stage.source.clone()),
    );
    stage.insert(
        "triggers".to_string(),
        serialized(&trigger_signal_names(entry))?,
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
        "fileWriters".to_string(),
        serialized(&entry.stage.file_writers)?,
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

pub(crate) fn trigger_signal_names(entry: &StageEntry) -> Vec<String> {
    entry.stage.trigger_signal_names()
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

fn serialized<T: serde::Serialize>(value: &T) -> Result<Value> {
    serde_json::to_value(value).map_err(|err| CompilerError::Message(err.to_string()))
}
