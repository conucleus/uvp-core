use serde::Deserialize;
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use thiserror::Error;
use uvp_hook_dsl::{
    parse_hook, Compatibility, DependencyKind, ParseHookOutput, ParseHookRequest, Profile,
};
use uvp_ir::hash_canonical;
use uvp_model::{ZhixuDefinition, ZhixuExecutor, ZhixuStage};

pub mod dock;

const COMPILER_NAME: &str = "uvp-eth-compiler";
const COMPILER_VERSION: &str = "0.1.0";
const HOOK_PLAN_SCHEMA_VERSION: &str = "uvp.hookPlan.v2";
/// cloud 编译产物的信封版本：Go 侧 pkg/version.CloudArtifactSchema 镜像此值，
/// parity 测试按 `pub const` 声明逐字比对，必须保持 pub。
pub const CLOUD_ARTIFACT_SCHEMA_VERSION: &str = "uvp.cloudArtifact.v2";

#[derive(Debug, Error)]
pub enum CompilerError {
    #[error("{0}")]
    Message(String),
    #[error("compilation failed: {0}")]
    Issues(String),
}

type Result<T> = std::result::Result<T, CompilerError>;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompileRequest {
    #[serde(default = "default_target")]
    pub target: String,
    #[serde(alias = "zhixu")]
    pub definition: Value,
    /// Dock resolution manifest（PRD94 §5.2）：由 Store/发布系统或离线
    /// lock 文件提供；含 zhixu executor 的可运行编译必须提供，否则返回
    /// `UNRESOLVED_DOCK_TARGET`。
    #[serde(default)]
    pub resolution_manifest: Option<Value>,
}

fn default_target() -> String {
    "hook_plan".to_string()
}

pub fn compile_json(input: &str) -> String {
    let result = serde_json::from_str::<CompileRequest>(input)
        .map_err(|err| CompilerError::Message(format!("invalid compile request: {err}")))
        .and_then(|req| compile_request(&req));
    envelope_json(result)
}

pub fn compile_request(req: &CompileRequest) -> Result<Value> {
    let manifest = req.resolution_manifest.as_ref();
    match req.target.as_str() {
        "hook_plan" | "evm" => compile_zhixu_hook_plan(&req.definition, manifest, false),
        "cloud" | "cloud_db" => compile_cloud_artifact(&req.definition, manifest, false),
        // parse-only：允许 unresolved route（PRD94 §5.1）。
        "parse" => compile_zhixu_hook_plan(&req.definition, manifest, true),
        "dock_link" => compile_dock_link(&req.definition, manifest),
        other => Err(CompilerError::Message(format!(
            "unsupported compile target {other:?}"
        ))),
    }
}

fn issues_from_dock(issues: &[dock::DockIssue]) -> CompilerError {
    CompilerError::Issues(
        issues
            .iter()
            .map(std::string::ToString::to_string)
            .collect::<Vec<_>>()
            .join("; "),
    )
}

/// 定义身份：dock 场景必须使用 `metadata.uid`（PRD94 §7.3）。
fn definition_uid(definition: &ZhixuDefinition, requires_uid: bool) -> Result<String> {
    match &definition.metadata.uid {
        Some(uid) if !uid.trim().is_empty() => Ok(uid.trim().to_string()),
        _ if requires_uid => Err(CompilerError::Message(
            "metadata.uid is required for definitions that publish a dockInterface or delegate to a zhixu executor"
                .to_string(),
        )),
        _ => Ok(definition.metadata.name.clone()),
    }
}

fn definition_version(definition: &ZhixuDefinition) -> Result<String> {
    definition
        .metadata
        .annotations
        .get("version")
        .map(String::as_str)
        .filter(|version| !version.is_empty())
        .map(str::to_string)
        .ok_or_else(|| {
            CompilerError::Message(
                "metadata.annotations.version is required: missing or empty version".to_string(),
            )
        })
}

/// Compute the stable plan identity shared by hook-plan and cloud artifacts.
/// Dock routes carry this value as their local plan namespace so that two
/// otherwise identical orders from different plan revisions cannot alias.
fn plan_id(
    definition: &ZhixuDefinition,
    platform: &Value,
    version: &str,
    zhixu_id: &str,
) -> Result<String> {
    hash_canonical(
        "uvp:hook-plan-id:v1",
        &json!({
            "compiler": { "name": COMPILER_NAME, "version": COMPILER_VERSION },
            "platform": platform,
            "version": version,
            "zhixuId": zhixu_id,
            "zhixuName": definition.metadata.name,
        }),
    )
    .map_err(|err| CompilerError::Message(err.to_string()))
}

/// Add the local plan namespace to every resolved route.  This is metadata
/// only: route hashes/roots intentionally remain unchanged, while runtime
/// dockInstanceId derivation consumes the plan id explicitly.
fn with_local_plan_id(mut routes: Vec<Value>, local_plan_id: &str) -> Vec<Value> {
    for route in &mut routes {
        if let Some(local) = route.get_mut("local").and_then(Value::as_object_mut) {
            local.insert(
                "planId".to_string(),
                Value::String(local_plan_id.to_string()),
            );
        }
    }
    routes
}

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
        DockProfile::Evm,
    )?;

    let mut validation_issues = Vec::new();
    validation_issues.extend(validate_stage_executors(
        &stage_entries,
        &selected_stage_bindings,
    ));
    // 阶段物化三线统一（簇 A）：onchain 目标上阶段只能由本阶段 hook Ready
    // 物化（order-trigger 或 EMIT_READY）。无静态 executor、仅被
    // selectedStages 覆盖的阶段其 receive hook 一律 emit_ready=false、
    // flags=0——纯 watcher 不物化阶段，链上 `_evaluateHook` 对未物化阶段
    // 直接 revert UnknownHook，且没有任何恢复路径（executor patch 不物化）。
    // 该形态在编译期整体拒绝：这类阶段不得声明 receiveSignals。
    validation_issues.extend(validate_onchain_stage_materialization(&stage_entries));
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

    let zhixu_id = definition_uid(&definition, dock_state.requires_uid)?;
    let version = definition_version(&definition)?;
    let platform = normalize_platform_value(&definition.spec.platform)?;

    let mut compiled_hooks = Vec::new();
    for entry in &stage_entries {
        compiled_hooks.extend(compile_stage_hooks(entry, &dock_state)?);
    }
    let dependency_index = build_dependency_index(&compiled_hooks);
    let signal_capabilities = build_signal_capabilities(&stage_entries)?;
    let executor_routes = build_executor_routes(&stage_entries);
    let plan_id = plan_id(&definition, &platform, &version, &zhixu_id)?;
    let dock_routes = with_local_plan_id(dock_state.routes_json.clone(), &plan_id);

    let payload = json!({
        "schemaVersion": HOOK_PLAN_SCHEMA_VERSION,
        "planId": plan_id,
        "zhixuId": zhixu_id,
        "version": version,
        "zhixuName": definition.metadata.name,
        "platform": platform,
        "compiledHooks": compiled_hooks,
        "dependencyIndex": dependency_index,
        "executorRoutes": executor_routes,
        "dockInterface": dock_state.interface_json,
        "dockRoutes": dock_routes,
        "dockRoutesRoot": dock::word_hex(&dock_state.dock_routes_root),
        "dockInterfaceRoot": dock::word_hex(&dock_state.dock_interface_root),
        "selectedStageBindings": selected_stage_bindings,
        "signalCapabilities": signal_capabilities,
        "source": uvp_ir::canonicalize(definition_value).map_err(|err| CompilerError::Message(err.to_string()))?,
    });
    let plan_hash = hash_canonical("uvp:hook-plan-artifact:v1", &payload)
        .map_err(|err| CompilerError::Message(err.to_string()))?;

    Ok(json!({
        "schemaVersion": HOOK_PLAN_SCHEMA_VERSION,
        "planId": payload["planId"].clone(),
        "zhixuId": payload["zhixuId"].clone(),
        "version": payload["version"].clone(),
        "zhixuName": payload["zhixuName"].clone(),
        "platform": payload["platform"].clone(),
        "compiledHooks": payload["compiledHooks"].clone(),
        "dependencyIndex": payload["dependencyIndex"].clone(),
        "executorRoutes": payload["executorRoutes"].clone(),
        "dockInterface": payload["dockInterface"].clone(),
        "dockRoutes": payload["dockRoutes"].clone(),
        "dockRoutesRoot": payload["dockRoutesRoot"].clone(),
        "dockInterfaceRoot": payload["dockInterfaceRoot"].clone(),
        "selectedStageBindings": payload["selectedStageBindings"].clone(),
        "signalCapabilities": payload["signalCapabilities"].clone(),
        "planHash": plan_hash,
    }))
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
        DockProfile::Cloud,
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
    if !validation_issues.is_empty() {
        return Err(CompilerError::Issues(validation_issues.join("; ")));
    }
    let zhixu_id = definition_uid(&definition, dock_state.requires_uid)?;
    let version = definition_version(&definition)?;
    let platform = normalize_platform_value(&definition.spec.platform)?;
    let plan_id = plan_id(&definition, &platform, &version, &zhixu_id)?;
    let dock_routes = with_local_plan_id(dock_state.routes_json.clone(), &plan_id);
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

    Ok(json!({
        "schemaVersion": CLOUD_ARTIFACT_SCHEMA_VERSION,
        "planId": plan_id,
        "zhixuId": zhixu_id,
        "version": version,
        "zhixuName": definition.metadata.name,
        "platform": platform,
        "stages": stages,
        "hooks": hooks,
        "orderStageDefaults": stages,
        "dockInterface": dock_state.interface_json,
        "dockRoutes": dock_routes,
        "dockRoutesRoot": dock::word_hex(&dock_state.dock_routes_root),
        "dockInterfaceRoot": dock::word_hex(&dock_state.dock_interface_root),
    }))
}

/// dock_link：只做 link，输出已解析 route 与根（PRD96 §4.3 API 边界）。
fn compile_dock_link(
    definition_value: &Value,
    resolution_manifest: Option<&Value>,
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
    let dock_state = compile_dock_state(
        &definition,
        &stage_pairs,
        resolution_manifest,
        false,
        DockProfile::Evm,
    )?;
    Ok(json!({
        "schemaVersion": "uvp.dockLink.v1",
        "dockInterface": dock_state.interface_json,
        "dockRoutes": dock_state.routes_json,
        "dockRoutesRoot": dock::word_hex(&dock_state.dock_routes_root),
        "dockInterfaceRoot": dock::word_hex(&dock_state.dock_interface_root),
    }))
}

/// D018（PRD94 §9）：可运行产物必须能解析 runtime target identity——
/// EVM 轨要求 manifest 提供 target evmPlanId，Cloud 轨要求 cloudArtifactId。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DockProfile {
    Evm,
    Cloud,
}

/// 一次编译中的 dock 状态：接口产物、未链接/已链接 route、hook 标记输入。
struct DockState {
    interface_json: Option<Value>,
    routes_json: Vec<Value>,
    dock_routes_root: dock::Word,
    dock_interface_root: dock::Word,
    /// dockInterface input port 引用的本地 hook（`<task>.<stage>#<hook>`），
    /// 这些 mailbox hook 不走普通依赖引用校验（PRD94 §3.3）。
    input_port_hook_ids: BTreeSet<String>,
    requires_uid: bool,
}

fn compile_dock_state(
    definition: &ZhixuDefinition,
    stage_pairs: &[(String, ZhixuStage)],
    resolution_manifest: Option<&Value>,
    allow_unresolved: bool,
    profile: DockProfile,
) -> Result<DockState> {
    let version = definition_version(definition)?;
    let unlinked =
        dock::collect_unlinked_routes(stage_pairs).map_err(|issues| issues_from_dock(&issues))?;
    let requires_uid = definition.spec.dock_interface.is_some() || !unlinked.is_empty();

    let interface_artifact = match &definition.spec.dock_interface {
        Some(dock_interface) => Some(
            dock::compile_dock_interface(
                dock_interface,
                &definition_uid(definition, true)?,
                &version,
                stage_pairs,
            )
            .map_err(|issues| issues_from_dock(&issues))?,
        ),
        None => None,
    };
    let interface_root = interface_artifact
        .as_ref()
        .map(|artifact| artifact.interface_root)
        .unwrap_or(dock::EMPTY_MERKLE_ROOT);
    let input_port_hook_ids = interface_artifact
        .as_ref()
        .map(|artifact| {
            artifact
                .inputs
                .iter()
                .map(|port| port.hook_id.clone())
                .collect::<BTreeSet<_>>()
        })
        .unwrap_or_default();

    let routes = if unlinked.is_empty() {
        Vec::new()
    } else {
        match resolution_manifest {
            Some(manifest_value) => {
                let manifest = dock::parse_resolution_manifest(manifest_value)
                    .map_err(|issues| issues_from_dock(&issues))?;
                let identity = dock::LocalLinkIdentity {
                    uid: definition_uid(definition, true)?,
                    version,
                };
                dock::link_dock_routes(&identity, stage_pairs, &unlinked, &manifest)
                    .map_err(|issues| issues_from_dock(&issues))?
            }
            None if allow_unresolved => Vec::new(),
            None => {
                return Err(CompilerError::Message(
                    "UNRESOLVED_DOCK_TARGET: definition contains zhixu executor routes but no resolutionManifest was provided; runnable compilation requires linking against published target interfaces (PRD94 §5)".to_string(),
                ));
            }
        }
    };
    // D018：runtime target identity 必须完整可解析（profile 编译硬门槛）。
    if !allow_unresolved {
        for route in &routes {
            let resolvable = match profile {
                DockProfile::Evm => route.target_evm_plan_id.is_some(),
                DockProfile::Cloud => route.target_cloud_artifact_id.is_some(),
            };
            if !resolvable {
                return Err(CompilerError::Message(format!(
                    "D018 {}.executor.zhixuExecutorConfig: resolved target {}@{} has no {} in the resolution manifest; the {} profile cannot resolve the runtime target identity",
                    route.stage_identifier,
                    route.target_zhixu_uid,
                    route.target_version,
                    match profile {
                        DockProfile::Evm => "evmPlanId",
                        DockProfile::Cloud => "cloudArtifactId",
                    },
                    match profile {
                        DockProfile::Evm => "evm",
                        DockProfile::Cloud => "cloud",
                    }
                )));
            }
        }
    }
    let routes_json = routes
        .iter()
        .map(|route| route.to_json())
        .collect::<Vec<_>>();
    Ok(DockState {
        interface_json: interface_artifact
            .as_ref()
            .map(|artifact| artifact.to_json()),
        routes_json,
        dock_routes_root: dock::dock_routes_root(&routes),
        dock_interface_root: interface_root,
        input_port_hook_ids,
        requires_uid,
    })
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct Envelope {
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    value: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    diagnostics: Option<Vec<Diagnostic>>,
}

#[derive(Debug, serde::Serialize)]
struct Diagnostic {
    message: String,
}

fn envelope_json(result: Result<Value>) -> String {
    let envelope = match result {
        Ok(value) => Envelope {
            ok: true,
            value: Some(value),
            diagnostics: None,
        },
        Err(err) => Envelope {
            ok: false,
            value: None,
            diagnostics: Some(vec![Diagnostic {
                message: err.to_string(),
            }]),
        },
    };
    serde_json::to_string(&envelope).expect("compile envelope should serialize")
}

#[derive(Debug, Clone)]
struct StageEntry {
    stage: ZhixuStage,
    stage_identifier: String,
}

/// 全局 stage.source 上限：source 既是路由键也是 sourceId=keccak(source)
/// 的哈希输入，落库列（订阅路由维度）宽 36——与订阅 target source 同值
/// （Go 镜像 zhixu_schema.go 的 ≤36 同款）。
const MAX_STAGE_SOURCE_BYTES: usize = 36;
/// DDL 维度镜像：global_zhixu.uid VARCHAR(64)。
const MAX_UID_BYTES: usize = 64;
/// DDL 维度镜像：global_zhixu.name / global_stage.stage_identifier
/// VARCHAR(100)。
const MAX_IDENTIFIER_BYTES: usize = 100;
/// DDL 维度镜像：canonical 三段式 task.stage.signal 落
/// individual_record.signal_name / hook_dependency.signal_name VARCHAR(100)。
const MAX_SIGNAL_NAME_BYTES: usize = 100;

fn validate_zhixu_shape(definition: &ZhixuDefinition) -> Vec<String> {
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
            "metadata.name {:?} exceeds {MAX_IDENTIFIER_BYTES} bytes (global_zhixu.name)",
            definition.metadata.name
        ));
    }
    if let Some(uid) = &definition.metadata.uid {
        let trimmed = uid.trim();
        if !trimmed.is_empty() && trimmed.len() > MAX_UID_BYTES {
            issues.push(format!(
                "metadata.uid {:?} exceeds {MAX_UID_BYTES} bytes (global_zhixu.uid)",
                trimmed
            ));
        }
    }
    if definition.spec.platform.platform_type.trim().is_empty() {
        issues.push("spec.platform must be an object with a non-empty type".to_string());
    }
    if definition.spec.task_patterns.is_empty() {
        issues.push("spec.taskPatterns must contain at least one task pattern".to_string());
    }
    for (task_index, task) in definition.spec.task_patterns.iter().enumerate() {
        if !valid_identifier_part(&task.name) {
            issues.push(format!(
                "spec.taskPatterns[{task_index}].name must start with an ASCII letter and contain only ASCII letters, digits, '_' or '-': {}",
                task.name
            ));
        }
        for (stage_index, stage) in task.stages.iter().enumerate() {
            if !valid_identifier_part(&stage.name) {
                issues.push(format!(
                    "spec.taskPatterns[{task_index}].stages[{stage_index}].name must start with an ASCII letter and contain only ASCII letters, digits, '_' or '-': {}",
                    stage.name
                ));
            }
            // stage.source：非空、plain identifier 字符集、≤36（与订阅
            // target source 同值）。空串会以空键混进 mintedSources；含
            // 空格/Unicode 的 source 是路由键与 keccak 输入，两侧必须逐字节
            // 一致（Go 镜像 zhixu_schema.go 同款）。
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
            // sendSignals 组合维度（individual_record.signal_name）：stage
            // 标识符本身合法不等于组合合法，超限在编译期报确定性错误而不是
            // 落库时 value too long（Go 镜像 validateDDLDimensions 同款）。
            for signal in &stage.send_signals {
                if stage_identifier.len() + 1 + signal.len() > MAX_SIGNAL_NAME_BYTES {
                    issues.push(format!(
                        "spec.taskPatterns[{task_index}].stages[{stage_index}] ({stage_identifier:?}) sendSignal {signal:?} exceeds {MAX_SIGNAL_NAME_BYTES} bytes combined (individual_record.signal_name)"
                    ));
                }
            }
        }
    }
    issues
}

/// source 类字符集：与 uvp-hook-dsl 的 is_plain_identifier 同规则
/// （非空，仅 ASCII 字母/数字/下划线/中划线）。
fn is_plain_source_identifier(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
}

fn valid_identifier_part(value: &str) -> bool {
    let mut chars = value.chars();
    matches!(chars.next(), Some(ch) if ch.is_ascii_alphabetic())
        && chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
}

fn flatten_stages(definition: &ZhixuDefinition) -> Result<Vec<StageEntry>> {
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

fn normalize_platform_value(platform: &uvp_model::ZhixuPlatform) -> Result<Value> {
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

fn build_selected_stage_bindings(
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

fn build_executor_routes(entries: &[StageEntry]) -> Value {
    let mut routes = Map::new();
    for entry in entries {
        if is_zhixu_executor_stage(entry) {
            // zhixu 委托 route 不进静态 executor route：权威形态是
            // dockRoutes 中的 resolved DockRouteV1（PRD94 §7.1）。
            continue;
        }
        if entry.stage.executor.is_some() {
            routes.insert(entry.stage_identifier.clone(), route_for_stage(entry));
        }
    }
    Value::Object(routes)
}

fn is_zhixu_executor_stage(entry: &StageEntry) -> bool {
    entry
        .stage
        .executor
        .as_ref()
        .is_some_and(|executor| executor.supplier_type.trim() == "zhixu")
}

fn validate_stage_executors(entries: &[StageEntry], bindings: &[Value]) -> Vec<String> {
    let mut issues = Vec::new();
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
        // 模-1 同族裁决：订阅阶段的投递目标编译期定死、
        // 运行时禁止 executor patch。selectedStages 可达只对可 patch 的普通
        // 阶段构成绑定——订阅阶段被 selector 指到也不豁免，否则定义可编译
        // 却没有 executor route、又禁补绑，永远无法形成可执行静态绑定。
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

// stage_is_subscription 报告阶段是否声明了 ANCHOR 订阅入口。解析失败的
// hook 不算订阅形态：语法错误由引用存在性校验统一上报。
fn stage_is_subscription(stage: &ZhixuStage) -> bool {
    stage.receive_signals.values().any(|raw| {
        parse_hook_for_compiler("HOOK", raw)
            .map(|parsed| parsed.mode == uvp_hook_dsl::HookMode::Subscription)
            .unwrap_or(false)
    })
}

/// 阶段物化三线统一（簇 A，onchain 目标）：无静态 executor（仅被
/// selectedStages 覆盖）的阶段不得声明任何 receiveSignals。这类 hook 编译
/// 为 flags=0 纯 watcher（emit_ready=false、非 order-trigger），而链上阶段
/// 只能由本阶段 order-trigger / EMIT_READY hook Ready 物化——纯 watcher 不
/// 物化，executor patch 也不物化（UVPStateMachine activateStageExecutor
/// 不调用 _materializeStage），形态一旦上链即死锁且无恢复路径。
fn validate_onchain_stage_materialization(entries: &[StageEntry]) -> Vec<String> {
    let mut issues = Vec::new();
    for entry in entries {
        if has_static_executor(entry.stage.executor.as_ref()) {
            continue;
        }
        for hook_name in entry.stage.receive_signals.keys() {
            issues.push(format!(
                "{}.receiveSignals.{}: stage has no static executor and is only reachable through selectedStages; its hooks compile to flags=0 watchers which can never materialize the stage on-chain (deadlock, no recovery path) — declare a static executor or drop receiveSignals from this stage",
                entry.stage_identifier, hook_name
            ));
        }
    }
    issues
}

/// UVP-01（模-1 同族裁决，对齐 Go 镜像 zhixu_schema.go 的同款检查）：zhixu
/// 委托执行器的信封恒为 NewSource=false 的订单锚定子信号，无法携带通道
/// 事实身份。本域 source 类无 mint 声明时订阅注入 route=fanin、投递落通道
/// 维度（order_id=''），委托信封缺 order_id 会被状态机按永久错误拒绝——
/// "编译放行、运行必死"的组合在编译期关闭；有锚阶段（本类存在 mint 声明，
/// route=order 按单投递）不受此限。mint 出生阶段与委托的组合由
/// validate_mint_anchors 单独拒绝。
fn validate_subscription_delegation(entries: &[StageEntry]) -> Vec<String> {
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
                // 语法错误由引用存在性校验统一上报。
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

fn validate_mint_anchors(entries: &[StageEntry]) -> Vec<String> {
    let mut issues = Vec::new();
    // 编译期固定五件事（模-1/模-2 裁决）：
    // 1) mint 取值合法（当前仅 per-fact）；
    // 2) mint 阶段必须编译期静态绑定非委托执行者（运行时 patch 对出生阶段
    //    一律拒绝）；
    // 3) 出生入口只能是 ANCHOR 订阅（跨类事实携带溯源进入；普通 hook
    //    在铸单前没有可求值的订单上下文）；
    // 4) 防无界代铸链：mint 阶段的订阅目标不得指向本阶段自己的 source 类；
    // 5) 防跨源代铸环：全部 mint 阶段的订阅目标 source 类构成的有向图
    //    不得存在可达环。委托对译边由 dock v1 的 route 启动图环检测
    //    （D015）覆盖：本地编译不持有目标接口，无法可靠对译远端类。
    for entry in entries {
        if let Some(mint) = &entry.stage.mint {
            // 与 Go 侧口径一致：精确比较，不接受带空白的变体。
            if mint != "per-fact" {
                issues.push(format!(
                    "{}.mint only supports per-fact: {}",
                    entry.stage_identifier, mint
                ));
            }
            // 模-1 裁决：mint 出生阶段必须编译期静态绑定非委托执行者。
            // 运行时 patch 对订阅/出生阶段一律拒绝，没有静态执行者的出生
            // 阶段是"出生即死"的代铸死锁。
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
                    .map(|executor| executor.supplier_type.trim().to_string())
                    .unwrap_or_default();
                if executor_type == "zhixu" {
                    // mint 出生 + 委托执行器：出生（代铸）与委托（dock 子订单）
                    // 是两种互斥的订单创建路径——组合直接拒绝。
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
                    Err(_) => {} // 语法错误由引用存在性校验统一上报
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
                        // 模-2 裁决：出生入口只能是 ANCHOR 订阅——出生事实
                        // 一律走订阅通道携带溯源进入。
                        issues.push(format!(
                            "{}.receiveSignals.{hook_name}: mint stage accepts ANCHOR(@…) subscription entries only; plain birth-entry hooks are retired",
                            entry.stage_identifier
                        ));
                    }
                }
            }
        }
    }
    // 5) 防跨源代铸环（源类级统一环检测，直连自环已在上面按条上报）。
    issues.extend(validate_mint_subscription_cycles(entries));
    issues
}

/// mint 跨源代铸环检测（源类级）：收集全部 mint 阶段的订阅目标 source 类，
/// 构建有向边并检测可达环（A→B→A、A→B→C→A）。成环意味着代铸事实在源类
/// 之间互相触发、永不收敛——per-fact mint 构成无界代铸环，编译期直接拒绝。
/// dock v1 起，经 zhixu 委托的远端类对译边由 link 阶段的 route 启动图
/// 环检测（D015）覆盖。
fn validate_mint_subscription_cycles(entries: &[StageEntry]) -> Vec<String> {
    let mut edges: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for entry in entries {
        if entry.stage.mint.is_none() {
            continue;
        }
        let source = entry.stage.source.clone();
        for raw_expression in entry.stage.receive_signals.values() {
            // 解析失败的条目不构边：语法错误由引用存在性校验统一上报。
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

/// 在 source 类有向图中找第一个可达环并回溯出完整路径（BFS + 父指针，
/// 节点遍历顺序确定保证诊断确定；迭代实现避免毒定义撑爆调用栈）。
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
                    // 回到起点：start -> … -> current -> start
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

fn validate_receive_signal_references(
    entries: &[StageEntry],
    input_port_hook_ids: &BTreeSet<String>,
) -> Vec<String> {
    let mut issues = Vec::new();
    let catalog = SignalReferenceCatalog::new(entries);
    for entry in entries {
        for (hook_name, raw_expression) in &entry.stage.receive_signals {
            // dockInterface input port 的 mailbox hook 由 dock 模块按端口
            // 约束校验（单一正向 atom、source 同域），不走普通引用校验。
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

// receiveSignals key 即阶段内 hook_name，落 hook_name 列（VARCHAR(36)）。
// 语法手册 §7.4："key 不可为空且不能含 '.'"；'#' 是 hookId 分隔符
// （stage#hook_name）——key 携带任一分隔符都会让 hookId 命名空间含混。
fn validate_receive_signal_keys(entries: &[StageEntry]) -> Vec<String> {
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
            if hook_name.contains('.') || hook_name.contains('#') || hook_name.len() > 36 {
                issues.push(format!(
                    "{}.receiveSignals.{hook_name} is invalid: key must be 1-36 bytes and must not contain '.' or '#'",
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
            // 订阅寻址只在本域解析（subscription-mint-spec §2.1）：receive
            // 钩子的依赖 source 必须 ∈ 本域 source 类集合。
            issues.push(format!(
                "{path} subscription source {} is not a declared source in this zhixu",
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
        if !referenced_stage.stage.send_signals.contains(&signal_name) {
            // 目标 stage 未声明 sendSignals 时任何引用都是悬空引用：文档要求
            // 引用存在，放行会把死依赖从编译期推迟为运行期静默 init。
            issues.push(format!(
                "{path} references unknown signal {stage_identifier}.{signal_name}"
            ));
        }
    }
    issues
}

fn parse_signal_reference(signal_name: &str) -> Option<(String, String)> {
    let parts = signal_name.split('.').collect::<Vec<_>>();
    if parts.len() != 3 {
        return None;
    }
    Some((format!("{}.{}", parts[0], parts[1]), parts[2].to_string()))
}

fn compile_stage_hooks(entry: &StageEntry, dock_state: &DockState) -> Result<Vec<Value>> {
    let mut hooks = Vec::new();
    let is_mint_stage = entry.stage.mint.is_some();
    // entrance 端口引用的目标侧 hook 是 dock 出生入口（PRD94 §3.4）。
    let entrance_hook_ids = dock_state
        .interface_json
        .as_ref()
        .map(|interface| {
            interface["inputs"]
                .as_array()
                .unwrap_or(&Vec::new())
                .iter()
                .filter(|port| port["kind"] == json!("entrance"))
                .filter_map(|port| port["hookId"].as_str().map(str::to_string))
                .collect::<BTreeSet<_>>()
        })
        .unwrap_or_default();
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
        // 是 executor dispatch 边（PRD94 §3.4 拆分 isTrigger）。
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

fn parse_hook_for_compiler(hook_name: &str, raw_expression: &str) -> Result<ParseHookOutput> {
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

fn parse_hook_for_cloud(hook_name: &str, raw_expression: &str) -> Result<ParseHookOutput> {
    parse_hook(ParseHookRequest {
        profile: Profile::CloudCompat,
        hook_name: hook_name.to_string(),
        hook: raw_expression.to_string(),
    })
    .map_err(|err| CompilerError::Message(err.to_string()))
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
            capabilities.push(capability);
        }
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

fn parse_signal_capability(entry: &StageEntry, declared_signal: &str) -> Result<Value> {
    let signal = declared_signal.trim();
    if signal.is_empty() {
        return Err(CompilerError::Issues(format!(
            "{}.sendSignals cannot contain an empty signal",
            entry.stage_identifier
        )));
    }
    if let Some((target_source, target_signal_name)) = signal.split_once("::") {
        let target_source = target_source.trim();
        let target_signal_name = target_signal_name.trim();
        if target_source.is_empty() || target_signal_name.is_empty() {
            return Err(CompilerError::Issues(format!(
                "{}.sendSignals contains invalid target signal {}",
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
    let target_signal_name = if signal.contains('.') {
        signal.to_string()
    } else {
        format!("{}.{}", entry.stage_identifier, signal)
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

fn has_static_executor(executor: Option<&ZhixuExecutor>) -> bool {
    match executor {
        None => false,
        // zhixu 委托执行器本身就是静态锚定（配置合法性由 dock 模块校验）。
        Some(executor) if executor.supplier_type.trim() == "zhixu" => true,
        Some(executor) => executor
            .supplier_id
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty()),
    }
}

fn value_str<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ------------------------------------------------------------------
    // PRD94 §3.1 目标示例（payment_execution）
    // ------------------------------------------------------------------
    fn target_payment_definition() -> Value {
        json!({
            "apiVersion": "uvp/v0",
            "kind": "Zhixu",
            "metadata": {
                "name": "payment_execution",
                "uid": "zx-payment-execution",
                "annotations": { "version": "1.2.0" }
            },
            "spec": {
                "platform": { "type": "cloud" },
                "nucleation": { "id": "payment-core" },
                "dockInterface": {
                    "schemaVersion": "uvp.dock.v1",
                    "inputs": {
                        "execute": {
                            "kind": "entrance",
                            "hook": "payment_flow.init#DOCK_EXECUTE",
                            "access": { "policy": "permit" }
                        },
                        "cancel": {
                            "kind": "signal",
                            "hook": "payment_flow.control#DOCK_CANCEL",
                            "access": { "policy": "linked" }
                        }
                    },
                    "outputs": {
                        "started": { "signal": "payment::payment_flow.init.str" },
                        "completed": {
                            "signal": "payment::payment_flow.settle.cmp",
                            "terminal": "success"
                        },
                        "failed": {
                            "signal": "payment::payment_flow.settle.err",
                            "terminal": "failure"
                        }
                    }
                },
                "taskPatterns": [
                    { "name": "payment_flow", "stages": [
                        {
                            "name": "init",
                            "source": "payment",
                            "receiveSignals": {
                                "DOCK_EXECUTE": "payment::payment_flow.init.execute"
                            },
                            "sendSignals": ["str"],
                            "executor": { "supplierType": "organization", "supplierID": "payment-gateway" }
                        },
                        {
                            "name": "control",
                            "source": "payment",
                            "receiveSignals": {
                                "DOCK_CANCEL": "payment::payment_flow.control.cancel"
                            },
                            "sendSignals": ["cxl"],
                            "executor": { "supplierType": "organization", "supplierID": "payment-gateway" }
                        },
                        {
                            "name": "settle",
                            "source": "payment",
                            "receiveSignals": {
                                "SETTLE": "payment::payment_flow.init.str"
                            },
                            "sendSignals": ["cmp", "err"],
                            "executor": { "supplierType": "organization", "supplierID": "payment-gateway" }
                        }
                    ]}
                ]
            }
        })
    }

    // ------------------------------------------------------------------
    // PRD94 §4.1/§12 调用方示例（settlement）
    // ------------------------------------------------------------------
    fn parent_settlement_definition() -> Value {
        json!({
            "apiVersion": "uvp/v0",
            "kind": "Zhixu",
            "metadata": {
                "name": "settlement",
                "uid": "zx-settlement",
                "annotations": { "version": "2.0.0" }
            },
            "spec": {
                "platform": { "type": "cloud" },
                "nucleation": { "id": "settlement-core" },
                "taskPatterns": [
                    { "name": "checkout", "stages": [
                        {
                            "name": "confirm",
                            "source": "buyer",
                            "sendSignals": ["cmp"],
                            "executor": { "supplierType": "organization", "supplierID": "buyer-app" }
                        },
                        {
                            "name": "cancel",
                            "source": "buyer",
                            "sendSignals": ["cmp"],
                            "executor": { "supplierType": "organization", "supplierID": "buyer-app" }
                        }
                    ]},
                    { "name": "settlement", "stages": [
                        {
                            "name": "execute_payment",
                            "source": "buyer",
                            "receiveSignals": {
                                "EXECUTE": "buyer::checkout.confirm.cmp",
                                "CANCEL": "buyer::checkout.cancel.cmp"
                            },
                            "sendSignals": ["str", "cmp", "err", "cxl"],
                            "executor": {
                                "supplierType": "zhixu",
                                "zhixuExecutorConfig": {
                                    "schemaVersion": "uvp.dock.v1",
                                    "target": { "zhixu": "zx-payment-execution", "version": "1.2.0" },
                                    "order": { "idPolicy": "derived-v1" },
                                    "inputMap": { "EXECUTE": "execute", "CANCEL": "cancel" },
                                    "signalMap": { "str": "started", "cmp": "completed", "err": "failed" }
                                }
                            }
                        }
                    ]}
                ]
            }
        })
    }

    /// 构造 resolution manifest：真实流程由 Store/发布系统在目标发布后
    /// 生成；测试中直接编译目标定义取其产物。
    fn manifest_for(target: &Value, extra_edges: Option<Value>) -> Value {
        let plan = compile_zhixu_hook_plan(target, None, true).expect("target compiles");
        let interface = plan["dockInterface"].clone();
        let mut entry = json!({
            "zhixu": "zx-payment-execution",
            "version": "1.2.0",
            "definitionRefHash": interface["definition"]["definitionRefHash"].clone(),
            "artifactHash": plan["planHash"].clone(),
            "published": true,
            "interface": interface,
            "evmPlanId": plan["planId"].clone(),
            "cloudArtifactId": format!("artifact://{}", plan["planHash"].as_str().unwrap_or(""))
        });
        if let Some(edges) = extra_edges {
            entry["dockEdges"] = edges;
        }
        json!({
            "schemaVersion": "uvp.dock.resolution.v1",
            "definitions": [entry]
        })
    }

    #[test]
    fn compiles_linked_parent_with_dock_route() {
        let manifest = manifest_for(&target_payment_definition(), None);
        let plan = compile_zhixu_hook_plan(&parent_settlement_definition(), Some(&manifest), false)
            .expect("linked parent compiles");
        assert_eq!(plan["schemaVersion"], "uvp.hookPlan.v2");
        assert_eq!(plan["zhixuId"], "zx-settlement");

        // hooks：无 signalMap 伪 hook；flags 拆分。
        let hooks = plan["compiledHooks"].as_array().unwrap();
        let hook_ids = hooks
            .iter()
            .map(|hook| hook["hookId"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert!(!hook_ids.iter().any(|id| id.starts_with("signalMap.")));
        let execute = hooks
            .iter()
            .find(|hook| hook["hookId"] == "settlement.execute_payment#EXECUTE")
            .unwrap();
        assert_eq!(execute["orderTriggerKind"], "none");
        assert_eq!(execute["emitReady"], true);

        // route：resolved，入口绑定 EXECUTE→execute。
        let routes = plan["dockRoutes"].as_array().unwrap();
        assert_eq!(routes.len(), 1);
        let route = &routes[0];
        assert_eq!(route["schemaVersion"], "uvp.dockRoute.v1");
        assert_eq!(
            route["local"]["stageIdentifier"],
            "settlement.execute_payment"
        );
        assert_eq!(route["orderIdPolicy"], "derived-v1");
        assert_eq!(route["sourceSeam"], "payment");
        assert_eq!(route["entrance"]["localHookName"], "EXECUTE");
        assert_eq!(route["entrance"]["targetPort"], "execute");
        assert_eq!(route["entrance"]["accessPolicy"], "permit");
        assert_eq!(route["inputs"].as_array().unwrap().len(), 2);
        assert_eq!(route["outputs"].as_array().unwrap().len(), 3);
        assert_ne!(
            route["routeHash"],
            "0x0000000000000000000000000000000000000000000000000000000000000000"
        );
        // zhixu stage 不在静态 executorRoutes 中（权威形态是 dockRoutes）。
        assert!(plan["executorRoutes"]
            .as_object()
            .unwrap()
            .get("settlement.execute_payment")
            .is_none());
    }

    #[test]
    fn target_compiles_interface_and_dock_trigger_flags() {
        let plan = compile_zhixu_hook_plan(&target_payment_definition(), None, true)
            .expect("target compiles standalone");
        let interface = &plan["dockInterface"];
        assert_eq!(interface["schemaVersion"], "uvp.dockInterfaceArtifact.v1");
        assert_eq!(interface["inputs"].as_array().unwrap().len(), 2);
        assert_eq!(interface["outputs"].as_array().unwrap().len(), 3);
        // 端口按名排序。
        let input_ports: Vec<&str> = interface["inputs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|port| port["port"].as_str().unwrap())
            .collect();
        assert_eq!(input_ports, vec!["cancel", "execute"]);

        let hooks = plan["compiledHooks"].as_array().unwrap();
        let entrance = hooks
            .iter()
            .find(|hook| hook["hookId"] == "payment_flow.init#DOCK_EXECUTE")
            .unwrap();
        assert_eq!(entrance["orderTriggerKind"], "dock");
        assert_eq!(entrance["emitReady"], true);
        let cancel = hooks
            .iter()
            .find(|hook| hook["hookId"] == "payment_flow.control#DOCK_CANCEL")
            .unwrap();
        assert_eq!(cancel["orderTriggerKind"], "none");
        assert_eq!(cancel["emitReady"], true);
    }

    #[test]
    fn rejects_parent_without_manifest() {
        let error = compile_zhixu_hook_plan(&parent_settlement_definition(), None, false)
            .expect_err("unresolved dock target must fail");
        assert!(
            error.to_string().contains("UNRESOLVED_DOCK_TARGET"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn parse_target_allows_unresolved() {
        let value = compile_zhixu_hook_plan(&parent_settlement_definition(), None, true)
            .expect("parse target allows unresolved routes");
        assert_eq!(value["dockRoutes"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn rejects_unsupported_executor_config_shapes() {
        // triggerEntrance：不受支持的调用方字段，D002 未知字段硬错误。
        let mut parent = parent_settlement_definition();
        parent["spec"]["taskPatterns"][1]["stages"][0]["executor"]["zhixuExecutorConfig"]
            .as_object_mut()
            .unwrap()
            .insert("triggerEntrance".to_string(), json!("payment_flow.init"));
        let error = compile_zhixu_hook_plan(&parent, None, false)
            .expect_err("triggerEntrance must hard-fail");
        let message = error.to_string();
        assert!(
            message.contains("D002") && message.contains("triggerEntrance"),
            "{message}"
        );
        assert!(message.contains("unknown field"), "{message}");

        // signalMap value 是 Hook DSL：v1 只接受端口名。
        let mut parent = parent_settlement_definition();
        parent["spec"]["taskPatterns"][1]["stages"][0]["executor"]["zhixuExecutorConfig"]
            .as_object_mut()
            .unwrap()["signalMap"]["str"] = json!("payment::payment_flow.init.str");
        let error = compile_zhixu_hook_plan(&parent, None, false)
            .expect_err("hook-DSL signalMap value must hard-fail");
        assert!(error.to_string().contains("D006"), "{}", error.to_string());

        // supplierID + zhixu：D001。
        let mut parent = parent_settlement_definition();
        parent["spec"]["taskPatterns"][1]["stages"][0]["executor"]["supplierID"] =
            json!("payment-zhixu");
        let error = compile_zhixu_hook_plan(&parent, None, false)
            .expect_err("supplierID on zhixu executor must fail");
        assert!(error.to_string().contains("D001"), "{}", error.to_string());
    }

    #[test]
    fn rejects_signal_map_keys_outside_hook_name_budget() {
        // D006：key 超 26 字节（hook_name = "signalMap." + key 落 VARCHAR(36)）。
        let mut parent = parent_settlement_definition();
        parent["spec"]["taskPatterns"][1]["stages"][0]["executor"]["zhixuExecutorConfig"]
            .as_object_mut()
            .unwrap()["signalMap"]["x".repeat(27)] = json!("started");
        let error = compile_zhixu_hook_plan(&parent, None, false)
            .expect_err("oversized signalMap key must fail");
        assert!(
            error.to_string().contains("D006") && error.to_string().contains("at most 26 bytes"),
            "{}",
            error.to_string()
        );

        // D006：key 携带信号名分隔符 '.'。
        let mut parent = parent_settlement_definition();
        parent["spec"]["taskPatterns"][1]["stages"][0]["executor"]["zhixuExecutorConfig"]
            .as_object_mut()
            .unwrap()["signalMap"]
            .as_object_mut()
            .unwrap()
            .insert("bad.key".to_string(), json!("cancelled"));
        let error = compile_zhixu_hook_plan(&parent, None, false)
            .expect_err("dotted signalMap key must fail");
        assert!(
            error.to_string().contains("D006")
                && error.to_string().contains("must not contain '.'"),
            "{}",
            error.to_string()
        );

        // 组合维度：stage 标识符 + key 超 signal_name 列宽（100）。
        let mut parent = parent_settlement_definition();
        let long_stage = "s".repeat(90);
        parent["spec"]["taskPatterns"][1]["stages"][0]["name"] = json!(long_stage);
        let error = compile_zhixu_hook_plan(&parent, None, false)
            .expect_err("combined signal name must fail");
        assert!(
            error.to_string().contains("individual_record.signal_name"),
            "{}",
            error.to_string()
        );
    }

    #[test]
    fn rejects_target_versions_outside_whitelist_charset() {
        // D003：黑名单时代 `1/2` 之类链轨串可通过；白名单与 Go 镜像同集。
        for version in ["1/2", "^1.0.0", "1.0.0 beta", "latest", ""] {
            let mut parent = parent_settlement_definition();
            parent["spec"]["taskPatterns"][1]["stages"][0]["executor"]["zhixuExecutorConfig"]
                .as_object_mut()
                .unwrap()["target"]["version"] = json!(version);
            let error = compile_zhixu_hook_plan(&parent, None, false)
                .expect_err("non-whitelisted target version must be rejected");
            assert!(
                error.to_string().contains("D003"),
                "version {version:?}: {error}"
            );
        }
        // 合法精确版本（含 +build 元数据）仍放行到 link 阶段。
        let mut parent = parent_settlement_definition();
        parent["spec"]["taskPatterns"][1]["stages"][0]["executor"]["zhixuExecutorConfig"]
            .as_object_mut()
            .unwrap()["target"]["version"] = json!("1.2.0+build.1");
        let error = compile_zhixu_hook_plan(&parent, None, false)
            .expect_err("valid version reaches linking, which fails without a manifest");
        assert!(
            !error.to_string().contains("D003"),
            "valid charset must not trip D003: {error}"
        );
    }

    /// 无锚扇入订阅 + zhixu 委托执行器（UVP-01）：编译期拒绝；本类存在
    /// mint 声明（有锚，route=order）时放行。对齐 Go 镜像
    /// TestValidateStage_AnchoredSubscriptionAllowsZhixuExecutor 的边界。
    fn delegation_subscription_definition(with_anchor: bool) -> Value {
        let mut stages = vec![
            json!({
                "name": "fanin",
                "source": "anchoredcls",
                "receiveSignals": { "SUB": "::ANCHOR(@other::anchor_task.emit.cmp)" },
                "sendSignals": ["str", "cmp"],
                "executor": {
                    "supplierType": "zhixu",
                    "zhixuExecutorConfig": {
                        "schemaVersion": "uvp.dock.v1",
                        "target": { "zhixu": "zx-target", "version": "1.0.0" },
                        "order": { "idPolicy": "derived-v1" },
                        "inputMap": { "SUB": "execute" },
                        "signalMap": { "str": "started", "cmp": "completed" }
                    }
                }
            }),
            // 订阅目标 source 类必须在本域声明（引用存在性校验）。
            json!({
                "name": "emit",
                "source": "other",
                "sendSignals": ["cmp"],
                "executor": { "supplierType": "organization", "supplierID": "other-org" }
            }),
        ];
        if with_anchor {
            stages.push(json!({
                "name": "anchor",
                "source": "anchoredcls",
                "mint": "per-fact",
                "receiveSignals": { "SPAWN": "::ANCHOR(@other::anchor_task.emit.cmp)" },
                "sendSignals": ["str"],
                "executor": { "supplierType": "organization", "supplierID": "anchor-org" }
            }));
        }
        json!({
            "apiVersion": "uvp/v0",
            "kind": "Zhixu",
            "metadata": {
                "name": "delegation_subscription",
                "uid": "zx-delegation-sub",
                "annotations": { "version": "1.0.0" }
            },
            "spec": {
                "platform": { "type": "cloud" },
                "nucleation": { "id": "delegation-core" },
                "taskPatterns": [ { "name": "anchor_task", "stages": stages } ]
            }
        })
    }

    #[test]
    fn rejects_unanchored_subscription_stage_with_zhixu_executor() {
        let error = compile_zhixu_hook_plan(&delegation_subscription_definition(false), None, true)
            .expect_err("unanchored fan-in subscription + zhixu executor must fail");
        assert!(
            error
                .to_string()
                .contains("unanchored fan-in subscription stage cannot bind a zhixu delegation"),
            "{error}"
        );
        // cloud target 同口径。
        let error = compile_cloud_artifact(&delegation_subscription_definition(false), None, true)
            .expect_err("cloud target must reject the same combination");
        assert!(
            error
                .to_string()
                .contains("unanchored fan-in subscription stage cannot bind a zhixu delegation"),
            "{error}"
        );
    }

    #[test]
    fn anchored_subscription_stage_allows_zhixu_executor() {
        // 本类（anchoredcls）存在 mint 声明：订阅按 route=order 沿对接记录
        // 按单投递，委托信封可携带订单锚定。
        compile_zhixu_hook_plan(&delegation_subscription_definition(true), None, true)
            .expect("anchored subscription with zhixu executor compiles");
        compile_cloud_artifact(&delegation_subscription_definition(true), None, true)
            .expect("cloud target accepts the anchored combination");
    }

    #[test]
    fn rejects_invalid_stage_sources() {
        for (label, source) in [
            ("empty", String::new()),
            ("whitespace", "  ".to_string()),
            ("space inside", "sell er".to_string()),
            ("unicode", "卖家".to_string()),
            ("oversized", "s".repeat(37)),
        ] {
            let mut parent = parent_settlement_definition();
            parent["spec"]["taskPatterns"][1]["stages"][0]["source"] = json!(source);
            let error = compile_zhixu_hook_plan(&parent, None, false)
                .expect_err("invalid stage source must be rejected");
            assert!(
                error.to_string().contains(".source"),
                "source {label:?}: {error}"
            );
        }
        // 36 字节边界恰好放行。
        let mut parent = parent_settlement_definition();
        parent["spec"]["taskPatterns"][1]["stages"][0]["source"] = json!("s".repeat(36));
        let error = compile_zhixu_hook_plan(&parent, None, false)
            .expect_err("boundary source must pass shape checks and fail later on linking");
        assert!(
            !error.to_string().contains("exceeds 36 bytes"),
            "36-byte source is legal: {error}"
        );
    }

    #[test]
    fn rejects_unknown_spec_and_executor_fields() {
        // spec 顶层未知字段（含不受支持的 trigger/externalSignals）不被静默
        // 忽略/透传（对齐 Go 入口 decodeObjectStrict）。
        let mut parent = parent_settlement_definition();
        parent["spec"]["trigger"] = json!([]);
        let error = compile_zhixu_hook_plan(&parent, None, false)
            .expect_err("unsupported spec-level trigger key must fail");
        assert!(
            error.to_string().contains("unknown field `trigger`"),
            "{error}"
        );

        let mut parent = parent_settlement_definition();
        parent["spec"]["externalSignals"] = json!({});
        let error = compile_zhixu_hook_plan(&parent, None, false)
            .expect_err("unsupported spec-level externalSignals key must fail");
        assert!(
            error
                .to_string()
                .contains("unknown field `externalSignals`"),
            "{error}"
        );

        // executor 内未知字段。
        let mut parent = parent_settlement_definition();
        parent["spec"]["taskPatterns"][1]["stages"][0]["executor"]["handlerType"] = json!("http");
        let error = compile_zhixu_hook_plan(&parent, None, false)
            .expect_err("unknown executor field must fail");
        assert!(
            error.to_string().contains("unknown field `handlerType`"),
            "{error}"
        );

        // metadata 层未知字段（如 description）同样拒绝。
        let mut parent = parent_settlement_definition();
        parent["metadata"]["description"] = json!("demo");
        let error = compile_zhixu_hook_plan(&parent, None, false)
            .expect_err("metadata-level unknown field must fail");
        assert!(
            error.to_string().contains("unknown field `description`"),
            "{error}"
        );
    }

    #[test]
    fn rejects_interface_shape_violations() {
        let mut target = target_payment_definition();
        // 组合表达式的 input port hook。
        target["spec"]["taskPatterns"][0]["stages"][0]["receiveSignals"]["DOCK_EXECUTE"] =
            json!("payment::payment_flow.init.execute & payment::payment_flow.control.cxl");
        let error = compile_zhixu_hook_plan(&target, None, true)
            .expect_err("composite input port hook must fail");
        assert!(error.to_string().contains("D013"), "{}", error.to_string());

        // 输出端口引用非 sendSignals 信号。
        let mut target = target_payment_definition();
        target["spec"]["dockInterface"]["outputs"]["started"]["signal"] =
            json!("payment::payment_flow.init.nope");
        let error = compile_zhixu_hook_plan(&target, None, true)
            .expect_err("unknown output signal must fail");
        assert!(error.to_string().contains("D014"), "{}", error.to_string());

        // signal kind 使用非 linked policy。
        let mut target = target_payment_definition();
        target["spec"]["dockInterface"]["inputs"]["cancel"]["access"]["policy"] = json!("open");
        let error = compile_zhixu_hook_plan(&target, None, true)
            .expect_err("signal port with open policy must fail");
        assert!(error.to_string().contains("D023"), "{}", error.to_string());

        // 非法端口名。
        let mut target = target_payment_definition();
        let inputs = target["spec"]["dockInterface"]["inputs"]
            .as_object_mut()
            .unwrap();
        let execute = inputs.remove("execute").unwrap();
        inputs.insert("BadPort".to_string(), execute);
        let error =
            compile_zhixu_hook_plan(&target, None, true).expect_err("invalid port name must fail");
        assert!(error.to_string().contains("D021"), "{}", error.to_string());
    }

    #[test]
    fn rejects_link_violations() {
        // D008：目标不在 manifest。
        let mut manifest = manifest_for(&target_payment_definition(), None);
        manifest["definitions"][0]["zhixu"] = json!("zx-other");
        let error =
            compile_zhixu_hook_plan(&parent_settlement_definition(), Some(&manifest), false)
                .expect_err("missing target must fail");
        assert!(error.to_string().contains("D008"), "{}", error.to_string());

        // D008：interfaceRoot 与叶子不一致。
        let mut manifest = manifest_for(&target_payment_definition(), None);
        manifest["definitions"][0]["interface"]["interfaceRoot"] =
            json!("0x0000000000000000000000000000000000000000000000000000000000000001");
        let error =
            compile_zhixu_hook_plan(&parent_settlement_definition(), Some(&manifest), false)
                .expect_err("tampered interfaceRoot must fail");
        assert!(
            error.to_string().contains("interfaceRoot"),
            "{}",
            error.to_string()
        );

        // D009：引用不存在的输出端口。
        let mut parent = parent_settlement_definition();
        parent["spec"]["taskPatterns"][1]["stages"][0]["executor"]["zhixuExecutorConfig"]
            .as_object_mut()
            .unwrap()["signalMap"]["cxl"] = json!("cancelled");
        let manifest = manifest_for(&target_payment_definition(), None);
        let error = compile_zhixu_hook_plan(&parent, Some(&manifest), false)
            .expect_err("unknown output port must fail");
        assert!(error.to_string().contains("D009"), "{}", error.to_string());

        // D010：inputMap 缺 entrance（目标去掉 entrance 端口后 link）。
        let mut target = target_payment_definition();
        let inputs = target["spec"]["dockInterface"]["inputs"]
            .as_object_mut()
            .unwrap();
        let mut execute = inputs.remove("execute").unwrap();
        execute["kind"] = json!("signal");
        execute["access"]["policy"] = json!("linked");
        inputs.insert("execute".to_string(), execute);
        let manifest = manifest_for(&target, None);
        let error =
            compile_zhixu_hook_plan(&parent_settlement_definition(), Some(&manifest), false)
                .expect_err("missing entrance must fail");
        assert!(error.to_string().contains("D010"), "{}", error.to_string());

        // D015：目标边回指父定义（经 manifest dockEdges）。
        let edges = json!([{ "zhixu": "zx-settlement", "version": "2.0.0" }]);
        let manifest = manifest_for(&target_payment_definition(), Some(edges));
        let error =
            compile_zhixu_hook_plan(&parent_settlement_definition(), Some(&manifest), false)
                .expect_err("route cycle must fail");
        assert!(error.to_string().contains("D015"), "{}", error.to_string());
    }

    #[test]
    fn rejects_evm_routes_without_plan_identity_and_cloud_without_artifact_id() {
        // D018：可运行产物必须能解析 runtime target identity。
        let manifest = manifest_for(&target_payment_definition(), None);
        let mut no_plan = manifest.clone();
        no_plan["definitions"][0]
            .as_object_mut()
            .unwrap()
            .remove("evmPlanId");
        let error = compile_zhixu_hook_plan(&parent_settlement_definition(), Some(&no_plan), false)
            .expect_err("evm profile without evmPlanId must fail");
        assert!(error.to_string().contains("D018"), "{}", error.to_string());

        let mut no_artifact = manifest;
        no_artifact["definitions"][0]
            .as_object_mut()
            .unwrap()
            .remove("cloudArtifactId");
        let error =
            compile_cloud_artifact(&parent_settlement_definition(), Some(&no_artifact), false)
                .expect_err("cloud profile without cloudArtifactId must fail");
        assert!(error.to_string().contains("D018"), "{}", error.to_string());
    }

    #[test]
    fn cloud_artifact_uses_resolved_routes() {
        let manifest = manifest_for(&target_payment_definition(), None);
        let artifact =
            compile_cloud_artifact(&parent_settlement_definition(), Some(&manifest), false)
                .expect("cloud artifact compiles with manifest");
        assert_eq!(artifact["schemaVersion"], "uvp.cloudArtifact.v2");
        let hooks = artifact["hooks"].as_array().unwrap();
        assert!(hooks
            .iter()
            .all(|hook| hook["sourceZhixuRef"] == json!("self")));
        assert_eq!(artifact["dockRoutes"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn rejects_invalid_task_or_stage_identifier_parts() {
        for (field, value) in [
            ("taskPatterns[0].name", "checkout.main"),
            ("stages[0].name", "1main"),
        ] {
            let mut definition = target_payment_definition();
            if field.starts_with("taskPatterns") {
                definition["spec"]["taskPatterns"][0]["name"] = json!(value);
            } else {
                definition["spec"]["taskPatterns"][0]["stages"][0]["name"] = json!(value);
            }
            let error = compile_zhixu_hook_plan(&definition, None, true)
                .expect_err("invalid identifier must fail");
            assert!(error
                .to_string()
                .contains("must start with an ASCII letter"));
        }
    }

    #[test]
    fn rejects_empty_or_whitespace_metadata_name() {
        for name in ["", "   ", "\t"] {
            let mut definition = target_payment_definition();
            definition["metadata"]["name"] = json!(name);
            let error = compile_zhixu_hook_plan(&definition, None, true)
                .expect_err("metadata.name must not be empty or whitespace");
            assert!(
                error
                    .to_string()
                    .contains("metadata.name must be non-empty"),
                "unexpected error for {name:?}: {error}"
            );
        }
    }

    #[test]
    fn rejects_subscription_stage_bound_only_through_selected_stages() {
        // 订阅阶段的投递目标编译期定死、运行时禁止
        // executor patch。被 selector 指到的订阅阶段仍必须有自身静态 executor。
        let definition = json!({
            "apiVersion": "uvp/v0",
            "kind": "Zhixu",
            "metadata": {
                "name": "subscription_static_executor",
                "uid": "zx-subscription-static",
                "annotations": { "version": "1" }
            },
            "spec": {
                "platform": { "type": "cloud" },
                "nucleation": { "id": "core" },
                "taskPatterns": [
                    { "name": "selector", "stages": [
                        {
                            "name": "assign",
                            "source": "buyer",
                            "selectedStages": ["execution.main"],
                            "sendSignals": ["executor_selected"],
                            "executor": { "supplierType": "organization", "supplierID": "selector-org" }
                        }
                    ]},
                    { "name": "execution", "stages": [
                        {
                            "name": "main",
                            "source": "buyer",
                            "receiveSignals": {
                                "OBS": "::ANCHOR(@buyer::selector.assign.executor_selected)"
                            }
                        }
                    ]}
                ]
            }
        });

        let error = compile_zhixu_hook_plan(&definition, None, true)
            .expect_err("subscription stage without its own static executor must fail");
        assert!(
            error
                .to_string()
                .contains("requires its own static executor"),
            "unexpected error: {error}"
        );

        let error = compile_cloud_artifact(&definition, None, true)
            .expect_err("cloud target must enforce the same static executor contract");
        assert!(
            error
                .to_string()
                .contains("requires its own static executor"),
            "unexpected cloud error: {error}"
        );
    }

    #[test]
    fn rejects_receive_hooks_on_stage_without_static_executor() {
        // 簇 A 阶段物化裁决：无静态 executor、仅 selectedStages 覆盖的阶段
        // 不得声明 receiveSignals——hook 编译为 flags=0 watcher，链上永远
        // 无法物化（纯 watcher 不物化、executor patch 不物化）。
        let definition = json!({
            "apiVersion": "uvp/v0",
            "kind": "Zhixu",
            "metadata": {
                "name": "unmaterializable_watcher",
                "uid": "zx-unmaterializable",
                "annotations": { "version": "1" }
            },
            "spec": {
                "platform": { "type": "evm" },
                "nucleation": { "id": "core" },
                "taskPatterns": [
                    { "name": "selector", "stages": [
                        {
                            "name": "assign",
                            "source": "buyer",
                            "selectedStages": ["execution.main"],
                            "sendSignals": ["executor_selected"],
                            "executor": { "supplierType": "organization", "supplierID": "selector-org" }
                        }
                    ]},
                    { "name": "execution", "stages": [
                        {
                            "name": "main",
                            "source": "buyer",
                            "receiveSignals": {
                                "OBS": "buyer::selector.assign.executor_selected"
                            }
                        }
                    ]}
                ]
            }
        });

        let error = compile_zhixu_hook_plan(&definition, None, true)
            .expect_err("flags=0 watcher on a selectedStages-only stage must fail");
        assert!(
            error
                .to_string()
                .contains("can never materialize the stage"),
            "unexpected error: {error}"
        );

        // Cloud 目标不做 onchain 物化裁决：同一定义仍可编译（投递语义在云侧
        // 运行时），物化死锁是链轨专属形态。
        compile_cloud_artifact(&definition, None, true)
            .expect("cloud target must not enforce on-chain materialization");
    }

    #[test]
    fn rejects_receive_signal_keys_with_separators() {
        let mut definition = target_payment_definition();
        definition["spec"]["taskPatterns"][0]["stages"][0]["receiveSignals"]["BAD.KEY"] =
            json!("payment::payment_flow.init.execute");
        let error = compile_zhixu_hook_plan(&definition, None, true)
            .expect_err("receiveSignals key containing '.' must fail");
        let message = error.to_string();
        assert!(
            message.contains("receiveSignals.BAD.KEY")
                && message.contains("must not contain '.' or '#'"),
            "unexpected error: {message}"
        );

        let mut definition = target_payment_definition();
        definition["spec"]["taskPatterns"][0]["stages"][0]["receiveSignals"]["BAD#KEY"] =
            json!("payment::payment_flow.init.execute");
        let error = compile_zhixu_hook_plan(&definition, None, true)
            .expect_err("receiveSignals key containing '#' must fail");
        assert!(
            error.to_string().contains("must not contain '.' or '#'"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn rejects_mutual_mint_subscription_cycle() {
        // A↔B 互订：无界代铸环，编译期拒绝（含 cloud/hook_plan 两个 target）。
        let definition = mint_definitions(&[
            (
                "dispatch",
                mint_stage_value(
                    "dispatch",
                    "main",
                    "producer",
                    json!({ "SPAWN": "::ANCHOR(@buyer::orchard.retail.ack)" }),
                    &["smart_contract"],
                ),
            ),
            (
                "orchard",
                mint_stage_value(
                    "orchard",
                    "retail",
                    "buyer",
                    json!({ "SPAWN": "::ANCHOR(@producer::dispatch.main.smart_contract)" }),
                    &["ack"],
                ),
            ),
        ]);
        let error = compile_zhixu_hook_plan(&definition, None, true)
            .expect_err("mutual mint subscription cycle must fail");
        let message = error.to_string();
        assert!(
            message.contains("unbounded re-mint cycle")
                && message.contains("buyer -> producer -> buyer"),
            "unexpected error: {message}"
        );
        let error = compile_cloud_artifact(&definition, None, true)
            .expect_err("cloud target must reject the cycle too");
        assert!(
            error.to_string().contains("unbounded re-mint cycle"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn allows_acyclic_mint_subscription_chain() {
        // A→B 不回指：出生订阅链无环时必须照常放行。
        let definition = mint_definitions(&[
            (
                "dispatch",
                emitter_stage_value("dispatch", "main", "producer", &["smart_contract"]),
            ),
            (
                "orchard",
                mint_stage_value(
                    "orchard",
                    "retail",
                    "buyer",
                    json!({ "SPAWN": "::ANCHOR(@producer::dispatch.main.smart_contract)" }),
                    &["ack"],
                ),
            ),
        ]);
        compile_zhixu_hook_plan(&definition, None, true)
            .expect("acyclic mint subscription chain must compile for hook_plan");
        compile_cloud_artifact(&definition, None, true)
            .expect("acyclic mint subscription chain must compile for cloud");
    }

    #[test]
    fn still_rejects_mint_stage_subscribing_its_own_source() {
        // 直连自环（mint 阶段订阅自身 source 类）保持既有按条报错口径。
        let definition = mint_definitions(&[
            (
                "dispatch",
                emitter_stage_value("dispatch", "main", "producer", &["smart_contract"]),
            ),
            (
                "orchard",
                mint_stage_value(
                    "orchard",
                    "retail",
                    "producer",
                    json!({ "SPAWN": "::ANCHOR(@producer::dispatch.main.smart_contract)" }),
                    &["ack"],
                ),
            ),
        ]);
        let error = compile_zhixu_hook_plan(&definition, None, true)
            .expect_err("mint stage subscribing its own source class must fail");
        assert!(
            error
                .to_string()
                .contains("must not subscribe its own source class"),
            "unexpected error: {error}"
        );
    }

    fn mint_stage_value(
        task: &str,
        name: &str,
        source: &str,
        receive_signals: Value,
        send_signals: &[&str],
    ) -> Value {
        json!({
            "name": name,
            "source": source,
            "receiveSignals": receive_signals,
            "sendSignals": send_signals,
            "mint": "per-fact",
            "executor": {
                "supplierType": "organization",
                "supplierID": format!("{task}-{name}-executor")
            }
        })
    }

    fn emitter_stage_value(task: &str, name: &str, source: &str, send_signals: &[&str]) -> Value {
        json!({
            "name": name,
            "source": source,
            "sendSignals": send_signals,
            "executor": {
                "supplierType": "organization",
                "supplierID": format!("{task}-{name}-executor")
            }
        })
    }

    fn mint_definitions(stages: &[(&str, Value)]) -> Value {
        json!({
            "apiVersion": "uvp/v0",
            "kind": "Zhixu",
            "metadata": {
                "name": "mint_cycle",
                "uid": "zhixu-mint-cycle-001",
                "annotations": { "version": "1" }
            },
            "spec": {
                "platform": { "type": "cloud" },
                "nucleation": { "id": "core" },
                "taskPatterns": stages
                    .iter()
                    .map(|(task, stage)| json!({ "name": task, "stages": [stage] }))
                    .collect::<Vec<_>>()
            }
        })
    }
}
