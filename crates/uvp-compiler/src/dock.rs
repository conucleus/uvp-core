//! Dock（静态 A -> B 时间窗订阅）文档的编译校验与 `uvp.dock.v1` 产物。
//! `signalMap` 值是 Hook DSL validWhen 表达式（Filter 语义，header source =
//! from.source）；atoms 的 `targetOrderRelation` 取信号在其所属 plan 里的关系
//! 值，当前编译器只有 0。目标 Zhixu 零感知 Dock，任何非 birth stage 均可作为
//! `to.stage`。

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

fn is_definition_uid(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 35
        && bytes.starts_with(b"zx-")
        && bytes[3..]
            .iter()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(b))
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
        if !is_definition_uid(&uid) {
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
    if !is_definition_uid(uid) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn source_zhixu() -> Value {
        json!({
            "apiVersion": "uvp/v0",
            "kind": "Zhixu",
            "metadata": { "name": "source_flow" },
            "spec": {
                "platform": { "type": "blockchain" },
                "nucleation": { "id": "core" },
                "stages": [
                    {
                        "name": "intake",
                        "source": "ops",
                        "sendSignals": [
                            { "name": "started", "trigger": true },
                            { "name": "passed" },
                            { "name": "rejected" }
                        ]
                    },
                    {
                        "name": "release",
                        "source": "ops",
                        "sendSignals": [
                            { "name": "cleared" }
                        ]
                    },
                    {
                        "name": "audit",
                        "source": "auditor",
                        "sendSignals": [
                            { "name": "flagged" }
                        ]
                    }
                ]
            }
        })
    }

    fn target_zhixu() -> Value {
        json!({
            "apiVersion": "uvp/v0",
            "kind": "Zhixu",
            "metadata": { "name": "target_flow" },
            "spec": {
                "platform": { "type": "blockchain" },
                "nucleation": { "id": "core" },
                "stages": [
                    {
                        "name": "birth_stage",
                        "source": "initiator",
                        "sendSignals": [
                            { "name": "created", "trigger": true }
                        ]
                    },
                    {
                        "name": "work_stage",
                        "source": "performer",
                        "executor": {
                            "supplierType": "organization"
                        },
                        "sendSignals": [
                            { "name": "start" },
                            { "name": "submit" },
                            { "name": "cancel" }
                        ],
                        "receiveSignals": {
                            "START": "initiator::birth_stage.created"
                        }
                    }
                ]
            }
        })
    }

    fn published_pair() -> (String, String, Value) {
        let from_def = source_zhixu();
        let to_def = target_zhixu();
        let from_uid = uvp_ir::derive_definition_uid(&from_def).unwrap();
        let to_uid = uvp_ir::derive_definition_uid(&to_def).unwrap();
        let published = json!([
            { "uid": from_uid, "definition": from_def },
            { "uid": to_uid, "definition": to_def }
        ]);
        (from_uid, to_uid, published)
    }

    #[test]
    fn compiles_dock_targeting_ordinary_non_birth_stage_without_dock_supplier_type() {
        let (from_uid, to_uid, published) = published_pair();
        let dock = json!({
            "apiVersion": "uvp/v0",
            "kind": "Dock",
            "metadata": { "name": "source-to-target" },
            "spec": {
                "from": { "zhixu": from_uid, "source": "ops" },
                "to": { "zhixu": to_uid, "stage": "work_stage" },
                "signalMap": {
                    "start": "intake.passed",
                    "submit": "(release.cleared & ~intake.rejected) | intake.passed+3600s",
                    "cancel": "intake.rejected"
                }
            }
        });

        let artifact = compile_dock_artifact(&dock, &published).expect("dock should compile");
        assert_eq!(artifact["schemaVersion"], DOCK_ARTIFACT_SCHEMA_VERSION);
        let dkid = artifact["dkid"].as_str().unwrap();
        assert!(dkid.starts_with("zd-") && dkid.len() == 35, "{dkid}");
        assert!(artifact["docHash"].as_str().unwrap().starts_with("0x"));
        assert_eq!(artifact["from"]["source"], "ops");
        assert_eq!(artifact["to"]["stage"], "work_stage");
        assert_eq!(artifact["to"]["source"], "performer");

        let outputs = artifact["signalMap"].as_array().unwrap();
        assert_eq!(outputs.len(), 3);
        let submit = outputs
            .iter()
            .find(|item| item["signal"] == "submit")
            .expect("submit output should exist");
        assert_eq!(submit["ast"]["condition"]["kind"], "or");
        assert_eq!(submit["ast"]["condition"]["terms"][0]["kind"], "and");
        assert_eq!(
            submit["ast"]["condition"]["terms"][0]["terms"][1]["kind"],
            "not"
        );
        assert_eq!(submit["ast"]["condition"]["terms"][1]["kind"], "delay");
        assert_eq!(
            submit["ast"]["condition"]["terms"][1]["durationSeconds"],
            3600
        );
        let atom_signals: BTreeSet<&str> = submit["atoms"]
            .as_array()
            .unwrap()
            .iter()
            .map(|atom| atom["signal"].as_str().unwrap())
            .collect();
        assert_eq!(
            atom_signals,
            BTreeSet::from(["cleared", "passed", "rejected"])
        );
        let dep_signals: BTreeSet<&str> = submit["dependencies"]
            .as_array()
            .unwrap()
            .iter()
            .map(|dep| dep["signalName"].as_str().unwrap())
            .collect();
        assert_eq!(
            dep_signals,
            BTreeSet::from(["intake.passed", "intake.rejected", "release.cleared"])
        );
    }

    #[test]
    fn rejects_birth_trigger_stage_as_dock_target_with_dk008() {
        let (from_uid, to_uid, published) = published_pair();
        let dock = json!({
            "apiVersion": "uvp/v0",
            "kind": "Dock",
            "metadata": { "name": "bad-birth-target" },
            "spec": {
                "from": { "zhixu": from_uid, "source": "ops" },
                "to": { "zhixu": to_uid, "stage": "birth_stage" },
                "signalMap": {
                    "created": "intake.passed"
                }
            }
        });

        let issues = compile_dock_artifact(&dock, &published).unwrap_err();
        assert!(
            issues.iter().any(|issue| issue.code == "DK008"),
            "expected DK008, got {issues:?}"
        );
    }

    #[test]
    fn rejects_missing_from_source_with_dk005_and_missing_to_stage_with_dk006() {
        let (from_uid, to_uid, published) = published_pair();
        let missing_source = json!({
            "apiVersion": "uvp/v0",
            "kind": "Dock",
            "metadata": { "name": "bad-source" },
            "spec": {
                "from": { "zhixu": from_uid, "source": "ghost_source" },
                "to": { "zhixu": to_uid, "stage": "work_stage" },
                "signalMap": {
                    "start": "intake.passed"
                }
            }
        });
        let source_issues = compile_dock_artifact(&missing_source, &published).unwrap_err();
        assert!(
            source_issues.iter().any(|issue| issue.code == "DK005"),
            "expected DK005, got {source_issues:?}"
        );

        let missing_stage = json!({
            "apiVersion": "uvp/v0",
            "kind": "Dock",
            "metadata": { "name": "bad-stage" },
            "spec": {
                "from": { "zhixu": from_uid, "source": "ops" },
                "to": { "zhixu": to_uid, "stage": "ghost_stage" },
                "signalMap": {
                    "start": "intake.passed"
                }
            }
        });
        let stage_issues = compile_dock_artifact(&missing_stage, &published).unwrap_err();
        assert!(
            stage_issues.iter().any(|issue| issue.code == "DK006"),
            "expected DK006, got {stage_issues:?}"
        );
    }

    #[test]
    fn rejects_undeclared_or_misowned_atoms_with_exact_dk_codes() {
        let (from_uid, to_uid, published) = published_pair();

        let undeclared_signal = json!({
            "apiVersion": "uvp/v0",
            "kind": "Dock",
            "metadata": { "name": "undeclared-atom" },
            "spec": {
                "from": { "zhixu": from_uid, "source": "ops" },
                "to": { "zhixu": to_uid, "stage": "work_stage" },
                "signalMap": {
                    "start": "intake.nonexistent"
                }
            }
        });
        let issues_16 = compile_dock_artifact(&undeclared_signal, &published).unwrap_err();
        assert!(
            issues_16.iter().any(|issue| issue.code == "DK016"),
            "expected DK016, got {issues_16:?}"
        );

        let wrong_source_atom = json!({
            "apiVersion": "uvp/v0",
            "kind": "Dock",
            "metadata": { "name": "wrong-source-atom" },
            "spec": {
                "from": { "zhixu": from_uid, "source": "ops" },
                "to": { "zhixu": to_uid, "stage": "work_stage" },
                "signalMap": {
                    "start": "audit.flagged"
                }
            }
        });
        let issues_15 = compile_dock_artifact(&wrong_source_atom, &published).unwrap_err();
        assert!(
            issues_15.iter().any(|issue| issue.code == "DK015"),
            "expected DK015, got {issues_15:?}"
        );

        let unknown_stage_atom = json!({
            "apiVersion": "uvp/v0",
            "kind": "Dock",
            "metadata": { "name": "unknown-stage-atom" },
            "spec": {
                "from": { "zhixu": from_uid, "source": "ops" },
                "to": { "zhixu": to_uid, "stage": "work_stage" },
                "signalMap": {
                    "start": "ghost.passed"
                }
            }
        });
        let issues_14 = compile_dock_artifact(&unknown_stage_atom, &published).unwrap_err();
        assert!(
            issues_14.iter().any(|issue| issue.code == "DK014"),
            "expected DK014, got {issues_14:?}"
        );
    }

    #[test]
    fn rejects_self_referencing_output_signal_within_same_zhixu_with_dk018() {
        let (_, to_uid, published) = published_pair();
        let self_ref = json!({
            "apiVersion": "uvp/v0",
            "kind": "Dock",
            "metadata": { "name": "self-ref" },
            "spec": {
                "from": { "zhixu": to_uid, "source": "performer" },
                "to": { "zhixu": to_uid, "stage": "work_stage" },
                "signalMap": {
                    "submit": "work_stage.submit"
                }
            }
        });
        let issues = compile_dock_artifact(&self_ref, &published).unwrap_err();
        assert!(
            issues.iter().any(|issue| issue.code == "DK018"),
            "expected DK018, got {issues:?}"
        );
    }
}
