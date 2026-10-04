//! 定义校验：形状/尺寸闸、执行器绑定、物化门、mint/订阅/信号引用等
//! 全部编译期校验规则的汇集地。

use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, VecDeque};

use uvp_hook_dsl::ParseHookOutput;
use uvp_model::{ZhixuDefinition, ZhixuExecutor, ZhixuStage};

use crate::lower::{is_zhixu_executor_stage, parse_hook_for_compiler, value_str, StageEntry};

const MAX_STAGE_SOURCE_BYTES: usize = 36;
const MAX_IDENTIFIER_BYTES: usize = 100;
const MAX_SIGNAL_NAME_BYTES: usize = 100;
const NAME_SLUG_PATTERN: &str = "^[a-z][a-z0-9_-]{0,99}$";

const MAX_TASK_PATTERNS: usize = 64;
const MAX_STAGE_ENTRIES: usize = 256;
const MAX_HOOKS: usize = 512;

const MAX_SUPPLIER_NAME_BYTES: usize = 100;
const MAX_SUPPLIER_TYPE_BYTES: usize = 60;
const MAX_SUPPLIER_REAL_ID_TYPE_BYTES: usize = 20;
const MAX_SUPPLIER_REAL_ID_BYTES: usize = 100;

pub(crate) fn validate_zhixu_shape(definition: &ZhixuDefinition) -> Vec<String> {
    let mut issues = Vec::new();
    if definition.api_version != "uvp/v0" {
        issues.push("apiVersion must be uvp/v0".to_string());
    }
    if definition.kind != "Zhixu" {
        issues.push("kind must be Zhixu".to_string());
    }
    if definition.metadata.name.trim().is_empty() {
        issues.push("metadata.name must be non-empty".to_string());
    }
    if definition.metadata.name.len() > MAX_IDENTIFIER_BYTES {
        issues.push(format!(
            "metadata.name {:?} exceeds {MAX_IDENTIFIER_BYTES} bytes",
            definition.metadata.name
        ));
    }
    if !is_name_slug(&definition.metadata.name) {
        issues.push(format!(
            "metadata.name {:?} must match {NAME_SLUG_PATTERN} (definition-local technical label)",
            definition.metadata.name
        ));
    }
    if definition.spec.platform.platform_type.trim().is_empty() {
        issues.push("spec.platform must be an object with a non-empty type".to_string());
    }
    if definition.spec.nucleation.id.trim().is_empty() {
        issues.push("spec.nucleation.id must be non-empty".to_string());
    }
    if definition.spec.task_patterns.is_empty() {
        issues.push("spec.taskPatterns must contain at least one task pattern".to_string());
    }
    if definition.spec.task_patterns.len() > MAX_TASK_PATTERNS {
        issues.push(format!(
            "spec.taskPatterns contains {} task patterns, limit is {MAX_TASK_PATTERNS}",
            definition.spec.task_patterns.len()
        ));
    }
    let stage_total: usize = definition
        .spec
        .task_patterns
        .iter()
        .map(|task| task.stages.len())
        .sum();
    if stage_total > MAX_STAGE_ENTRIES {
        issues.push(format!(
            "definition flattens to {stage_total} stages across taskPatterns, limit is {MAX_STAGE_ENTRIES}"
        ));
    }
    let hook_total: usize = definition
        .spec
        .task_patterns
        .iter()
        .flat_map(|task| task.stages.iter())
        .map(|stage| stage.receive_signals.len())
        .sum();
    if hook_total > MAX_HOOKS {
        issues.push(format!(
            "definition declares {hook_total} receiveSignals channels (compiled hooks) across taskPatterns, limit is {MAX_HOOKS}"
        ));
    }
    for (task_index, task) in definition.spec.task_patterns.iter().enumerate() {
        if !valid_identifier_part(&task.name) {
            issues.push(format!(
                "spec.taskPatterns[{task_index}].name must start with an ASCII letter and contain only ASCII letters, digits, '_' or '-': {}",
                task.name
            ));
        }
        if task.stages.is_empty() {
            issues.push(format!(
                "spec.taskPatterns[{task_index}].stages must contain at least one stage",
            ));
        }
        for (stage_index, stage) in task.stages.iter().enumerate() {
            if !valid_identifier_part(&stage.name) {
                issues.push(format!(
                    "spec.taskPatterns[{task_index}].stages[{stage_index}].name must start with an ASCII letter and contain only ASCII letters, digits, '_' or '-': {}",
                    stage.name
                ));
            }
            if let Some(executor) = &stage.executor {
                if !uvp_model::is_known_supplier_type(&executor.supplier_type) {
                    issues.push(format!(
                        "spec.taskPatterns[{task_index}].stages[{stage_index}].executor.supplierType must be one of {} (exact match, whitespace variants rejected), found {:?}",
                        uvp_model::SUPPLIER_TYPES
                            .map(|value| format!("{value:?}"))
                            .join(", "),
                        executor.supplier_type
                    ));
                }
                if let Some(selectable) = &executor.selectable_resource {
                    let path = format!(
                        "spec.taskPatterns[{task_index}].stages[{stage_index}].executor.selectableResource"
                    );
                    match selectable.as_object() {
                        Some(entries) => {
                            for (key, resource) in entries {
                                if let Some(issue) = file_resource_type_issue(&path, key, resource)
                                {
                                    issues.push(issue);
                                }
                            }
                        }
                        None => issues.push(format!("{path} must be a map of file resources")),
                    }
                }
            }
            for (key, resource) in &stage.file_resources {
                let path =
                    format!("spec.taskPatterns[{task_index}].stages[{stage_index}].fileResources");
                if let Some(issue) = file_resource_type_issue(&path, key, resource) {
                    issues.push(issue);
                }
            }
            if stage.source.trim().is_empty() {
                issues.push(format!(
                    "spec.taskPatterns[{task_index}].stages[{stage_index}].source must be non-empty"
                ));
            } else {
                if !is_plain_source_identifier(&stage.source) {
                    issues.push(format!(
                        "spec.taskPatterns[{task_index}].stages[{stage_index}].source must be a plain identifier (ASCII letters, digits, '_' or '-'): {}",
                        stage.source
                    ));
                }
                if stage.source.len() > MAX_STAGE_SOURCE_BYTES {
                    issues.push(format!(
                        "spec.taskPatterns[{task_index}].stages[{stage_index}].source {:?} exceeds {MAX_STAGE_SOURCE_BYTES} bytes",
                        stage.source
                    ));
                }
            }
            let stage_identifier = format!("{}.{}", task.name, stage.name);
            if stage_identifier.len() > MAX_IDENTIFIER_BYTES {
                issues.push(format!(
                    "spec.taskPatterns[{task_index}].stages[{stage_index}] identifier {stage_identifier:?} exceeds {MAX_IDENTIFIER_BYTES} bytes (global_stage.stage_identifier)"
                ));
            }
            for signal in &stage.send_signals {
                let full_name_bytes = if signal.name.contains('.') {
                    signal.name.len()
                } else {
                    stage_identifier.len() + 1 + signal.name.len()
                };
                if full_name_bytes > MAX_SIGNAL_NAME_BYTES {
                    issues.push(format!(
                        "spec.taskPatterns[{task_index}].stages[{stage_index}] ({stage_identifier:?}) sendSignal {:?} exceeds {MAX_SIGNAL_NAME_BYTES} bytes combined (individual_record.signal_name)",
                        signal.name
                    ));
                }
            }
        }
    }
    issues
}

pub(crate) fn validate_supplier_dimensions(supplier: &Value) -> Result<(), String> {
    let mut issues = Vec::new();
    let spec = supplier.get("spec");
    let name = supplier
        .get("metadata")
        .map(|metadata| value_str(metadata, "name"))
        .unwrap_or_default()
        .trim();
    if name.is_empty() {
        issues.push("metadata.name is required and cannot be blank".to_string());
    }
    if name.len() > MAX_SUPPLIER_NAME_BYTES {
        issues.push(format!(
            "supplier name {name:?} exceeds {MAX_SUPPLIER_NAME_BYTES} bytes (global_supplier.name)"
        ));
    }
    let supplier_type = spec
        .map(|spec| value_str(spec, "supplierType"))
        .unwrap_or_default();
    if supplier_type.len() > MAX_SUPPLIER_TYPE_BYTES {
        issues.push(format!(
            "supplierType {supplier_type:?} exceeds {MAX_SUPPLIER_TYPE_BYTES} bytes (global_supplier.type)"
        ));
    }
    let real_id_type = spec
        .map(|spec| value_str(spec, "realIdType"))
        .unwrap_or_default();
    if real_id_type.len() > MAX_SUPPLIER_REAL_ID_TYPE_BYTES {
        issues.push(format!(
            "realIdType {real_id_type:?} exceeds {MAX_SUPPLIER_REAL_ID_TYPE_BYTES} bytes (global_supplier.real_id_type)"
        ));
    }
    let real_id = spec
        .map(|spec| value_str(spec, "realId"))
        .unwrap_or_default();
    if real_id.len() > MAX_SUPPLIER_REAL_ID_BYTES {
        issues.push(format!(
            "realId {real_id:?} exceeds {MAX_SUPPLIER_REAL_ID_BYTES} bytes (global_supplier.real_id)"
        ));
    }
    if issues.is_empty() {
        Ok(())
    } else {
        Err(crate::join_issues_bounded(&issues))
    }
}

fn is_plain_source_identifier(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
}

fn file_resource_type_issue(path: &str, key: &str, resource: &Value) -> Option<String> {
    let file_type = resource.get("fileType").and_then(Value::as_str);
    if file_type.is_some_and(uvp_model::is_known_file_type) {
        return None;
    }
    Some(format!(
        "{path}[{key:?}].fileType must be one of {} (exact match, whitespace variants rejected), found {:?}",
        uvp_model::FILE_TYPES
            .map(|value| format!("{value:?}"))
            .join(", "),
        resource.get("fileType")
    ))
}

pub(crate) fn is_definition_uid(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 35
        && bytes.starts_with(b"zx-")
        && bytes[3..]
            .iter()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(b))
}

pub(crate) fn is_name_slug(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.is_empty() || bytes.len() > MAX_IDENTIFIER_BYTES || !bytes[0].is_ascii_lowercase() {
        return false;
    }
    bytes[1..]
        .iter()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'_' || *b == b'-')
}

pub(crate) fn valid_identifier_part(value: &str) -> bool {
    let mut chars = value.chars();
    matches!(chars.next(), Some(ch) if ch.is_ascii_alphabetic())
        && chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
}

pub(crate) fn validate_stage_executors(entries: &[StageEntry], bindings: &[Value]) -> Vec<String> {
    let mut issues = Vec::new();
    for entry in entries {
        let Some(executor) = entry.stage.executor.as_ref() else {
            continue;
        };
        if executor.supplier_type == "zhixu" {
            continue;
        }
        if executor
            .supplier_id
            .as_deref()
            .is_none_or(|value| value.trim().is_empty())
        {
            issues.push(format!(
                "{}.executor.supplierID is required when supplierType is {:?}",
                entry.stage_identifier, executor.supplier_type
            ));
        }
    }
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

pub(crate) fn stage_is_subscription(stage: &ZhixuStage) -> bool {
    stage.receive_signals.values().any(|raw| {
        parse_hook_for_compiler("HOOK", raw)
            .map(|parsed| parsed.mode == uvp_hook_dsl::HookMode::Subscription)
            .unwrap_or(false)
    })
}

pub(crate) fn validate_onchain_stage_materialization(
    entries: &[StageEntry],
    entrance_hook_ids: &BTreeSet<String>,
) -> Vec<String> {
    let mut issues = Vec::new();
    for entry in entries {
        if entry.stage.receive_signals.is_empty() {
            issues.push(format!(
                "{} declares no receiveSignals and compiles to zero hooks: the stage can never materialize on-chain (materialization only happens via this stage's own order-trigger/EMIT_READY hooks) and its sendSignals have no hook to hang on — submitSignal requires the source stage to be materialized and reverts UnknownHook forever (deadlock, no recovery path); declare receiveSignals carrying a mint/dock entrance or a static executor",
                entry.stage_identifier
            ));
            continue;
        }
        let has_materializing_hook = entry.stage.executor.is_some()
            || entry.stage.mint.is_some()
            || entry.stage.receive_signals.keys().any(|hook_name| {
                entrance_hook_ids.contains(&format!("{}#{hook_name}", entry.stage_identifier))
            });
        if has_materializing_hook {
            continue;
        }
        for hook_name in entry.stage.receive_signals.keys() {
            issues.push(format!(
                "{}.receiveSignals.{}: stage has no order-trigger or EMIT_READY hook; its hooks compile to flags=0 watchers which can never materialize the stage on-chain (deadlock, no recovery path) — declare a static executor or drop receiveSignals from this stage",
                entry.stage_identifier, hook_name
            ));
        }
    }
    issues
}

pub(crate) fn validate_subscription_delegation(entries: &[StageEntry]) -> Vec<String> {
    let minted_sources: BTreeSet<&str> = entries
        .iter()
        .filter(|entry| entry.stage.mint.is_some())
        .map(|entry| entry.stage.source.as_str())
        .collect();
    let mut issues = Vec::new();
    for entry in entries {
        if entry.stage.mint.is_some() || !is_zhixu_executor_stage(entry) {
            continue;
        }
        if minted_sources.contains(entry.stage.source.as_str()) {
            continue;
        }
        for (hook_name, raw_expression) in &entry.stage.receive_signals {
            let Ok(parsed) = parse_hook_for_compiler("HOOK", raw_expression) else {
                continue;
            };
            if parsed.mode == uvp_hook_dsl::HookMode::Subscription {
                issues.push(format!(
                    "{}.receiveSignals.{hook_name}: unanchored fan-in subscription stage cannot bind a zhixu delegation executor (fan-in delivery has no order context; the delegation envelope only carries same-order child signals)",
                    entry.stage_identifier
                ));
            }
        }
    }
    issues
}

pub(crate) fn validate_mint_anchors(
    entries: &[StageEntry],
    entrance_fact_keys: &BTreeMap<(String, String), Vec<String>>,
) -> Vec<String> {
    let mut issues = Vec::new();
    for entry in entries {
        if let Some(mint) = &entry.stage.mint {
            if mint != "per-fact" {
                issues.push(format!(
                    "{}.mint only supports per-fact: {}",
                    entry.stage_identifier, mint
                ));
            }
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
                    .map(|executor| executor.supplier_type.clone())
                    .unwrap_or_default();
                if executor_type == "zhixu" {
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
                    Err(_) => {}
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
                        issues.push(format!(
                            "{}.receiveSignals.{hook_name}: mint stage accepts ANCHOR(@…) subscription entries only; plain birth-entry hooks are retired",
                            entry.stage_identifier
                        ));
                    }
                }
            }
        }
    }
    issues.extend(validate_mint_subscription_cycles(entries));
    issues.extend(validate_birth_channel_key_uniqueness(
        entries,
        entrance_fact_keys,
    ));
    issues
}

fn validate_mint_subscription_cycles(entries: &[StageEntry]) -> Vec<String> {
    let mut edges: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for entry in entries {
        if entry.stage.mint.is_none() {
            continue;
        }
        let source = entry.stage.source.clone();
        for raw_expression in entry.stage.receive_signals.values() {
            let Ok(parsed) = parse_hook_for_compiler("HOOK", raw_expression) else {
                continue;
            };
            let Some(target) = &parsed.subscription_target else {
                continue;
            };
            if target.source != source {
                edges
                    .entry(source.clone())
                    .or_default()
                    .insert(target.source.clone());
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

fn validate_birth_channel_key_uniqueness(
    entries: &[StageEntry],
    entrance_fact_keys: &BTreeMap<(String, String), Vec<String>>,
) -> Vec<String> {
    let mut stages_by_key: BTreeMap<(String, String), BTreeSet<&str>> = BTreeMap::new();
    for entry in entries {
        if entry.stage.mint.is_none() {
            continue;
        }
        for raw_expression in entry.stage.receive_signals.values() {
            let Ok(parsed) = parse_hook_for_compiler("HOOK", raw_expression) else {
                continue;
            };
            let Some(target) = &parsed.subscription_target else {
                continue;
            };
            stages_by_key
                .entry((target.source.clone(), target.signal_name.clone()))
                .or_default()
                .insert(entry.stage_identifier.as_str());
        }
    }
    let mut keys: BTreeSet<&(String, String)> = stages_by_key.keys().collect();
    keys.extend(entrance_fact_keys.keys());
    keys.into_iter()
        .filter_map(|key| {
            let stages: Vec<&str> = stages_by_key
                .get(key)
                .map(|stages| stages.iter().copied().collect())
                .unwrap_or_default();
            let ports: Vec<&str> = entrance_fact_keys
                .get(key)
                .map(|ports| ports.iter().map(String::as_str).collect())
                .unwrap_or_default();
            if stages.len() + ports.len() < 2 {
                return None;
            }
            let (source, signal) = (&key.0, &key.1);
            if ports.is_empty() {
                let stages = stages.join(", ");
                return Some(format!(
                    "{source}::{signal} is declared as the birth entry by multiple mint stages ({stages}): one fact mints at most one order (一事一单：同一事实至多铸一单；多阶段消费请改用 hook 依赖)"
                ));
            }
            let mut claimants = Vec::new();
            if !stages.is_empty() {
                claimants.push(format!("mint stage(s) ({})", stages.join(", ")));
            }
            if ports.len() > 1 || !stages.is_empty() {
                claimants.push(format!("dock entrance port(s) ({})", ports.join(", ")));
            }
            Some(format!(
                "{source}::{signal} is declared as a birth channel by both {}: one fact keys at most one birth channel in the plan (出生通道键并集查重：mint 出生键 ∪ dock entrance 键内不得重复)",
                claimants.join(" and ")
            ))
        })
        .collect()
}

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

pub(crate) fn validate_receive_signal_references(
    entries: &[StageEntry],
    input_port_hook_ids: &BTreeSet<String>,
) -> Vec<String> {
    let mut issues = Vec::new();
    let catalog = SignalReferenceCatalog::new(entries);
    for entry in entries {
        for (hook_name, raw_expression) in &entry.stage.receive_signals {
            let hook_id = format!("{}#{hook_name}", entry.stage_identifier);
            if input_port_hook_ids.contains(&hook_id) {
                continue;
            }
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

pub(crate) fn validate_signal_references(
    hook: &ParseHookOutput,
    path: &str,
    entries: &[StageEntry],
) -> Vec<String> {
    let catalog = SignalReferenceCatalog::new(entries);
    validate_hook_dependency_references(hook, path, &catalog)
}

pub(crate) fn validate_receive_signal_keys(entries: &[StageEntry]) -> Vec<String> {
    let mut issues = Vec::new();
    for entry in entries {
        for hook_name in entry.stage.receive_signals.keys() {
            if hook_name.trim().is_empty() {
                issues.push(format!(
                    "{}.receiveSignals contains an empty hook name",
                    entry.stage_identifier
                ));
                continue;
            }
            if hook_name.contains('.')
                || hook_name.contains('#')
                || hook_name.len() > 36
                || hook_name.chars().any(char::is_whitespace)
            {
                issues.push(format!(
                    "{}.receiveSignals.{hook_name} is invalid: key must be 1-36 bytes and must not contain '.', '#' or whitespace",
                    entry.stage_identifier
                ));
            }
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
        if !catalog.local_sources.contains(&dependency.source) {
            issues.push(format!(
                "{path} references source {} that is not a declared source in this zhixu",
                dependency.source
            ));
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
        if !declares_signal_expanding_to(
            &referenced_stage.stage.send_signals,
            &referenced_stage.stage_identifier,
            &dependency.signal_name,
        ) {
            issues.push(format!(
                "{path} references unknown signal {stage_identifier}.{signal_name}"
            ));
        }
    }
    issues
}

pub(crate) fn declares_signal_expanding_to(
    send_signals: &[uvp_model::ZhixuSendSignal],
    stage_identifier: &str,
    full_signal_name: &str,
) -> bool {
    send_signals.iter().any(|declared| {
        if declared.name == full_signal_name {
            return true;
        }
        !declared.name.contains('.')
            && format!("{stage_identifier}.{}", declared.name) == full_signal_name
    })
}

fn parse_signal_reference(signal_name: &str) -> Option<(String, String)> {
    let parts = signal_name.split('.').collect::<Vec<_>>();
    if parts.len() != 3 {
        return None;
    }
    Some((format!("{}.{}", parts[0], parts[1]), parts[2].to_string()))
}

pub(crate) fn valid_signal_declaration(value: &str) -> bool {
    let parts: Vec<&str> = value.split('.').collect();
    matches!(parts.len(), 1 | 3) && parts.iter().all(|part| valid_identifier_part(part))
}

fn has_static_executor(executor: Option<&ZhixuExecutor>) -> bool {
    match executor {
        None => false,
        Some(executor) if executor.supplier_type == "zhixu" => true,
        Some(executor) => executor
            .supplier_id
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty()),
    }
}
