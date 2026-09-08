use serde::Deserialize;
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use thiserror::Error;
use uvp_hook_dsl::{
    parse_hook, Compatibility, DependencyKind, ParseHookOutput, ParseHookRequest, Profile,
};
use uvp_model::{ZhixuDefinition, ZhixuExecutor, ZhixuStage};

pub mod dock;

/// HookPlan 产物信封版本（TS 权威 uvp-protocol compiler types 的
/// HOOK_PLAN_SCHEMA_VERSION 镜像）。pub 供 uvp-node NAPI 导出
/// hookPlanSchemaVersion：TS 侧兼容门逐字比对两侧常量，防漂移。
pub const HOOK_PLAN_SCHEMA_VERSION: &str = "uvp.hookPlan.v2";
/// cloud 编译产物的信封版本：Go 侧 pkg/version.CloudArtifactSchema 镜像此值，
/// parity 测试按 `pub const` 声明逐字比对，必须保持 pub。
pub const CLOUD_ARTIFACT_SCHEMA_VERSION: &str = "uvp.cloudArtifact.v2";
/// G-18：UVPStateMachine._signalStageId 在每次信号提交时线性扫描
/// signalCapabilities，无上限则单次提交 gas 随 plan 规模无界增长。
/// 256 使扫描 gas 低于 ~5k。TS 侧 onchain-hook-plan.ts 的
/// MAX_SIGNAL_CAPABILITIES 在编译+反序列化两边界同值同文案；Rust 是语义
/// 权威，此值即上限的唯一出处，TS 必须镜像。
const MAX_SIGNAL_CAPABILITIES: usize = 256;

#[derive(Debug, Error)]
pub enum CompilerError {
    #[error("{0}")]
    Message(String),
    #[error("compilation failed: {0}")]
    Issues(String),
}

type Result<T> = std::result::Result<T, CompilerError>;

#[derive(Debug, Deserialize)]
// FFI/NAPI 最外层请求信封：未知字段确定性拒绝（拼错的调用方输入不得
// 被静默忽略成零值语义）。
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct CompileRequest {
    #[serde(default = "default_target")]
    pub target: String,
    #[serde(alias = "zhixu")]
    pub definition: Value,
    /// Dock resolution manifest：由 Store/发布系统或离线
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
        // parse-only：允许 unresolved route。
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

/// hook_plan 产物：中性 plan 壳（模型/校验/编译结果 + dock 声明面）。
/// 哈希承诺（planHash/roots/派生身份）由链轨 TS 在此壳上计算；云轨
/// 身份归 DB——core 不产出任何身份字段。
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
    let dock_state = compile_dock_state(&definition, &stage_pairs, resolution_manifest, allow_unresolved)?;

    let mut validation_issues = Vec::new();
    validation_issues.extend(validate_stage_executors(
        &stage_entries,
        &selected_stage_bindings,
    ));
    // 阶段物化门（簇 A，onchain 目标）：每个阶段声明都必须编译出至少一个
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

    let platform = normalize_platform_value(&definition.spec.platform)?;

    let mut compiled_hooks = Vec::new();
    for entry in &stage_entries {
        compiled_hooks.extend(compile_stage_hooks(entry, &dock_state)?);
    }
    let dependency_index = build_dependency_index(&compiled_hooks);
    let signal_capabilities = build_signal_capabilities(&stage_entries)?;
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
    let dock_state =
        compile_dock_state(&definition, &stage_pairs, resolution_manifest, allow_unresolved)?;

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
    // sendSignals capability 同口径（空串/重复在两个 target 一致拒绝）：
    // cloud 产物供 Go 主链路消费，不得放行 hook_plan 已拒绝的声明。
    build_signal_capabilities(&stage_entries)?;
    if !validation_issues.is_empty() {
        return Err(CompilerError::Issues(validation_issues.join("; ")));
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

/// dock_link：只做 link 的 API 边界——校验父定义的 dock 声明能按 manifest
/// 全量解析，输出中性 route 与接口声明（不编译 hooks）。
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
    let dock_state = compile_dock_state(&definition, &stage_pairs, resolution_manifest, false)?;
    let mut artifact = json!({
        "schemaVersion": "uvp.dockLink.v1",
        "dockRoutes": Value::Array(dock_state.routes_json),
    });
    if let Some(interface_json) = dock_state.interface_json {
        artifact["dockInterface"] = interface_json;
    }
    if !dock_state.unresolved_json.is_empty() {
        artifact["unresolvedDockRoutes"] = Value::Array(dock_state.unresolved_json);
    }
    Ok(artifact)
}

/// 一次编译中的 dock 状态：接口声明、未链接/已链接 route、hook 标记输入。
struct DockState {
    interface_json: Option<Value>,
    routes_json: Vec<Value>,
    /// target:null 动态选择 route 的声明面产物：不进 link——目标空缺的
    /// route 没有可解析的绑定面。
    unresolved_json: Vec<Value>,
    /// dockInterface input port 引用的本地 hook（`<task>.<stage>#<hook>`），
    /// 这些 mailbox hook 不走普通依赖引用校验（dock 模块按端口约束校验）。
    input_port_hook_ids: BTreeSet<String>,
    /// 可作为 new 模式出生锚的 input 端口（orderModes 含 new 的接口）
    /// 引用的本地 hook：编译为 orderTriggerKind=dock。
    entrance_hook_ids: BTreeSet<String>,
}

fn compile_dock_state(
    definition: &ZhixuDefinition,
    stage_pairs: &[(String, ZhixuStage)],
    resolution_manifest: Option<&Value>,
    allow_unresolved: bool,
) -> Result<DockState> {
    let unlinked =
        dock::collect_unlinked_routes(stage_pairs).map_err(|issues| issues_from_dock(&issues))?;

    // target:null 的动态选择 route 不进 link（目标空缺，无 D008 可言），
    // 改入未解析清单随产物携带（云轨运行时由选择记录补齐）。
    let mut static_routes = Vec::new();
    let mut unresolved_json = Vec::new();
    for route in unlinked {
        match route.config.target_name.as_ref() {
            Some(_) => static_routes.push(route),
            None => unresolved_json.push(route.unresolved_json()),
        }
    }

    let interfaces = if definition.spec.dock_interface.is_empty() {
        None
    } else {
        Some(
            dock::compile_dock_interface(&definition.spec.dock_interface, stage_pairs)
                .map_err(|issues| issues_from_dock(&issues))?,
        )
    };
    let input_port_hook_ids = interfaces
        .as_deref()
        .map(dock::input_port_hook_ids)
        .unwrap_or_default();
    let entrance_hook_ids = interfaces
        .as_deref()
        .map(dock::entrance_hook_ids)
        .unwrap_or_default();

    let routes = if static_routes.is_empty() {
        Vec::new()
    } else {
        match resolution_manifest {
            Some(manifest_value) => {
                let manifest = dock::parse_resolution_manifest(manifest_value)
                    .map_err(|issues| issues_from_dock(&issues))?;
                dock::link_dock_routes(&definition.metadata.name, &static_routes, &manifest)
                    .map_err(|issues| issues_from_dock(&issues))?
            }
            None if allow_unresolved => Vec::new(),
            None => {
                return Err(CompilerError::Message(
                    "UNRESOLVED_DOCK_TARGET: definition contains zhixu executor routes with static targets but no resolutionManifest was provided; runnable compilation requires linking against published target interfaces".to_string(),
                ));
            }
        }
    };
    let routes_json = routes
        .iter()
        .map(|route| route.to_json())
        .collect::<Vec<_>>();
    Ok(DockState {
        interface_json: interfaces
            .as_deref()
            .map(dock::interface_declarations_json),
        routes_json,
        unresolved_json,
        input_port_hook_ids,
        entrance_hook_ids,
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

/// 全局 stage.source 上限：DSL 壳字段统一 100 字节（与 metadata.name
/// 同宽，bug_audit #14）。hook_name 的 36 字节上限是链轨落库内幕
/// （global_hook.hook_name 列宽），不反向约束 DSL 壳字段。
const MAX_STAGE_SOURCE_BYTES: usize = 100;
/// DDL 维度镜像：global_zhixu.name / global_stage.stage_identifier
/// VARCHAR(100)。
const MAX_IDENTIFIER_BYTES: usize = 100;
/// DDL 维度镜像：canonical 三段式 task.stage.signal 落
/// individual_record.signal_name / hook_dependency.signal_name VARCHAR(100)。
const MAX_SIGNAL_NAME_BYTES: usize = 100;
/// metadata.name 的 slug 形态：技术名风格，仅限形态校验，
/// 不承担任何语义判断（非唯一、不参与关系推断）。
const NAME_SLUG_PATTERN: &str = "^[a-z][a-z0-9_-]{0,99}$";

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
    // N7：name 是作者技术标签，slug 形态保证任何报错都有可读且可排序的
    // 标签；校验仅限形态。
    if !is_name_slug(&definition.metadata.name) {
        issues.push(format!(
            "metadata.name {:?} must match {NAME_SLUG_PATTERN} (definition-local technical label)",
            definition.metadata.name
        ));
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
        // 与文法一致（taskPattern 至少含一个 stage）：空/缺失 stages 的
        // taskPattern 是确定性的非法形状，不得靠 serde default 编译成
        // "无阶段任务"。
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
                // supplierType 闭集（uvp_model::SUPPLIER_TYPES）：拼错的类型
                // 会经 executorRoutes 进链上承诺，闭集外的字符串在此拒绝。
                if !uvp_model::is_known_supplier_type(&executor.supplier_type) {
                    issues.push(format!(
                        "spec.taskPatterns[{task_index}].stages[{stage_index}].executor.supplierType must be one of {} (trim-sensitive), found {:?}",
                        uvp_model::SUPPLIER_TYPES
                            .map(|value| format!("{value:?}"))
                            .join(", "),
                        executor.supplier_type
                    ));
                }
            }
            // stage.source：非空、plain identifier 字符集、≤100（DSL 壳
            // 字段统一 100 字节，与 metadata.name 同宽，bug_audit #14）。
            // 空串会以空键混进 mintedSources；含空格/Unicode 的 source
            // 是路由键，两侧必须逐字节一致（Go 镜像 zhixu_schema.go 同款
            // 字符集校验；36 字节的 hook_name 上限是链轨落库内幕，不约束
            // 本 DSL 壳字段）。
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

/// `^[a-z][a-z0-9_-]{0,99}$`（字节口径）。
fn is_name_slug(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.is_empty() || bytes.len() > MAX_IDENTIFIER_BYTES || !bytes[0].is_ascii_lowercase() {
        return false;
    }
    bytes[1..]
        .iter()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'_' || *b == b'-')
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
            // dockRoutes 中的 resolved DockRoute（dockRoutes 是权威形态）。
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
    // 非委托 executor 必须携带非空 supplierID（对齐 TS 侧同款拒绝）：
    // 缺 supplierID 的执行器即使被 selectedStages 锚定也是"看似绑定"——
    // 产物里会出现没有投递目标的 executor route。zhixu 委托的身份在
    // zhixuExecutorConfig.target（D001 禁 supplierID），不在此列。
    for entry in entries {
        let Some(executor) = entry.stage.executor.as_ref() else {
            continue;
        };
        if executor.supplier_type.trim() == "zhixu" {
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

/// 阶段物化门（簇 A，onchain 目标）：链上阶段只能由本阶段 order-trigger
/// （mint/dock）或 EMIT_READY hook Ready 物化；executor patch 也不物化
/// （UVPStateMachine activateStageExecutor 不调用 _materializeStage）。
/// 因此每个阶段声明都必须编译出至少一个带物化位的 hook（P0-4）：
/// - 仅 sendSignals、无 receiveSignals 的阶段编译为零 hook——阶段永不可
///   物化，其信号在链上没有钩子可挂（_recordSignal 要求源阶段已物化，
///   submitSignal 恒 revert UnknownHook），下游 hook 永 Init；
/// - 有 receiveSignals 但全部编译为 flags=0 纯 watcher 的阶段同样不物化。
///
/// dockInterface entrance 端口钩子编译为 dock|emitReady（=6），是合法
/// 物化路径，不按 watcher 拒绝（CORE-8）。
fn validate_onchain_stage_materialization(
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

// 可作为 new 模式出生锚的 dockInterface input 端口（orderModes 含 new 的
// 接口的全部 input 端口——new 模式 route 的唯一 input 绑定可落在其中任意
// 一个）引用的本地 hook（`<task>.<stage>#<hook>`）。这些钩子编译为
// orderTriggerKind=dock（flags=dock|emitReady=6），既是阶段物化门里的合法
// 物化路径，也是 compile_stage_hooks 打 order-trigger 标记的依据——两处
// 共用同一来源，避免判定口径漂移。
fn dock_entrance_hook_ids(dock_state: &DockState) -> BTreeSet<String> {
    dock_state.entrance_hook_ids.clone()
}

fn compile_stage_hooks(entry: &StageEntry, dock_state: &DockState) -> Result<Vec<Value>> {
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
    // dependencies 此处只投 signalName/dependencyKind 两维：Go 主链路按
    // (signalName, kind) 消费该结构（uvp.cloudArtifact.v2 冻结面），source
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
    // 上限在去重之后检查：重复声明已被前面拒绝，此处计数即编译产物的
    // signalCapabilities 长度，与 TS signalCapabilityCountIssues 同口径。
    if capabilities.len() > MAX_SIGNAL_CAPABILITIES {
        return Err(CompilerError::Issues(format!(
            "signal capabilities {} exceed the documented limit {} (UVPStateMachine._signalStageId linearly scans capabilities per signal submission; unbounded plan-controlled gas)",
            capabilities.len(),
            MAX_SIGNAL_CAPABILITIES
        )));
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

    /// 任意 name 形态的未发布目标占位（link 不可达目标，仅形态合法）。
    const UNKNOWN_TARGET: &str = "unpublished-zhixu";

    // ------------------------------------------------------------------
    // 目标示例（payment_execution）：两个具名接口
    // payment_service[new] 与 payment_evidence[existing]。
    // ------------------------------------------------------------------
    fn target_payment_definition() -> Value {
        json!({
            "apiVersion": "uvp/v0",
            "kind": "Zhixu",
            "metadata": { "name": "payment_execution" },
            "spec": {
                "platform": { "type": "cloud" },
                "nucleation": { "id": "payment-core" },
                "dockInterface": {
                    "payment_service": {
                        "orderModes": ["new"],
                        "inputs": {
                            "execute": { "hook": "payment_flow.init#DOCK_EXECUTE" },
                            "cancel": { "hook": "payment_flow.control#DOCK_CANCEL" }
                        },
                        "outputs": {
                            "started": { "signal": "payment::payment_flow.init.str" },
                            "completed": { "signal": "payment::payment_flow.settle.cmp" },
                            "failed": { "signal": "payment::payment_flow.settle.err" }
                        }
                    },
                    "payment_evidence": {
                        "orderModes": ["existing"],
                        "outputs": {
                            "cancelled": { "signal": "payment::payment_flow.control.cxl" }
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

    const TARGET_NAME: &str = "payment_execution";

    // ------------------------------------------------------------------
    // 调用方示例（settlement）：new 模式静态指定生产委托。
    // ------------------------------------------------------------------
    fn parent_settlement_definition(target_name: &str) -> Value {
        json!({
            "apiVersion": "uvp/v0",
            "kind": "Zhixu",
            "metadata": { "name": "settlement" },
            "spec": {
                "platform": { "type": "cloud" },
                "nucleation": { "id": "settlement-core" },
                "taskPatterns": [
                    { "name": "checkout", "stages": [
                        {
                            "name": "confirm",
                            "source": "buyer",
                            // P0-4 物化门：零 hook 阶段在链上永不可物化、信号
                            // 没有钩子可挂；seed 是执行者自发入口信号。
                            "receiveSignals": { "PLACE": "buyer::checkout.confirm.seed" },
                            "sendSignals": ["cmp", "seed"],
                            "executor": { "supplierType": "organization", "supplierID": "buyer-app" }
                        },
                        {
                            "name": "cancel",
                            "source": "buyer",
                            "receiveSignals": { "ABORT": "buyer::checkout.cancel.seed" },
                            "sendSignals": ["cmp", "seed"],
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
                                    "target": { "zhixu": target_name },
                                    "interface": "payment_service",
                                    "order": { "mode": "new" },
                                    "inputMap": { "EXECUTE": "execute" },
                                    "signalMap": { "str": "started", "cmp": "completed", "err": "failed" }
                                }
                            }
                        }
                    ]}
                ]
            }
        })
    }

    // ------------------------------------------------------------------
    // 调用方示例（recycling）：existing 模式动态引用已有事实。
    // ------------------------------------------------------------------
    fn parent_recycling_definition(target_name: &str) -> Value {
        json!({
            "apiVersion": "uvp/v0",
            "kind": "Zhixu",
            "metadata": { "name": "recycling" },
            "spec": {
                "platform": { "type": "cloud" },
                "nucleation": { "id": "recycling-core" },
                "taskPatterns": [
                    { "name": "recycling", "stages": [
                        {
                            "name": "source_evidence",
                            "source": "recycler",
                            "receiveSignals": { "READ": "recycler::recycling.source_evidence.seed" },
                            "sendSignals": ["cmp", "seed"],
                            "executor": {
                                "supplierType": "zhixu",
                                "zhixuExecutorConfig": {
                                    "target": { "zhixu": target_name },
                                    "interface": "payment_evidence",
                                    "order": { "mode": "existing" },
                                    "signalMap": { "cmp": "cancelled" }
                                }
                            }
                        }
                    ]}
                ]
            }
        })
    }

    /// 构造 resolution manifest：真实流程由 Store/发布系统在目标发布后
    /// 生成；测试中直接编译目标定义取其中性接口声明。
    fn manifest_entry(target: &Value) -> Value {
        let plan = compile_zhixu_hook_plan(target, None, true).expect("target compiles");
        json!({
            "name": plan["zhixuName"].clone(),
            "interfaces": plan["dockInterface"].clone(),
        })
    }

    fn manifest_for(target: &Value) -> Value {
        json!({
            "schemaVersion": dock::DOCK_RESOLUTION_SCHEMA_VERSION,
            "definitions": [manifest_entry(target)]
        })
    }

    #[test]
    fn send_signals_total_is_capped_at_256() {
        // G-18 镜像：hook_plan 与 cloud 共用 build_signal_capabilities，
        // 两侧同值同文案；256 条放行、257 条拒绝。
        let definition_with = |count: usize| {
            let signals: Vec<String> = (0..count).map(|index| format!("sig{index:03}")).collect();
            json!({
                "apiVersion": "uvp/v0",
                "kind": "Zhixu",
                "metadata": { "name": "capability_cap" },
                "spec": {
                    "platform": { "type": "cloud" },
                    "nucleation": { "id": "cap-core" },
                    "taskPatterns": [
                        { "name": "main", "stages": [
                            {
                                "name": "work",
                                "source": "buyer",
                                // P0-4：零 hook 阶段不过物化门，给一条自发
                                // 种子入口钩子（能力计数不受影响）。
                                "receiveSignals": { "START": "buyer::main.work.sig000" },
                                "sendSignals": signals,
                                "executor": { "supplierType": "organization", "supplierID": "buyer-app" }
                            }
                        ]}
                    ]
                }
            })
        };
        let at_limit =
            compile_zhixu_hook_plan(&definition_with(MAX_SIGNAL_CAPABILITIES), None, true)
                .expect("256 capabilities compile");
        assert_eq!(
            at_limit["signalCapabilities"].as_array().map(Vec::len),
            Some(MAX_SIGNAL_CAPABILITIES)
        );
        let over_limit =
            compile_zhixu_hook_plan(&definition_with(MAX_SIGNAL_CAPABILITIES + 1), None, true)
                .unwrap_err();
        let CompilerError::Issues(message) = &over_limit else {
            panic!("expected issues error, got {over_limit:?}");
        };
        assert!(
            message.contains("signal capabilities 257 exceed the documented limit 256"),
            "message: {message}"
        );
    }

    #[test]
    fn resolved_routes_carry_the_neutral_name_keyed_shape() {
        // 壳上无身份：route 只携带本地声明与目标 name/接口名/端口绑定，
        // 不携带任何派生字段（哈希承诺由各轨在此形状上自行计算）。
        let target = target_payment_definition();
        let manifest = manifest_for(&target);
        let plan = compile_zhixu_hook_plan(
            &parent_settlement_definition(TARGET_NAME),
            Some(&manifest),
            false,
        )
        .expect("new-mode route links");
        let route = &plan["dockRoutes"][0];
        assert_eq!(route["schemaVersion"], "uvp.dockRoute.v2");
        assert_eq!(
            route["local"]["stageIdentifier"],
            "settlement.execute_payment"
        );
        assert_eq!(route["target"]["name"], json!(TARGET_NAME));
        assert_eq!(route["target"]["interfaceName"], json!("payment_service"));
        assert_eq!(route["orderMode"], "new");
        assert_eq!(
            route["inputBindings"],
            json!([{ "hookId": "settlement.execute_payment#EXECUTE", "port": "execute" }])
        );
        assert_eq!(
            route["outputBindings"],
            json!([
                { "signal": "cmp", "port": "completed" },
                { "signal": "err", "port": "failed" },
                { "signal": "str", "port": "started" }
            ])
        );
        for absent in [
            "routeId",
            "routeHash",
            "sourceSeam",
            "inputBindingsRoot",
            "outputBindingsRoot",
            "localDefinitionRefHash",
            "stageKey",
        ] {
            assert!(
                route.get(absent).is_none()
                    && route["local"].get(absent).is_none()
                    && route["target"].get(absent).is_none(),
                "resolved route must not carry {absent}"
            );
        }
        assert!(
            route["target"].get("zhixuUid").is_none()
                && route["target"].get("definitionRefHash").is_none(),
            "route target must not carry derived identity fields"
        );

        // existing/output-only route 同一中性形状。
        let recycling = compile_cloud_artifact(
            &parent_recycling_definition(TARGET_NAME),
            Some(&manifest),
            false,
        )
        .expect("existing-mode output-only route links");
        let recycling_route = &recycling["dockRoutes"][0];
        assert_eq!(recycling_route["orderMode"], "existing");
        assert_eq!(
            recycling_route["target"]["interfaceName"],
            json!("payment_evidence")
        );
        assert_eq!(
            recycling_route["inputBindings"].as_array().map(Vec::len),
            Some(0)
        );
        assert_eq!(
            recycling_route["outputBindings"],
            json!([{ "signal": "cmp", "port": "cancelled" }])
        );
    }

    #[test]
    fn link_reports_every_routes_issues_in_one_pass() {
        // issues 按 route 独立收集：任意 route 的失败不得吞掉其他 route 的
        // 报错——错误一次报全，调用方不需要逐个修复再重编来发现下一个。
        let target = target_payment_definition();
        let mut parent = parent_settlement_definition(TARGET_NAME);
        let second_dock = json!({
            "name": "second_dock",
            "source": "buyer",
            "receiveSignals": { "START": "buyer::checkout.confirm.cmp" },
            "sendSignals": ["str", "cmp", "err"],
            "executor": {
                "supplierType": "zhixu",
                "zhixuExecutorConfig": {
                    "target": { "zhixu": UNKNOWN_TARGET },
                    "interface": "payment_service",
                    "order": { "mode": "new" },
                    "inputMap": { "START": "execute" },
                    "signalMap": { "str": "started", "cmp": "completed" }
                }
            }
        });
        parent["spec"]["taskPatterns"][1]["stages"]
            .as_array_mut()
            .unwrap()
            .push(second_dock);
        // 同一 manifest：settlement 的目标存在，second_dock 的目标缺失。
        let manifest = manifest_for(&target);
        let error = compile_zhixu_hook_plan(&parent, Some(&manifest), false)
            .expect_err("unresolvable second route must fail");
        let message = error.to_string();
        assert!(
            message.contains("D008")
                && message.contains("settlement.second_dock")
                && message.contains(UNKNOWN_TARGET),
            "failing route must be reported: {message}"
        );
    }

    #[test]
    fn compiles_linked_parent_with_dock_route() {
        let target = target_payment_definition();
        let parent = parent_settlement_definition(TARGET_NAME);
        let manifest = manifest_for(&target);
        let plan = compile_zhixu_hook_plan(&parent, Some(&manifest), false)
            .expect("linked parent compiles");
        assert_eq!(plan["schemaVersion"], "uvp.hookPlan.v2");
        assert_eq!(plan["zhixuName"], json!("settlement"));

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

        // route：resolved，new 模式唯一 input 绑定 EXECUTE→execute。
        let routes = plan["dockRoutes"].as_array().unwrap();
        assert_eq!(routes.len(), 1);
        let route = &routes[0];
        assert_eq!(route["schemaVersion"], "uvp.dockRoute.v2");
        assert_eq!(
            route["local"]["stageIdentifier"],
            "settlement.execute_payment"
        );
        assert_eq!(route["orderMode"], "new");
        assert_eq!(route["target"]["name"], json!(TARGET_NAME));
        assert_eq!(route["target"]["interfaceName"], json!("payment_service"));
        assert!(route.get("orderIdPolicy").is_none());
        assert_eq!(route["inputBindings"].as_array().unwrap().len(), 1);
        assert_eq!(
            route["inputBindings"][0]["hookId"],
            "settlement.execute_payment#EXECUTE"
        );
        assert_eq!(route["inputBindings"][0]["port"], "execute");
        assert_eq!(route["outputBindings"].as_array().unwrap().len(), 3);
        // zhixu stage 不在静态 executorRoutes 中（权威形态是 dockRoutes）。
        assert!(plan["executorRoutes"]
            .as_object()
            .unwrap()
            .get("settlement.execute_payment")
            .is_none());
    }

    #[test]
    fn target_compiles_named_interfaces_and_dock_trigger_flags() {
        let plan = compile_zhixu_hook_plan(&target_payment_definition(), None, true)
            .expect("target compiles standalone");
        let interface = &plan["dockInterface"];
        let interfaces = interface.as_array().unwrap();
        assert_eq!(interfaces.len(), 2);
        // 接口按名排序。
        assert_eq!(interfaces[0]["name"], json!("payment_evidence"));
        assert_eq!(interfaces[1]["name"], json!("payment_service"));
        assert_eq!(interfaces[0]["orderModes"], json!(["existing"]));
        assert_eq!(interfaces[1]["orderModes"], json!(["new"]));
        // 中性声明：inputs 是端口→{source, hook}，outputs 是端口→{signal}
        // 原文（source 是 input 侧的单源 seam 观测面，bug_audit #1）。
        assert_eq!(
            interfaces[1]["inputs"],
            json!({
                "cancel": { "source": "payment", "hook": "payment_flow.control#DOCK_CANCEL" },
                "execute": { "source": "payment", "hook": "payment_flow.init#DOCK_EXECUTE" }
            })
        );
        assert_eq!(
            interfaces[1]["outputs"],
            json!({
                "completed": { "signal": "payment::payment_flow.settle.cmp" },
                "failed": { "signal": "payment::payment_flow.settle.err" },
                "started": { "signal": "payment::payment_flow.init.str" }
            })
        );
        assert!(
            interfaces[0].get("interfaceRoot").is_none()
                && interfaces[0].get("inputsRoot").is_none()
                && interface.get("interfaceRoot").is_none(),
            "neutral interface declaration must not carry roots"
        );

        let hooks = plan["compiledHooks"].as_array().unwrap();
        // payment_service 支持 new：其全部 input 端口都是出生锚候选。
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
        assert_eq!(cancel["orderTriggerKind"], "dock");
        assert_eq!(cancel["emitReady"], true);
    }

    #[test]
    fn rejects_parent_without_manifest() {
        let error = compile_zhixu_hook_plan(&parent_settlement_definition(TARGET_NAME), None, false)
            .expect_err("unresolved dock target must fail");
        assert!(
            error.to_string().contains("UNRESOLVED_DOCK_TARGET"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn parse_target_allows_unresolved() {
        let value =
            compile_zhixu_hook_plan(&parent_settlement_definition(TARGET_NAME), None, true)
                .expect("parse target allows unresolved routes");
        assert_eq!(value["dockRoutes"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn rejects_unsupported_executor_config_shapes() {
        // triggerEntrance：不受支持的调用方字段，D002 未知字段硬错误。
        let mut parent = parent_settlement_definition(TARGET_NAME);
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

        // schemaVersion 残留键：作者面没有 schemaVersion，按未知字段硬拒绝。
        let mut parent = parent_settlement_definition(TARGET_NAME);
        parent["spec"]["taskPatterns"][1]["stages"][0]["executor"]["zhixuExecutorConfig"]
            .as_object_mut()
            .unwrap()
            .insert("schemaVersion".to_string(), json!("uvp.dock.v1"));
        let error = compile_zhixu_hook_plan(&parent, None, false)
            .expect_err("leftover schemaVersion must hard-fail");
        let message = error.to_string();
        assert!(
            message.contains("D002") && message.contains("schemaVersion"),
            "{message}"
        );

        // signalMap value 是 Hook DSL：只接受端口名。
        let mut parent = parent_settlement_definition(TARGET_NAME);
        parent["spec"]["taskPatterns"][1]["stages"][0]["executor"]["zhixuExecutorConfig"]
            .as_object_mut()
            .unwrap()["signalMap"]["str"] = json!("payment::payment_flow.init.str");
        let error = compile_zhixu_hook_plan(&parent, None, false)
            .expect_err("hook-DSL signalMap value must hard-fail");
        assert!(error.to_string().contains("D006"), "{}", error.to_string());

        // supplierID + zhixu：D001。
        let mut parent = parent_settlement_definition(TARGET_NAME);
        parent["spec"]["taskPatterns"][1]["stages"][0]["executor"]["supplierID"] =
            json!("payment-zhixu");
        let error = compile_zhixu_hook_plan(&parent, None, false)
            .expect_err("supplierID on zhixu executor must fail");
        assert!(error.to_string().contains("D001"), "{}", error.to_string());
    }

    #[test]
    fn rejects_signal_map_keys_outside_hook_name_budget() {
        // D006：key 超 26 字节（hook_name = "signalMap." + key 落 VARCHAR(36)）。
        let mut parent = parent_settlement_definition(TARGET_NAME);
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
        let mut parent = parent_settlement_definition(TARGET_NAME);
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
        let mut parent = parent_settlement_definition(TARGET_NAME);
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
    fn rejects_leftover_target_version_field() {
        // target 只携带 zhixu（name 即完整目标引用）。残留 version 键按
        // D002 未知字段硬拒绝——静默忽略会让调用方误以为版本钉扎仍生效。
        let mut parent = parent_settlement_definition(TARGET_NAME);
        parent["spec"]["taskPatterns"][1]["stages"][0]["executor"]["zhixuExecutorConfig"]
            .as_object_mut()
            .unwrap()["target"]["version"] = json!("1.2.0");
        let error = compile_zhixu_hook_plan(&parent, None, false)
            .expect_err("leftover target.version must be rejected");
        let message = error.to_string();
        assert!(
            message.contains("D002") && message.contains("target.version"),
            "{message}"
        );
        assert!(
            message.contains("unknown field"),
            "must be reported as an unknown field: {message}"
        );
    }

    #[test]
    fn manifest_name_is_the_resolution_key() {
        // name 是 linker 的唯一解析键：manifest 缺该 name 即 D008；
        // manifest 内重名是发布方数据错误，响亮拒绝。
        let target = target_payment_definition();
        let manifest = manifest_for(&target);
        let plan = compile_zhixu_hook_plan(
            &parent_settlement_definition(TARGET_NAME),
            Some(&manifest),
            false,
        )
        .expect("honest manifest links");
        assert_eq!(plan["dockRoutes"][0]["target"]["name"], json!(TARGET_NAME));

        let mut renamed = manifest_for(&target);
        renamed["definitions"][0]["name"] = json!(UNKNOWN_TARGET);
        let error = compile_zhixu_hook_plan(
            &parent_settlement_definition(TARGET_NAME),
            Some(&renamed),
            false,
        )
        .expect_err("manifest without the referenced name must fail");
        let message = error.to_string();
        assert!(
            message.contains("D008")
                && message.contains("no definition named")
                && message.contains(TARGET_NAME),
            "{message}"
        );

        let mut duplicated = manifest_for(&target);
        let entry = duplicated["definitions"][0].clone();
        duplicated["definitions"]
            .as_array_mut()
            .unwrap()
            .push(entry);
        let error = dock::parse_resolution_manifest(&duplicated)
            .expect_err("duplicate manifest names must fail");
        assert!(
            error
                .iter()
                .any(|issue| issue.code == "D008" && issue.message.contains("duplicate")),
            "{error:?}"
        );
    }

    /// 无锚扇入订阅 + zhixu 委托执行器（UVP-01）：编译期拒绝；本类存在
    /// mint 声明（有锚，route=order）时放行。
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
                        "target": { "zhixu": UNKNOWN_TARGET },
                        "interface": "payment_service",
                        "order": { "mode": "new" },
                        "inputMap": { "SUB": "execute" },
                        "signalMap": { "str": "started", "cmp": "completed" }
                    }
                }
            }),
            // 订阅目标 source 类必须在本域声明（引用存在性校验）。
            json!({
                "name": "emit",
                "source": "other",
                // P0-4：自发种子入口钩子，避免零 hook 阶段被物化门拒绝。
                "receiveSignals": { "PUBLISH": "other::anchor_task.emit.seed" },
                "sendSignals": ["cmp", "seed"],
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
            "metadata": { "name": "delegation_subscription" },
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
            ("oversized", "s".repeat(101)),
        ] {
            let mut parent = parent_settlement_definition(TARGET_NAME);
            parent["spec"]["taskPatterns"][1]["stages"][0]["source"] = json!(source);
            let error = compile_zhixu_hook_plan(&parent, None, false)
                .expect_err("invalid stage source must be rejected");
            assert!(
                error.to_string().contains(".source"),
                "source {label:?}: {error}"
            );
        }
        // 100 字节边界恰好放行（DSL 壳字段统一 100，bug_audit #14）。
        let mut parent = parent_settlement_definition(TARGET_NAME);
        parent["spec"]["taskPatterns"][1]["stages"][0]["source"] = json!("s".repeat(100));
        let error = compile_zhixu_hook_plan(&parent, None, false)
            .expect_err("boundary source must pass shape checks and fail later on linking");
        assert!(
            !error.to_string().contains("exceeds 100 bytes")
                && !error.to_string().contains("exceeds 36 bytes"),
            "100-byte source is legal: {error}"
        );
    }

    #[test]
    fn rejects_unknown_spec_and_executor_fields() {
        // spec 顶层未知字段（含不受支持的 trigger/externalSignals）不被静默
        // 忽略/透传（对齐 Go 入口 decodeObjectStrict）。
        let mut parent = parent_settlement_definition(TARGET_NAME);
        parent["spec"]["trigger"] = json!([]);
        let error = compile_zhixu_hook_plan(&parent, None, false)
            .expect_err("unsupported spec-level trigger key must fail");
        assert!(
            error.to_string().contains("unknown field `trigger`"),
            "{error}"
        );

        let mut parent = parent_settlement_definition(TARGET_NAME);
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
        let mut parent = parent_settlement_definition(TARGET_NAME);
        parent["spec"]["taskPatterns"][1]["stages"][0]["executor"]["handlerType"] = json!("http");
        let error = compile_zhixu_hook_plan(&parent, None, false)
            .expect_err("unknown executor field must fail");
        assert!(
            error.to_string().contains("unknown field `handlerType`"),
            "{error}"
        );

        // metadata 层未知字段（如 description）同样拒绝。
        let mut parent = parent_settlement_definition(TARGET_NAME);
        parent["metadata"]["description"] = json!("demo");
        let error = compile_zhixu_hook_plan(&parent, None, false)
            .expect_err("metadata-level unknown field must fail");
        assert!(
            error.to_string().contains("unknown field `description`"),
            "{error}"
        );

        // metadata.uid 不是作者可写字段，出现即未知字段响亮拒绝。
        let mut parent = parent_settlement_definition(TARGET_NAME);
        parent["metadata"]["uid"] = json!("zx-hand-written");
        let error = compile_zhixu_hook_plan(&parent, None, false)
            .expect_err("metadata.uid must be rejected as an unknown field");
        assert!(error.to_string().contains("unknown field `uid`"), "{error}");
    }

    #[test]
    fn rejects_interface_shape_violations() {
        // 组合表达式的 input port hook。
        let mut target = target_payment_definition();
        target["spec"]["taskPatterns"][0]["stages"][0]["receiveSignals"]["DOCK_EXECUTE"] =
            json!("payment::payment_flow.init.execute & payment::payment_flow.control.cxl");
        let error = compile_zhixu_hook_plan(&target, None, true)
            .expect_err("composite input port hook must fail");
        assert!(error.to_string().contains("D013"), "{}", error.to_string());

        // 同 atom 的组合式（A&A / A|A）：依赖去重后只剩一项，计数判定会被
        // 伪装成"单 atom"——判定必须看去重前的语法结构。
        for expression in [
            "payment::payment_flow.init.execute & payment::payment_flow.init.execute",
            "payment::payment_flow.init.execute | payment::payment_flow.init.execute",
        ] {
            let mut target = target_payment_definition();
            target["spec"]["taskPatterns"][0]["stages"][0]["receiveSignals"]["DOCK_EXECUTE"] =
                json!(expression);
            let error = compile_zhixu_hook_plan(&target, None, true)
                .expect_err("same-atom composition must fail");
            assert!(
                error.to_string().contains("D013"),
                "{expression}: {}",
                error.to_string()
            );
        }

        // input atom 的 (task, stage) 必须落在所属 stage 上：mailbox 地址
        // 指向别处（同 source 的另一 stage）同样是 D013。
        let mut target = target_payment_definition();
        target["spec"]["taskPatterns"][0]["stages"][0]["receiveSignals"]["DOCK_EXECUTE"] =
            json!("payment::payment_flow.control.cxl");
        let error = compile_zhixu_hook_plan(&target, None, true)
            .expect_err("input atom addressing another stage must fail");
        assert!(
            error.to_string().contains("D013")
                && error.to_string().contains("must address the owning stage"),
            "{}",
            error.to_string()
        );

        // 输出端口引用非 sendSignals 信号。
        let mut target = target_payment_definition();
        target["spec"]["dockInterface"]["payment_service"]["outputs"]["started"]["signal"] =
            json!("payment::payment_flow.init.nope");
        let error = compile_zhixu_hook_plan(&target, None, true)
            .expect_err("unknown output signal must fail");
        assert!(error.to_string().contains("D014"), "{}", error.to_string());

        // 非法端口名。
        let mut target = target_payment_definition();
        let inputs = target["spec"]["dockInterface"]["payment_service"]["inputs"]
            .as_object_mut()
            .unwrap();
        let execute = inputs.remove("execute").unwrap();
        inputs.insert("BadPort".to_string(), execute);
        let error =
            compile_zhixu_hook_plan(&target, None, true).expect_err("invalid port name must fail");
        assert!(error.to_string().contains("D021"), "{}", error.to_string());

        // 非法接口名（与端口名同规则）。
        let mut target = target_payment_definition();
        let dock = target["spec"]["dockInterface"].as_object_mut().unwrap();
        let service = dock.remove("payment_service").unwrap();
        dock.insert("PaymentService".to_string(), service);
        let error = compile_zhixu_hook_plan(&target, None, true)
            .expect_err("invalid interface name must fail");
        assert!(
            error.to_string().contains("D021")
                && error.to_string().contains("interface name must match"),
            "{}",
            error.to_string()
        );

        // orderModes：空集 / 重复 / 未知取值 / new 无 input 端口。
        for (label, modes) in [
            ("empty", json!([])),
            ("duplicate", json!(["new", "new"])),
            ("unknown", json!(["reused"])),
        ] {
            let mut target = target_payment_definition();
            target["spec"]["dockInterface"]["payment_service"]["orderModes"] = modes;
            let error = compile_zhixu_hook_plan(&target, None, true)
                .expect_err("orderModes {label} must fail");
            assert!(
                error.to_string().contains("D025"),
                "orderModes {label}: {error}"
            );
        }
        let mut target = target_payment_definition();
        target["spec"]["dockInterface"]["payment_service"]["inputs"] = json!({});
        let error = compile_zhixu_hook_plan(&target, None, true)
            .expect_err("new-mode interface without inputs must fail");
        assert!(
            error.to_string().contains("D025")
                && error.to_string().contains("at least one input port"),
            "{}",
            error.to_string()
        );
    }

    #[test]
    fn rejects_config_mapping_violations() {
        // D004：order.mode 闭集。
        let mut parent = parent_settlement_definition(TARGET_NAME);
        parent["spec"]["taskPatterns"][1]["stages"][0]["executor"]["zhixuExecutorConfig"]
            .as_object_mut()
            .unwrap()["order"]["mode"] = json!("reused");
        let error = compile_zhixu_hook_plan(&parent, None, false)
            .expect_err("unknown order mode must fail");
        assert!(
            error.to_string().contains("D004")
                && error
                    .to_string()
                    .contains("must be \"new\" or \"existing\""),
            "{}",
            error.to_string()
        );

        // D001：supplierID 键出现即违规——空串是"看似生效"的零值占位，
        // 不因空值豁免（目标身份必须住在 zhixuExecutorConfig.target）。
        let mut parent = parent_settlement_definition(TARGET_NAME);
        parent["spec"]["taskPatterns"][1]["stages"][0]["executor"]["supplierID"] = json!("  ");
        let error = compile_zhixu_hook_plan(&parent, None, false)
            .expect_err("empty supplierID on a zhixu executor must fail");
        assert!(
            error.to_string().contains("D001")
                && error.to_string().contains("supplierID is forbidden"),
            "{}",
            error.to_string()
        );

        // D019：无任何映射。
        let mut parent = parent_settlement_definition(TARGET_NAME);
        let config = parent["spec"]["taskPatterns"][1]["stages"][0]["executor"]
            ["zhixuExecutorConfig"]
            .as_object_mut()
            .unwrap();
        config.remove("inputMap");
        config.remove("signalMap");
        let error =
            compile_zhixu_hook_plan(&parent, None, false).expect_err("empty mappings must fail");
        assert!(
            error.to_string().contains("D019")
                && error
                    .to_string()
                    .contains("at least one of inputMap/signalMap"),
            "{}",
            error.to_string()
        );

        // D010：new 模式多条 input 绑定。
        let mut parent = parent_settlement_definition(TARGET_NAME);
        parent["spec"]["taskPatterns"][1]["stages"][0]["executor"]["zhixuExecutorConfig"]
            .as_object_mut()
            .unwrap()["inputMap"]["CANCEL"] = json!("cancel");
        let error = compile_zhixu_hook_plan(&parent, None, false)
            .expect_err("new mode with two input bindings must fail");
        assert!(
            error.to_string().contains("D010")
                && error.to_string().contains("exactly one inputMap binding"),
            "{}",
            error.to_string()
        );

        // D010：new 模式零条 input 绑定（只有 signalMap）。
        let mut parent = parent_settlement_definition(TARGET_NAME);
        parent["spec"]["taskPatterns"][1]["stages"][0]["executor"]["zhixuExecutorConfig"]
            .as_object_mut()
            .unwrap()
            .remove("inputMap");
        let error = compile_zhixu_hook_plan(&parent, None, false)
            .expect_err("new mode without an input binding must fail");
        assert!(error.to_string().contains("D010"), "{}", error.to_string());

        // D003：target.zhixu 非 name slug 形态。
        let mut parent = parent_settlement_definition(TARGET_NAME);
        parent["spec"]["taskPatterns"][1]["stages"][0]["executor"]["zhixuExecutorConfig"]
            .as_object_mut()
            .unwrap()["target"]["zhixu"] = json!("Payment_Execution");
        let error = compile_zhixu_hook_plan(&parent, None, false)
            .expect_err("non-slug target name must fail");
        assert!(
            error.to_string().contains("D003")
                && error.to_string().contains("metadata.name"),
            "{}",
            error.to_string()
        );
    }

    #[test]
    fn accepts_dynamic_target_null_for_parse_only_compilation() {
        // target:null 表示运行时选择补齐：本地校验通过，
        // 无 manifest 的 parse 编译可过；静态目标缺失 manifest 才是
        // UNRESOLVED_DOCK_TARGET（见 rejects_parent_without_manifest）。
        let mut parent = parent_settlement_definition(TARGET_NAME);
        parent["spec"]["taskPatterns"][1]["stages"][0]["executor"]["zhixuExecutorConfig"]
            .as_object_mut()
            .unwrap()["target"] = json!(null);
        let parsed = compile_zhixu_hook_plan(&parent, None, true)
            .expect("parse-only compilation accepts a null target");
        assert_eq!(parsed["dockRoutes"].as_array().unwrap().len(), 0);
    }

    /// target:null 的父定义（无 manifest）：本地声明面完整进产物，两个
    /// 可运行 target 都放行——链轨拒绝在 TS onchain 边界（UNRESOLVED_DOCK_
    /// TARGET 口径），云轨运行时由选择记录补齐。
    fn null_target_parent() -> Value {
        let mut parent = parent_settlement_definition(TARGET_NAME);
        parent["spec"]["taskPatterns"][1]["stages"][0]["executor"]["zhixuExecutorConfig"]
            .as_object_mut()
            .unwrap()["target"] = json!(null);
        parent
    }

    #[test]
    fn dynamic_target_null_lands_in_unresolved_dock_routes() {
        let parent = null_target_parent();
        let artifact =
            compile_cloud_artifact(&parent, None, false).expect("cloud compiles a null target");
        assert_eq!(artifact["dockRoutes"].as_array().unwrap().len(), 0);
        let unresolved = artifact["unresolvedDockRoutes"].as_array().unwrap();
        assert_eq!(unresolved.len(), 1);
        let route = &unresolved[0];
        assert_eq!(route["schemaVersion"], "uvp.dockRoute.unresolved.v1");
        assert_eq!(route["stageIdentifier"], "settlement.execute_payment");
        assert_eq!(route["localSource"], "buyer");
        assert_eq!(route["interfaceName"], "payment_service");
        assert_eq!(route["orderMode"], "new");
        assert_eq!(
            route["inputBindings"],
            json!([{ "hookId": "settlement.execute_payment#EXECUTE", "port": "execute" }])
        );
        assert_eq!(
            route["outputBindings"],
            json!([
                { "signal": "cmp", "port": "completed" },
                { "signal": "err", "port": "failed" },
                { "signal": "str", "port": "started" }
            ])
        );
        // 未解析元素没有任何派生字段与目标引用。
        for absent in [
            "routeId",
            "routeHash",
            "target",
            "sourceSeam",
            "stageId",
            "localDefinitionRefHash",
            "localPlanId",
        ] {
            assert!(
                route.get(absent).is_none(),
                "unresolved route must not carry {absent}"
            );
        }

        // hook_plan 同口径携带（链轨拒绝由 TS onchain 边界承担）。
        let plan = compile_zhixu_hook_plan(&parent, None, false)
            .expect("hook_plan compiles a null target");
        assert_eq!(
            plan["unresolvedDockRoutes"].as_array().unwrap().len(),
            1,
            "hook plan artifact carries the unresolved declaration face"
        );
        assert_eq!(plan["dockRoutes"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn unresolved_routes_absent_when_all_targets_static() {
        // 无未解析 route 的产物不落字段。
        let target = target_payment_definition();
        let manifest = manifest_for(&target);
        let artifact = compile_cloud_artifact(
            &parent_settlement_definition(TARGET_NAME),
            Some(&manifest),
            false,
        )
        .expect("static-only parent compiles");
        assert!(artifact.get("unresolvedDockRoutes").is_none());
    }

    #[test]
    fn manifest_present_null_target_route_stays_unresolved() {
        // manifest 在场时 null-target route 不进 link（不报 D008），静态
        // route 照常解析：两类 route 各归其位。
        let target = target_payment_definition();
        let manifest = manifest_for(&target);
        let mut parent = null_target_parent();
        parent["spec"]["taskPatterns"][1]["stages"]
            .as_array_mut()
            .unwrap()
            .push(json!({
                "name": "static_dock",
                "source": "buyer",
                "receiveSignals": { "START": "buyer::checkout.confirm.cmp" },
                "sendSignals": ["str"],
                "executor": {
                    "supplierType": "zhixu",
                    "zhixuExecutorConfig": {
                        "target": { "zhixu": TARGET_NAME },
                        "interface": "payment_service",
                        "order": { "mode": "new" },
                        "inputMap": { "START": "execute" },
                        "signalMap": { "str": "started" }
                    }
                }
            }));
        let artifact = compile_cloud_artifact(&parent, Some(&manifest), false)
            .expect("mixed static/dynamic parent compiles");
        assert_eq!(artifact["dockRoutes"].as_array().unwrap().len(), 1);
        assert_eq!(
            artifact["dockRoutes"][0]["local"]["stageIdentifier"],
            "settlement.static_dock"
        );
        let unresolved = artifact["unresolvedDockRoutes"].as_array().unwrap();
        assert_eq!(unresolved.len(), 1);
        assert_eq!(
            unresolved[0]["stageIdentifier"],
            "settlement.execute_payment"
        );
    }

    #[test]
    fn dynamic_target_null_still_enforces_local_config_validation() {
        // 本地校验不依赖目标：D010 在 target:null 上同样拒绝（两 input 绑定）。
        let mut parent = null_target_parent();
        parent["spec"]["taskPatterns"][1]["stages"][0]["executor"]["zhixuExecutorConfig"]
            .as_object_mut()
            .unwrap()["inputMap"]["CANCEL"] = json!("cancel");
        let error = compile_cloud_artifact(&parent, None, false)
            .expect_err("new mode with two input bindings must fail without a target");
        assert!(
            error.to_string().contains("D010")
                && error.to_string().contains("exactly one inputMap binding"),
            "{}",
            error
        );

        // D019：无任何映射。
        let mut parent = null_target_parent();
        let config = parent["spec"]["taskPatterns"][1]["stages"][0]["executor"]
            ["zhixuExecutorConfig"]
            .as_object_mut()
            .unwrap();
        config.remove("inputMap");
        config.remove("signalMap");
        let error = compile_cloud_artifact(&parent, None, false)
            .expect_err("empty mappings must fail without a target");
        assert!(error.to_string().contains("D019"), "{}", error);
    }

    #[test]
    fn unresolved_routes_enforce_d016_binding_caps() {
        // D016 上限必须在声明面（parse 期）钉死：target:null 的 route 不进
        // link，上限若只放在 link 期会被未解析 route 绕过。
        let channels = (0..9).map(|index| format!("CH{index}")).collect::<Vec<_>>();
        let receive_signals: Map<String, Value> = channels
            .iter()
            .map(|channel| {
                (
                    channel.clone(),
                    Value::String("buyer::main.work.seed".to_string()),
                )
            })
            .collect();
        let input_map: Map<String, Value> = channels
            .iter()
            .enumerate()
            .map(|(index, channel)| {
                (
                    channel.clone(),
                    Value::String(format!("p{index}")),
                )
            })
            .collect();
        let parent = json!({
            "apiVersion": "uvp/v0",
            "kind": "Zhixu",
            "metadata": { "name": "cap_parent" },
            "spec": {
                "platform": { "type": "cloud" },
                "nucleation": { "id": "cap-core" },
                "taskPatterns": [
                    { "name": "main", "stages": [
                        {
                            "name": "work",
                            "source": "buyer",
                            "receiveSignals": { "START": "buyer::main.work.seed" },
                            "sendSignals": ["seed"],
                            "executor": { "supplierType": "organization", "supplierID": "buyer-app" }
                        },
                        { "name": "dock", "source": "buyer",
                          "receiveSignals": receive_signals,
                          "sendSignals": ["out"],
                          "executor": {
                            "supplierType": "zhixu",
                            "zhixuExecutorConfig": {
                                "target": null,
                                "interface": "bulk_service",
                                "order": { "mode": "existing" },
                                "inputMap": input_map,
                                "signalMap": { "out": "done" }
                            }
                        }}
                    ]}
                ]
            }
        });
        let error = compile_cloud_artifact(&parent, None, false)
            .expect_err("nine input bindings must fail the D016 cap");
        assert!(
            error.to_string().contains("D016")
                && error.to_string().contains("9 input ports, limit is 8"),
            "{}",
            error
        );

        // 上限内（8 条）照常进未解析清单。
        let mut parent = parent;
        let config = parent["spec"]["taskPatterns"][0]["stages"][1]["executor"]
            ["zhixuExecutorConfig"]
            .as_object_mut()
            .unwrap();
        config["inputMap"]
            .as_object_mut()
            .unwrap()
            .remove("CH8");
        parent["spec"]["taskPatterns"][0]["stages"][1]["receiveSignals"]
            .as_object_mut()
            .unwrap()
            .remove("CH8");
        let artifact = compile_cloud_artifact(&parent, None, false)
            .expect("eight input bindings compile as an unresolved route");
        assert_eq!(
            artifact["unresolvedDockRoutes"][0]["inputBindings"]
                .as_array()
                .map(Vec::len),
            Some(8)
        );
    }

    #[test]
    fn rejects_unknown_executor_supplier_types() {
        // supplierType 闭集 {individual, organization, zhixu}：拼错的类型
        // 会经 executorRoutes 进链上承诺，编译期拒绝。
        let mut parent = parent_settlement_definition(TARGET_NAME);
        parent["spec"]["taskPatterns"][0]["stages"][0]["executor"]["supplierType"] =
            json!("org");
        let error = compile_zhixu_hook_plan(&parent, None, false)
            .expect_err("unknown supplierType must fail before entering executorRoutes");
        assert!(
            error.to_string().contains("supplierType must be one of"),
            "{}",
            error
        );

        // 闭集内取值（zhixu 形态由 dock 系列测试覆盖）照常编译。
        for supplier_type in ["individual", "organization", " organization "] {
            let mut parent = parent_settlement_definition(TARGET_NAME);
            parent["spec"]["taskPatterns"][0]["stages"][0]["executor"]["supplierType"] =
                json!(supplier_type);
            compile_zhixu_hook_plan(&parent, None, true)
                .unwrap_or_else(|err| panic!("{supplier_type} must pass the closed set: {err}"));
        }
    }

    #[test]
    fn rejects_link_violations() {
        let target = target_payment_definition();

        // D008：目标不在 manifest（name 缺失）。
        let mut manifest = manifest_for(&target);
        manifest["definitions"][0]["name"] = json!(UNKNOWN_TARGET);
        let error = compile_zhixu_hook_plan(
            &parent_settlement_definition(TARGET_NAME),
            Some(&manifest),
            false,
        )
        .expect_err("missing target must fail");
        assert!(error.to_string().contains("D008"), "{}", error.to_string());

        // D009：引用不在所选接口上的输出端口（cancelled 只在 evidence 上）。
        let mut parent = parent_settlement_definition(TARGET_NAME);
        parent["spec"]["taskPatterns"][1]["stages"][0]["executor"]["zhixuExecutorConfig"]
            .as_object_mut()
            .unwrap()["signalMap"]["cxl"] = json!("cancelled");
        let manifest = manifest_for(&target);
        let error = compile_zhixu_hook_plan(&parent, Some(&manifest), false)
            .expect_err("unknown output port must fail");
        assert!(
            error.to_string().contains("D009") && error.to_string().contains("payment_service"),
            "{}",
            error.to_string()
        );

        // D009：接口不存在。
        let mut parent = parent_settlement_definition(TARGET_NAME);
        parent["spec"]["taskPatterns"][1]["stages"][0]["executor"]["zhixuExecutorConfig"]
            .as_object_mut()
            .unwrap()["interface"] = json!("payment_archive");
        let error = compile_zhixu_hook_plan(&parent, Some(&manifest), false)
            .expect_err("unknown interface must fail");
        assert!(
            error.to_string().contains("D009") && error.to_string().contains("payment_archive"),
            "{}",
            error.to_string()
        );

        // D020：mode 不在接口 orderModes 内（payment_service 只允许 new）。
        let mut parent = parent_settlement_definition(TARGET_NAME);
        parent["spec"]["taskPatterns"][1]["stages"][0]["executor"]["zhixuExecutorConfig"]
            .as_object_mut()
            .unwrap()["order"]["mode"] = json!("existing");
        let error = compile_zhixu_hook_plan(&parent, Some(&manifest), false)
            .expect_err("mode not allowed by interface must fail");
        assert!(
            error.to_string().contains("D020") && error.to_string().contains("allows orderModes"),
            "{}",
            error.to_string()
        );

        // D015：目标经 manifest dockEdges 回指父定义（跨定义启动环）。
        let mut cycled = manifest_for(&target);
        cycled["definitions"][0]["dockEdges"] = json!([{ "target": "settlement" }]);
        let error = compile_zhixu_hook_plan(
            &parent_settlement_definition(TARGET_NAME),
            Some(&cycled),
            false,
        )
        .expect_err("route cycle through manifest dockEdges must fail");
        assert!(
            error.to_string().contains("D015") && error.to_string().contains("cycle"),
            "{}",
            error.to_string()
        );

        // D015：route 目标 name 回指父定义（单边自环）。
        let interfaces = manifest["definitions"][0]["interfaces"].clone();
        let self_manifest = json!({
            "schemaVersion": dock::DOCK_RESOLUTION_SCHEMA_VERSION,
            "definitions": [{ "name": "settlement", "interfaces": interfaces }]
        });
        let mut parent = parent_settlement_definition(TARGET_NAME);
        parent["spec"]["taskPatterns"][1]["stages"][0]["executor"]["zhixuExecutorConfig"]
            .as_object_mut()
            .unwrap()["target"]["zhixu"] = json!("settlement");
        let error = compile_zhixu_hook_plan(&parent, Some(&self_manifest), false)
            .expect_err("self-referencing route must fail");
        assert!(error.to_string().contains("D015"), "{}", error.to_string());
    }

    /// manifest 定义的最小合法接口（D015 深度链垫底用）。
    fn minimal_interface_value(name: &str) -> Value {
        json!({
            "name": name,
            "orderModes": ["new"],
            "inputs": { "enter": { "source": "buyer", "hook": "main.work#DOCK_ENTER" } },
            "outputs": {},
        })
    }

    #[test]
    fn rejects_startup_depth_beyond_limit_via_manifest_edges() {
        // 链长 = settlement(1) + payment_execution(1) + mid-1..mid-7(7) = 9
        // > MAX_DOCK_DEPTH(8)；深度按 manifest 声明的静态 name 边累计。
        let target = target_payment_definition();
        let mut deep = manifest_for(&target);
        deep["definitions"][0]["dockEdges"] = json!([{ "target": "mid-1" }]);
        for index in 1..7 {
            deep["definitions"].as_array_mut().unwrap().push(json!({
                "name": format!("mid-{index}"),
                "interfaces": [minimal_interface_value(&format!("svc{index}"))],
                "dockEdges": [{ "target": format!("mid-{}", index + 1) }],
            }));
        }
        // 尾节点不声明 dockEdges：缺省=无出边。
        deep["definitions"].as_array_mut().unwrap().push(json!({
            "name": "mid-7",
            "interfaces": [minimal_interface_value("svc7")],
        }));
        let error = compile_zhixu_hook_plan(
            &parent_settlement_definition(TARGET_NAME),
            Some(&deep),
            false,
        )
        .expect_err("startup depth beyond MAX_DOCK_DEPTH must fail");
        assert!(
            error.to_string().contains("D015") && error.to_string().contains("depth"),
            "{}",
            error.to_string()
        );

        // 截短到限内（settlement + payment_execution + mid-1..mid-4 = 6）同
        // 一父定义照常编译。
        let mut shallow = manifest_for(&target);
        shallow["definitions"][0]["dockEdges"] = json!([{ "target": "mid-1" }]);
        for index in 1..4 {
            shallow["definitions"].as_array_mut().unwrap().push(json!({
                "name": format!("mid-{index}"),
                "interfaces": [minimal_interface_value(&format!("svc{index}"))],
                "dockEdges": [{ "target": format!("mid-{}", index + 1) }],
            }));
        }
        shallow["definitions"].as_array_mut().unwrap().push(json!({
            "name": "mid-4",
            "interfaces": [minimal_interface_value("svc4")],
        }));
        compile_zhixu_hook_plan(
            &parent_settlement_definition(TARGET_NAME),
            Some(&shallow),
            false,
        )
        .expect("in-limit startup depth compiles");
    }

    #[test]
    fn rejects_dock_startup_graph_cycles_at_manifest_level() {
        // bug_audit #16（D015 同源防线前移）：manifest 声明面可成的环在
        // link 期环检测一律拒绝——互为目标两节点环、经中间定义三节点环、
        // 自指标自环，不要求本地定义参与成环。
        let target = target_payment_definition();
        let parent = parent_settlement_definition(TARGET_NAME);
        let parent_with_route_to = |name: &str| {
            let mut parent = parent_settlement_definition(TARGET_NAME);
            parent["spec"]["taskPatterns"][1]["stages"][0]["executor"]["zhixuExecutorConfig"]
                .as_object_mut()
                .unwrap()["target"]["zhixu"] = json!(name);
            parent
        };

        // A→B→A：manifest 内两定义互为目标（本地未参与成环也要拒绝）。
        let mut mutual = manifest_for(&target);
        mutual["definitions"][0]["dockEdges"] = json!([{ "target": "wheel_a" }]);
        mutual["definitions"].as_array_mut().unwrap().push(json!({
            "name": "wheel_a",
            "interfaces": [minimal_interface_value("svc_a")],
            "dockEdges": [{ "target": "payment_execution" }],
        }));
        let error = compile_zhixu_hook_plan(&parent, Some(&mutual), false)
            .expect_err("mutual two-definition cycle must fail");
        assert!(
            error.to_string().contains("D015") && error.to_string().contains("cycle"),
            "{error}"
        );

        // A→B→C→A：本地 route A→payment_execution，manifest 边
        // payment_execution→mid、mid→settlement（回指本地）。
        let mut three_node = manifest_for(&target);
        three_node["definitions"][0]["dockEdges"] = json!([{ "target": "mid_cycle" }]);
        three_node["definitions"].as_array_mut().unwrap().push(json!({
            "name": "mid_cycle",
            "interfaces": [minimal_interface_value("svc_mid")],
            "dockEdges": [{ "target": "settlement" }],
        }));
        let error = compile_zhixu_hook_plan(&parent, Some(&three_node), false)
            .expect_err("three-definition cycle must fail");
        let message = error.to_string();
        // 环路径的起点按 BTreeMap 节点序确定（mid_cycle 最小），断言按
        // 环成员 + 完整三段回指形态，不钉旋转起点。
        assert!(
            message.contains("D015")
                && message.contains("mid_cycle -> settlement -> payment_execution -> mid_cycle"),
            "{message}"
        );

        // A→A：manifest 定义经 dockEdges 自指标（自环）。route 自环
        // （target 回指父定义名）由 rejects_link_violations 覆盖。
        let mut self_edge = manifest_for(&target);
        self_edge["definitions"][0]["dockEdges"] = json!([{ "target": "payment_execution" }]);
        let error = compile_zhixu_hook_plan(
            &parent_with_route_to(TARGET_NAME),
            Some(&self_edge),
            false,
        )
        .expect_err("manifest self-edge cycle must fail");
        assert!(
            error
                .to_string()
                .contains("D015")
                && error.to_string().contains("payment_execution -> payment_execution"),
            "{error}"
        );
    }

    #[test]
    fn cloud_artifact_uses_resolved_routes() {
        let target = target_payment_definition();
        let manifest = manifest_for(&target);
        let artifact = compile_cloud_artifact(
            &parent_settlement_definition(TARGET_NAME),
            Some(&manifest),
            false,
        )
        .expect("cloud artifact compiles with manifest");
        assert_eq!(artifact["schemaVersion"], "uvp.cloudArtifact.v2");
        let hooks = artifact["hooks"].as_array().unwrap();
        assert!(hooks
            .iter()
            .all(|hook| hook["sourceZhixuRef"] == json!("self")));
        assert_eq!(artifact["dockRoutes"].as_array().unwrap().len(), 1);
        assert_eq!(artifact["zhixuName"], json!("settlement"));
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
    fn rejects_task_pattern_with_empty_or_missing_stages() {
        // taskPatterns[].stages minItems 1（与文法一致）：空数组与缺失
        //（serde default 吞成空）都在编译期确定性拒绝，hook_plan/cloud
        // 两 target 同口径。
        for mutate in ["empty", "missing"] {
            let mut definition = target_payment_definition();
            match mutate {
                "empty" => definition["spec"]["taskPatterns"][0]["stages"] = json!([]),
                _ => {
                    definition["spec"]["taskPatterns"][0]
                        .as_object_mut()
                        .unwrap()
                        .remove("stages");
                }
            }
            let error = compile_zhixu_hook_plan(&definition, None, true)
                .expect_err("stages-less task pattern must fail");
            assert!(
                error
                    .to_string()
                    .contains("must contain at least one stage"),
                "{mutate}: {error}"
            );
            let error = compile_cloud_artifact(&definition, None, true)
                .expect_err("cloud target must reject the same shape");
            assert!(
                error
                    .to_string()
                    .contains("must contain at least one stage"),
                "{mutate} cloud: {error}"
            );
        }
    }

    #[test]
    fn cloud_target_rejects_send_signal_violations_like_hook_plan() {
        // sendSignals 空串/重复 capability 校验两 target 同口径，cloud 产物
        // 不得放行 hook_plan 已拒绝的声明。
        let mut empty = target_payment_definition();
        empty["spec"]["taskPatterns"][0]["stages"][0]["sendSignals"]
            .as_array_mut()
            .unwrap()
            .push(json!(""));
        for target in ["hook_plan", "cloud"] {
            let error = (if target == "hook_plan" {
                compile_zhixu_hook_plan(&empty, None, true)
            } else {
                compile_cloud_artifact(&empty, None, true)
            })
            .expect_err("empty sendSignal must fail");
            assert!(
                error.to_string().contains("cannot contain an empty signal"),
                "{target}: {error}"
            );
        }

        let mut duplicate = target_payment_definition();
        duplicate["spec"]["taskPatterns"][0]["stages"][0]["sendSignals"]
            .as_array_mut()
            .unwrap()
            .push(json!("str"));
        for target in ["hook_plan", "cloud"] {
            let error = (if target == "hook_plan" {
                compile_zhixu_hook_plan(&duplicate, None, true)
            } else {
                compile_cloud_artifact(&duplicate, None, true)
            })
            .expect_err("duplicate sendSignal must fail");
            assert!(
                error.to_string().contains("duplicate capability"),
                "{target}: {error}"
            );
        }
    }

    #[test]
    fn rejects_executor_without_supplier_id_even_when_selected_stages_anchored() {
        // 非委托 executor 缺 supplierID 时即使被 selectedStages 锚定也拒绝
        // ——产物里不得出现没有投递目标的 executor route。
        let definition = json!({
            "apiVersion": "uvp/v0",
            "kind": "Zhixu",
            "metadata": { "name": "supplier_id_required" },
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
                                "GO": "buyer::selector.assign.executor_selected"
                            },
                            "executor": { "supplierType": "organization" }
                        }
                    ]}
                ]
            }
        });
        let error = compile_zhixu_hook_plan(&definition, None, true)
            .expect_err("executor without supplierID must fail");
        assert!(
            error.to_string().contains("supplierID is required"),
            "unexpected error: {error}"
        );
        let error = compile_cloud_artifact(&definition, None, true)
            .expect_err("cloud target must enforce the same requirement");
        assert!(
            error.to_string().contains("supplierID is required"),
            "unexpected cloud error: {error}"
        );
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
    fn rejects_metadata_name_outside_slug_shape() {
        // N7：name 是作者技术标签，slug 形态（小写开头，小写字母/数字/
        // 下划线/中划线，≤100 字节）；校验仅限形态。
        for name in ["Payment", "pay ment", "1payment", "支付"] {
            let mut definition = target_payment_definition();
            definition["metadata"]["name"] = json!(name);
            let error = compile_zhixu_hook_plan(&definition, None, true)
                .expect_err("non-slug metadata.name must fail");
            assert!(
                error
                    .to_string()
                    .contains("must match ^[a-z][a-z0-9_-]{0,99}$"),
                "unexpected error for {name:?}: {error}"
            );
        }
        // 合法边界：中划线/下划线/数字。
        let mut definition = target_payment_definition();
        definition["metadata"]["name"] = json!("payment-execution_v2");
        compile_zhixu_hook_plan(&definition, None, true)
            .expect("slug-shaped metadata.name compiles");
    }

    #[test]
    fn rejects_subscription_stage_bound_only_through_selected_stages() {
        // 订阅阶段的投递目标编译期定死、运行时禁止 executor patch。被
        // selector 指到的订阅阶段仍必须有自身静态 executor。
        let definition = json!({
            "apiVersion": "uvp/v0",
            "kind": "Zhixu",
            "metadata": { "name": "subscription_static_executor" },
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
        // 阶段物化裁决：无静态 executor、仅 selectedStages 覆盖的阶段不得
        // 声明 receiveSignals——hook 编译为 flags=0 watcher，链上永远无法
        // 物化（纯 watcher 不物化、executor patch 不物化）。
        let definition = json!({
            "apiVersion": "uvp/v0",
            "kind": "Zhixu",
            "metadata": { "name": "unmaterializable_watcher" },
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
        let mut signals = send_signals.to_vec();
        // P0-4：零 hook 阶段不过物化门；seed 是执行者自发入口信号。
        signals.push("seed");
        json!({
            "name": name,
            "source": source,
            "receiveSignals": { "PUBLISH": format!("{source}::{task}.{name}.seed") },
            "sendSignals": signals,
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
            "metadata": { "name": "mint_cycle" },
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
