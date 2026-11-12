//! Dock（静态 A -> B 时间窗订阅）文档的编译校验与 `uvp.dock.v1` 产物。
//! `signalMap` 值是 Hook DSL validWhen 表达式（Filter 语义，header source =
//! from.source）；atoms 的 `targetOrderRelation` 取信号在其所属 plan 里的关系
//! 值，当前编译器只有 0。

use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use uvp_hook_dsl::{parse_hook, Gate, ParseHookRequest, Profile};
use uvp_model::{DockDefinition, ZhixuDefinition, ZhixuStage};

use crate::{join_issues_bounded, CompilerError};

pub const DOCK_ARTIFACT_SCHEMA_VERSION: &str = "uvp.dock.v1";

const MAX_PUBLISHED_DEFINITIONS: usize = 256;
const MAX_SIGNAL_MAP_ENTRIES: usize = 64;
const MAX_EXPRESSION_ATOMS: usize = 64;
const MAX_OUTPUT_SIGNAL_NAME_BYTES: usize = 100;

#[derive(Debug, Clone)]
pub struct DockIssue {
    pub code: &'static str,
    pub path: String,
    pub message: String,
}

impl DockIssue {
    fn new(code: &'static str, path: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code,
            path: path.into(),
            message: message.into(),
        }
    }
}

impl std::fmt::Display for DockIssue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} {}: {}", self.code, self.path, self.message)
    }
}

pub type DockResult<T> = std::result::Result<T, Vec<DockIssue>>;

pub(crate) fn dock_error(issues: &[DockIssue]) -> CompilerError {
    let messages = issues
        .iter()
        .map(std::string::ToString::to_string)
        .collect::<Vec<_>>();
    CompilerError::Issues(join_issues_bounded(&messages))
}

pub fn compile_dock_artifact(dock_value: &Value, published_value: &Value) -> DockResult<Value> {
    let dock = parse_dock_document(dock_value)?;
    let published = parse_published(published_value)?;
    let from_source = dock.spec.from.source.trim();
    let from_zhixu_uid = dock.spec.from.zhixu.trim();
    let to_zhixu_uid = dock.spec.to.zhixu.trim();

    let mut issues = Vec::new();
    let from_definition = resolve_published(
        &published,
        from_zhixu_uid,
        "spec.from.zhixu",
        "DK003",
        &mut issues,
    );
    let to_definition = resolve_published(
        &published,
        to_zhixu_uid,
        "spec.to.zhixu",
        "DK004",
        &mut issues,
    );
    let (Some(from_definition), Some(to_definition)) = (from_definition, to_definition) else {
        return Err(issues);
    };

    if !from_definition
        .spec
        .stages
        .iter()
        .any(|stage| stage.source == from_source)
    {
        issues.push(DockIssue::new(
            "DK005",
            "spec.from.source",
            format!("source {from_source:?} is not declared by any stage of {from_zhixu_uid}"),
        ));
    }

    let to_stage_name = dock.spec.to.stage.trim();
    let Some(to_stage) = to_definition
        .spec
        .stages
        .iter()
        .find(|stage| stage.name == to_stage_name)
    else {
        issues.push(DockIssue::new(
            "DK006",
            "spec.to.stage",
            format!("stage {to_stage_name:?} does not exist in {to_zhixu_uid}"),
        ));
        return Err(issues);
    };
    if !is_dock_stage(to_stage) {
        issues.push(DockIssue::new(
            "DK007",
            "spec.to.stage",
            format!("stage {to_stage_name:?} must declare executor.supplierType \"dock\""),
        ));
    }
    if to_stage.has_trigger() {
        issues.push(DockIssue::new(
            "DK008",
            "spec.to.stage",
            format!("stage {to_stage_name:?} must not declare a non-empty trigger"),
        ));
    }

    let mut outputs = Vec::new();
    for (signal, expression) in &dock.spec.signal_map {
        match compile_output(
            &dock,
            from_definition,
            to_stage,
            signal,
            expression,
            from_zhixu_uid == to_zhixu_uid,
        ) {
            Ok(output) => outputs.push(output),
            Err(mut output_issues) => issues.append(&mut output_issues),
        }
    }
    if !issues.is_empty() {
        return Err(issues);
    }

    let identity = uvp_ir::derive_dock_identity(dock_value).map_err(|err| {
        vec![DockIssue::new(
            "DK001",
            "dock",
            format!("Dock document cannot be hashed: {err}"),
        )]
    })?;

    Ok(json!({
        "schemaVersion": DOCK_ARTIFACT_SCHEMA_VERSION,
        "dkid": identity.dkid,
        "docHash": identity.doc_hash,
        "from": {
            "zhixu": from_zhixu_uid,
            "source": from_source,
            "sourceId": uvp_ir::keccak256_utf8_hex(from_source),
        },
        "to": {
            "zhixu": to_zhixu_uid,
            "stage": to_stage.name,
            "stageId": uvp_ir::keccak256_utf8_hex(&to_stage.name),
            "source": to_stage.source,
            "sourceId": uvp_ir::keccak256_utf8_hex(&to_stage.source),
        },
        "signalMap": outputs,
    }))
}

fn is_dock_stage(stage: &ZhixuStage) -> bool {
    stage
        .executor
        .as_ref()
        .is_some_and(|executor| executor.supplier_type == "dock")
}

fn parse_dock_document(value: &Value) -> DockResult<DockDefinition> {
    let dock: DockDefinition = serde_json::from_value(value.clone()).map_err(|err| {
        vec![DockIssue::new(
            "DK001",
            "dock",
            format!("Dock document is invalid: {err}"),
        )]
    })?;
    let mut issues = Vec::new();
    if dock.api_version != "uvp/v0" {
        issues.push(DockIssue::new(
            "DK001",
            "apiVersion",
            "apiVersion must be uvp/v0",
        ));
    }
    if dock.kind != "Dock" {
        issues.push(DockIssue::new("DK001", "kind", "kind must be Dock"));
    }
    if dock.metadata.name.trim().is_empty() {
        issues.push(DockIssue::new(
            "DK001",
            "metadata.name",
            "metadata.name must be non-empty",
        ));
    } else if !crate::validate::is_name_slug(&dock.metadata.name) {
        issues.push(DockIssue::new(
            "DK001",
            "metadata.name",
            "metadata.name must start with an ASCII lowercase letter and contain only ASCII lowercase letters, digits, '_' or '-'",
        ));
    }
    if dock.spec.from.source.trim().is_empty() {
        issues.push(DockIssue::new(
            "DK005",
            "spec.from.source",
            "from.source must be non-empty",
        ));
    }
    if dock.spec.to.stage.trim().is_empty() {
        issues.push(DockIssue::new(
            "DK006",
            "spec.to.stage",
            "to.stage must be non-empty",
        ));
    }
    if dock.spec.signal_map.is_empty() {
        issues.push(DockIssue::new(
            "DK009",
            "spec.signalMap",
            "signalMap must declare at least one output signal",
        ));
    } else if dock.spec.signal_map.len() > MAX_SIGNAL_MAP_ENTRIES {
        issues.push(DockIssue::new(
            "DK017",
            "spec.signalMap",
            format!(
                "signalMap declares {} outputs, limit is {MAX_SIGNAL_MAP_ENTRIES}",
                dock.spec.signal_map.len()
            ),
        ));
    }
    if issues.is_empty() {
        Ok(dock)
    } else {
        Err(issues)
    }
}

fn parse_published(value: &Value) -> DockResult<BTreeMap<String, ZhixuDefinition>> {
    let entries = value.as_array().ok_or_else(|| {
        vec![DockIssue::new(
            "DK002",
            "published",
            "published must be an array of {uid, definition} entries",
        )]
    })?;
    if entries.len() > MAX_PUBLISHED_DEFINITIONS {
        return Err(vec![DockIssue::new(
            "DK002",
            "published",
            format!(
                "published carries {} definitions, limit is {MAX_PUBLISHED_DEFINITIONS}",
                entries.len()
            ),
        )]);
    }
    let mut issues = Vec::new();
    let mut published = BTreeMap::new();
    for (index, entry) in entries.iter().enumerate() {
        let path = format!("published[{index}]");
        let Some(entry_object) = entry.as_object() else {
            issues.push(DockIssue::new(
                "DK002",
                &path,
                "published entry must be an object {uid, definition}",
            ));
            continue;
        };
        for key in entry_object.keys() {
            if !matches!(key.as_str(), "uid" | "definition") {
                issues.push(DockIssue::new(
                    "DK002",
                    format!("{path}.{key}"),
                    format!("unknown field {key:?}; allowed: [\"uid\", \"definition\"]"),
                ));
            }
        }
        let uid = entry
            .get("uid")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_string();
        if !crate::validate::is_definition_uid(&uid) {
            issues.push(DockIssue::new(
                "DK002",
                format!("{path}.uid"),
                format!(
                    "uid must be the published definition uid matching ^zx-[0-9a-f]{{32}}$, found {uid:?}"
                ),
            ));
            continue;
        }
        let Some(definition_value) = entry.get("definition") else {
            issues.push(DockIssue::new(
                "DK002",
                format!("{path}.definition"),
                "definition is required",
            ));
            continue;
        };
        let definition: ZhixuDefinition = match serde_json::from_value(definition_value.clone()) {
            Ok(definition) => definition,
            Err(err) => {
                issues.push(DockIssue::new(
                    "DK002",
                    format!("{path}.definition"),
                    format!("published definition is not a valid Zhixu document: {err}"),
                ));
                continue;
            }
        };
        match uvp_ir::derive_definition_uid(definition_value) {
            Ok(derived) if derived == uid => {}
            Ok(derived) => issues.push(DockIssue::new(
                "DK002",
                &path,
                format!("definition content hashes to {derived}, but the published uid is {uid:?}"),
            )),
            Err(err) => issues.push(DockIssue::new(
                "DK002",
                &path,
                format!("definition content cannot be hashed: {err}"),
            )),
        }
        if published.insert(uid.clone(), definition).is_some() {
            issues.push(DockIssue::new(
                "DK002",
                &path,
                format!("duplicate published definition uid {uid:?}"),
            ));
        }
    }
    if issues.is_empty() {
        Ok(published)
    } else {
        Err(issues)
    }
}

fn resolve_published<'a>(
    published: &'a BTreeMap<String, ZhixuDefinition>,
    uid: &str,
    path: &str,
    code: &'static str,
    issues: &mut Vec<DockIssue>,
) -> Option<&'a ZhixuDefinition> {
    if !crate::validate::is_definition_uid(uid) {
        issues.push(DockIssue::new(
            code,
            path,
            format!(
                "must be a published definition uid matching ^zx-[0-9a-f]{{32}}$, found {uid:?}"
            ),
        ));
        return None;
    }
    match published.get(uid) {
        Some(definition) => Some(definition),
        None => {
            issues.push(DockIssue::new(
                code,
                path,
                format!("no published definition with uid {uid:?}"),
            ));
            None
        }
    }
}

fn compile_output(
    dock: &DockDefinition,
    from_definition: &ZhixuDefinition,
    to_stage: &ZhixuStage,
    signal: &str,
    raw_expression: &str,
    same_zhixu: bool,
) -> DockResult<Value> {
    let base_path = format!("spec.signalMap.{signal}");
    if signal.trim().is_empty()
        || signal.contains('.')
        || signal.contains('#')
        || signal.chars().any(char::is_whitespace)
        || signal.len() > MAX_OUTPUT_SIGNAL_NAME_BYTES
    {
        return Err(vec![DockIssue::new(
            "DK010",
            &base_path,
            "output signal name must be 1-100 bytes and must not contain '.', '#' or whitespace",
        )]);
    }
    let output_signal = format!("{}.{signal}", to_stage.name);
    if !crate::validate::declares_signal_expanding_to(
        &to_stage.send_signals,
        &to_stage.name,
        &output_signal,
    ) {
        return Err(vec![DockIssue::new(
            "DK011",
            &base_path,
            format!(
                "output signal is not declared in {}.sendSignals",
                to_stage.name
            ),
        )]);
    }

    let expression = raw_expression.trim();
    if expression.is_empty() {
        return Err(vec![DockIssue::new(
            "DK012",
            &base_path,
            "validWhen expression must be non-empty",
        )]);
    }
    let hook = format!("{}::{expression}", dock.spec.from.source.trim());
    let parsed = match parse_hook(ParseHookRequest {
        profile: Profile::EvmStrict,
        gate: Gate::Filter,
        hook_name: "DOCK".to_string(),
        hook,
    }) {
        Ok(parsed) => parsed,
        Err(err) => {
            return Err(vec![DockIssue::new(
                "DK012",
                &base_path,
                format!("validWhen expression is invalid: {err}"),
            )])
        }
    };

    let mut atom_names = BTreeSet::new();
    for dependency in &parsed.dependencies {
        atom_names.insert(dependency.signal_name.clone());
    }
    if atom_names.len() > MAX_EXPRESSION_ATOMS {
        return Err(vec![DockIssue::new(
            "DK017",
            &base_path,
            format!(
                "validWhen expression references {} atoms, limit is {MAX_EXPRESSION_ATOMS}",
                atom_names.len()
            ),
        )]);
    }

    let from_source = dock.spec.from.source.trim();
    let mut issues = Vec::new();
    let mut atoms = Vec::with_capacity(atom_names.len());
    for atom in &atom_names {
        if same_zhixu && atom == &output_signal {
            issues.push(DockIssue::new(
                "DK018",
                &base_path,
                format!("validWhen must not reference the emitting signal {output_signal} itself"),
            ));
            continue;
        }
        let Some((stage_name, signal_name)) = atom.split_once('.') else {
            issues.push(DockIssue::new(
                "DK014",
                &base_path,
                format!("atom {atom:?} must be <stage>.<signal>"),
            ));
            continue;
        };
        let Some(stage) = from_definition
            .spec
            .stages
            .iter()
            .find(|stage| stage.name == stage_name)
        else {
            issues.push(DockIssue::new(
                "DK014",
                &base_path,
                format!("atom {atom:?} references a stage that does not exist in from.zhixu"),
            ));
            continue;
        };
        if stage.source != from_source {
            issues.push(DockIssue::new(
                "DK015",
                &base_path,
                format!(
                    "atom {atom:?} belongs to source {:?}, but from.source is {from_source:?}",
                    stage.source
                ),
            ));
            continue;
        }
        if !crate::validate::declares_signal_expanding_to(&stage.send_signals, &stage.name, atom) {
            issues.push(DockIssue::new(
                "DK016",
                &base_path,
                format!("atom {atom:?} is not declared in {stage_name}.sendSignals"),
            ));
            continue;
        }
        atoms.push(json!({
            "source": from_source,
            "sourceId": uvp_ir::keccak256_utf8_hex(from_source),
            "stage": stage_name,
            "stageId": uvp_ir::keccak256_utf8_hex(stage_name),
            "signal": signal_name,
            "signalId": uvp_ir::keccak256_utf8_hex(atom),
            "targetOrderRelation": 0,
        }));
    }
    if !issues.is_empty() {
        return Err(issues);
    }

    Ok(json!({
        "signal": signal,
        "signalId": uvp_ir::keccak256_utf8_hex(&output_signal),
        "sourceId": uvp_ir::keccak256_utf8_hex(&to_stage.source),
        "stageId": uvp_ir::keccak256_utf8_hex(&to_stage.name),
        "targetOrderRelation": 0,
        "rawExpression": expression,
        "ast": parsed.ast,
        "dependencies": serde_json::to_value(&parsed.dependencies)
            .map_err(|err| vec![DockIssue::new("DK001", &base_path, err.to_string())])?,
        "atoms": atoms,
    }))
}
