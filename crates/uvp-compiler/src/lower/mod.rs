//! 定义到目标表示的转换：stage 摊平、platform 归一化、绑定/路由构建与
//! hook 编译（调用 uvp-hook-dsl 的解析管线）。

use serde_json::{json, Map, Value};
use std::collections::BTreeSet;

use uvp_hook_dsl::{parse_hook, Compatibility, ParseHookOutput, ParseHookRequest, Profile};
use uvp_model::{ZhixuDefinition, ZhixuStage};

use crate::docking::{dock_entrance_hook_ids, DockState};
use crate::{CompilerError, Result};

#[derive(Debug, Clone)]
pub(crate) struct StageEntry {
    pub(crate) stage: ZhixuStage,
    pub(crate) stage_identifier: String,
}

pub(crate) fn flatten_stages(definition: &ZhixuDefinition) -> Result<Vec<StageEntry>> {
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

pub(crate) fn build_executor_routes(entries: &[StageEntry]) -> Value {
    let mut routes = Map::new();
    for entry in entries {
        if is_zhixu_executor_stage(entry) {
            // zhixu 委托 route 不进静态 executor route：权威形态是
            // dockRoutes 中的 resolved DockRoute（dockRoutes 是权威形态）。
            continue;
        }
        if entry.stage.executor.is_some() {
            routes.insert(entry.stage_identifier.clone(), route_for_stage(entry));
        }
    }
    Value::Object(routes)
}

pub(crate) fn is_zhixu_executor_stage(entry: &StageEntry) -> bool {
    // 精确比较：带空白的变体已在 validate_zhixu_shape 按闭集外拒绝，
    // 下游不再保留 trim 容忍（单一口径，杜绝"校验侧拒绝、比较侧放行"）。
    entry
        .stage
        .executor
        .as_ref()
        .is_some_and(|executor| executor.supplier_type == "zhixu")
}

pub(crate) fn compile_stage_hooks(
    entry: &StageEntry,
    dock_state: &DockState,
) -> Result<Vec<Value>> {
    let mut hooks = Vec::new();
    let is_mint_stage = entry.stage.mint.is_some();
    // entrance 端口引用的目标侧 hook 是 dock 出生入口。
    let entrance_hook_ids = dock_entrance_hook_ids(dock_state);
    let is_zhixu_stage = is_zhixu_executor_stage(entry);
    for (hook_name, raw_expression) in &entry.stage.receive_signals {
        let hook_id = format!("{}#{hook_name}", entry.stage_identifier);
        let order_trigger_kind = if is_mint_stage {
            "mint"
        } else if entrance_hook_ids.contains(&hook_id) {
            "dock"
        } else {
            "none"
        };
        // emitReady：出生/委托入口必发；有执行者的 stage 的 receive hook
        // 是 executor dispatch 边（触发/派发拆分）。
        let emit_ready = order_trigger_kind != "none" || entry.stage.executor.is_some();
        let route = if is_zhixu_stage {
            None
        } else {
            entry
                .stage
                .executor
                .as_ref()
                .map(|_| route_for_stage(entry))
        };
        hooks.push(compile_hook(
            "receive",
            &entry.stage_identifier,
            hook_name,
            order_trigger_kind,
            emit_ready,
            raw_expression,
            route,
        )?);
    }
    hooks.sort_by_key(|hook| value_str(hook, "hookId").to_lowercase());
    Ok(hooks)
}

fn compile_hook(
    kind: &str,
    stage_identifier: &str,
    hook_name: &str,
    order_trigger_kind: &str,
    emit_ready: bool,
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
    hook.insert(
        "orderTriggerKind".to_string(),
        Value::String(order_trigger_kind.to_string()),
    );
    hook.insert("emitReady".to_string(), Value::Bool(emit_ready));
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

pub(crate) fn parse_hook_for_compiler(
    hook_name: &str,
    raw_expression: &str,
) -> Result<ParseHookOutput> {
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

pub(crate) fn parse_hook_for_cloud(
    hook_name: &str,
    raw_expression: &str,
) -> Result<ParseHookOutput> {
    parse_hook(ParseHookRequest {
        profile: Profile::CloudCompat,
        hook_name: hook_name.to_string(),
        hook: raw_expression.to_string(),
    })
    .map_err(|err| CompilerError::Message(err.to_string()))
}

fn route_for_stage(entry: &StageEntry) -> Value {
    let mut route = Map::new();
    route.insert(
        "stageIdentifier".to_string(),
        Value::String(entry.stage_identifier.clone()),
    );
    if let Some(executor) = &entry.stage.executor {
        route.insert(
            "executor".to_string(),
            serde_json::to_value(executor).unwrap_or(Value::Null),
        );
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

pub(crate) fn value_str<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or_default()
}
