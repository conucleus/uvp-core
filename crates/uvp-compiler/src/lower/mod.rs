//! 定义到中立表示的转换：stage 遍历、platform 归一化、绑定构建与 hook 解析
//! （调用 uvp-hook-dsl 的解析管线）。

use serde_json::{json, Map, Value};
use std::collections::BTreeSet;

use uvp_hook_dsl::{parse_hook, Compatibility, Gate, ParseHookOutput, ParseHookRequest, Profile};
use uvp_model::{ZhixuDefinition, ZhixuStage};

use crate::{CompilerError, Result};

#[derive(Debug, Clone)]
pub(crate) struct StageEntry {
    pub(crate) stage: ZhixuStage,
    pub(crate) stage_identifier: String,
}

pub(crate) fn flatten_stages(definition: &ZhixuDefinition) -> Result<Vec<StageEntry>> {
    let mut entries = Vec::new();
    let mut stage_names = BTreeSet::new();
    for stage in &definition.spec.stages {
        if !stage_names.insert(stage.name.clone()) {
            return Err(CompilerError::Issues(format!(
                "duplicate stage {}",
                stage.name
            )));
        }
        entries.push(StageEntry {
            stage: stage.clone(),
            stage_identifier: stage.name.clone(),
        });
    }
    Ok(entries)
}

pub(crate) fn normalize_platform_value(platform: &uvp_model::ZhixuPlatform) -> Result<Value> {
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

pub(crate) fn build_selected_stage_bindings(
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

pub(crate) fn is_zhixu_executor_stage(entry: &StageEntry) -> bool {
    entry
        .stage
        .executor
        .as_ref()
        .is_some_and(|executor| executor.supplier_type == "zhixu")
}

pub(crate) const IMPLICIT_ENTRY_HOOK_NAME: &str = "str";

pub(crate) fn implicit_entry_hook_expression(entry: &StageEntry) -> String {
    format!("{}::{}.str", entry.stage.source, entry.stage_identifier)
}

pub(crate) fn parse_hook_for_compiler(
    hook_name: &str,
    raw_expression: &str,
) -> Result<ParseHookOutput> {
    let parsed = parse_hook(ParseHookRequest {
        profile: Profile::EvmStrict,
        gate: Gate::Hook,
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

pub(crate) fn parse_hook_for_cloud(
    hook_name: &str,
    raw_expression: &str,
) -> Result<ParseHookOutput> {
    parse_hook(ParseHookRequest {
        profile: Profile::CloudCompat,
        gate: Gate::Hook,
        hook_name: hook_name.to_string(),
        hook: raw_expression.to_string(),
    })
    .map_err(|err| CompilerError::Message(err.to_string()))
}

pub(crate) fn value_str<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or_default()
}
