//! 定义校验：形状/尺寸闸、执行器绑定、物化门、mint/订阅/信号引用等
//! 全部编译期校验规则的汇集地。

use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, VecDeque};

use uvp_hook_dsl::ParseHookOutput;
use uvp_model::{ZhixuDefinition, ZhixuExecutor, ZhixuStage};

use crate::lower::{is_zhixu_executor_stage, parse_hook_for_compiler, value_str, StageEntry};

/// 全局 stage.source 上限：36 字节，与 hook 标头/订阅目标的 source 类上限
/// 同口径（同一 source 类命名空间，hook-dsl 同值钉死）。37-100 字节的
/// source 是"声明即死"命名空间——所有 hook/订阅引用在解析层被拒。严于
/// 落库列宽（source_zhixu_id VARCHAR(64)），上限在编译期拒绝。
const MAX_STAGE_SOURCE_BYTES: usize = 36;
/// DDL 维度镜像：global_zhixu.name / global_stage.stage_identifier
/// VARCHAR(100)。
const MAX_IDENTIFIER_BYTES: usize = 100;
/// DDL 维度镜像：canonical 三段式 task.stage.signal 落
/// individual_record.signal_name / hook_dependency.signal_name VARCHAR(100)。
const MAX_SIGNAL_NAME_BYTES: usize = 100;
/// metadata.name 的 slug 形态：技术名风格，仅限形态校验，
/// 不承担任何语义判断（非唯一、不参与关系推断）。
const NAME_SLUG_PATTERN: &str = "^[a-z][a-z0-9_-]{0,99}$";

// 定义内计数上限（资源闸）：列宽族只约束单个标识符的长度，计数
// 维度无闸时 plan 控制的输入可以让编译期的校验/产物规模无界增长。取值
// 对真实计划留有余量，且不与合约侧同族上限打架（依赖键 1024）。
// 能力表/绑定表由 capabilitiesRoot 一次承诺，无 256/128 规模上限，
// 不在此闸的参照系内。
/// 单定义 taskPatterns 数上限。
const MAX_TASK_PATTERNS: usize = 64;
/// 单定义摊平后的阶段总数上限（每阶段至少编译一个 hook，阶段数是
/// hooks 与产物规模的直接下界——编译期资源闸，与链上注册面无关）。
const MAX_STAGE_ENTRIES: usize = 256;
/// 单定义 receiveSignals 通道（编译产物 hooks）总数上限：512 恰为合约
/// MAX_PLAN_DEPENDENCIES=1024 的一半，给每钩平均 ≥2 个依赖键的余量。
const MAX_HOOKS: usize = 512;

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
    // 文法两册 §2.2 都声明 spec.nucleation.id 必填：字段缺失由 serde 必填
    // 闸拒绝，此处钉住空白值——与 stage.source 的非空白闸同纪律（空白 id
    // 是确定性非法输入，不 trim 归一放行；该字段同时作用于云/链两轨）。
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
                // 精确匹配不 trim——带空白的变体按闭集外拒绝（Go 侧严格
                // 枚举闸同口径），不归一化放行。
                if !uvp_model::is_known_supplier_type(&executor.supplier_type) {
                    issues.push(format!(
                        "spec.taskPatterns[{task_index}].stages[{stage_index}].executor.supplierType must be one of {} (exact match, whitespace variants rejected), found {:?}",
                        uvp_model::SUPPLIER_TYPES
                            .map(|value| format!("{value:?}"))
                            .join(", "),
                        executor.supplier_type
                    ));
                }
                // executor.selectableResource 与 stage.fileResources 同为
                // FileResource 面：条目随 route 进链上承诺（executorHash /
                // resourcesHash），词表外 fileType（含带空白变体）在此拒绝。
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
            // stage.source：非空、plain identifier 字符集、≤36（与
            // hook-dsl 标头/订阅目标的 source 类上限同口径）。
            // 空串会以空键混进 mintedSources；含空格/Unicode 的 source
            // 是路由键，两侧必须逐字节一致（Go 镜像 zhixu_schema.go 同款
            // 字符集校验；36 严于落库列宽 source_zhixu_id VARCHAR(64)）。
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
            // 落库时 value too long。组合长度按 Go validateDDLDimensions
            // 的全名精确计：canonical 三段式声明本身即全名，按原文精确计
            // 长；裸名才拼 stage 前缀——三段式再拼一次前缀会把 task.stage
            // 段重复计入，误拒真实 ≤100 的合法声明。
            for signal in &stage.send_signals {
                let full_name_bytes = if signal.contains('.') {
                    signal.len()
                } else {
                    stage_identifier.len() + 1 + signal.len()
                };
                if full_name_bytes > MAX_SIGNAL_NAME_BYTES {
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

/// 单个 FileResource 条目的 fileType 闭集检查（fileResources 与
/// executor.selectableResource 共用）：缺失/非串/闭集外（含带空白变体）
/// 都是确定性的非法输入——条目内容会原样进链上承诺，归一化放行会让
/// 承诺侧按原文分叉。
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

/// `^[a-z][a-z0-9_-]{0,99}$`（字节口径）。
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
    // 非委托 executor 必须携带非空 supplierID（对齐 TS 侧同款拒绝）：
    // 缺 supplierID 的执行器即使被 selectedStages 锚定也是"看似绑定"——
    // 产物里会出现没有投递目标的 executor route。zhixu 委托的身份在
    // zhixuExecutorConfig.target（D001 禁 supplierID），不在此列。
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

/// 阶段物化门（onchain 目标）：链上阶段只能由本阶段 order-trigger
/// （mint/dock）或 EMIT_READY hook Ready 物化；executor patch 也不物化
/// （UVPStateMachine activateStageExecutor 不调用 _materializeStage）。
/// 因此每个阶段声明都必须编译出至少一个带物化位的 hook：
/// - 仅 sendSignals、无 receiveSignals 的阶段编译为零 hook——阶段永不可
///   物化，其信号在链上没有钩子可挂（_recordSignal 要求源阶段已物化，
///   submitSignal 恒 revert UnknownHook），下游 hook 永 Init；
/// - 有 receiveSignals 但全部编译为 flags=0 纯 watcher 的阶段同样不物化。
///
/// dockInterface entrance 端口钩子编译为 dock|emitReady（=6），是合法
/// 物化路径，不按 watcher 拒绝（CORE-8）。
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

/// UVP-01（模-1 同族裁决，对齐 Go 镜像 zhixu_schema.go 的同款检查）：zhixu
/// 委托执行器的信封恒为 NewSource=false 的订单锚定子信号，无法携带通道
/// 事实身份。本域 source 类无 mint 声明时订阅注入 route=fanin、投递落通道
/// 维度（order_id=''），委托信封缺 order_id 会被状态机按永久错误拒绝——
/// "编译放行、运行必死"的组合在编译期关闭；有锚阶段（本类存在 mint 声明，
/// route=order 按单投递）不受此限。mint 出生阶段与委托的组合由
/// validate_mint_anchors 单独拒绝。
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

pub(crate) fn validate_mint_anchors(
    entries: &[StageEntry],
    entrance_fact_keys: &BTreeMap<(String, String), Vec<String>>,
) -> Vec<String> {
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
                    .map(|executor| executor.supplier_type.clone())
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
    // 6) 出生通道键并集查重（U2）：mint 出生键 ∪ dock entrance 键内
    //    不得重复——跨通道重复同样拒绝。
    issues.extend(validate_birth_channel_key_uniqueness(
        entries,
        entrance_fact_keys,
    ));
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

/// 出生通道键并集查重（U2）：同一 plan 内，出生通道键的并集——mint 阶段
/// ANCHOR 订阅的出生事实键 (source, task.stage.signal) ∪ dockInterface
/// entrance 端口（orderModes 含 new 的接口的 input 端口）atom 的事实键
/// ——内不得重复。三个臂的裁决现状并不一致：
/// - mint∪dock / dock∪dock：三方一致拒绝（合约注册门
///   DuplicateBirthChannelKey、TS 编译器镜像、本仓编译期）。跨通道共享键
///   会让任一侧的出生事务把另一侧的出生线一并推 Ready、物化幻影阶段；
///   dock entrance 键按 route 钉死（planHookDependsOn），一键挂两条
///   entrance 时任一 route 的子单会物化另一 route 的阶段。
/// - mint∪mint：未收敛分叉。本仓拒绝（下方"一事一单"臂）：一事实多
///   mint 各铸一单，"该事实对应哪个订单"三线发散，本仓选择在编译期
///   收口；合约与 TS 放行：一事实扇出多条 mint 出生线是产品现行形态
///   （customs 基准 plan：order::registered 同时出生执行者选择与资源
///   发布两阶段），同一 mint 出生上下文内物化、不产生幻影阶段。收敛前
///   如实登记两侧口径，不宣称一致。
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
            // 解析失败的条目不构成事实键：语法错误由引用存在性校验统一上报。
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

pub(crate) fn validate_receive_signal_references(
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
// 空白字符与 hook-dsl validate_hook_name 同口径拒绝：通道名两侧必须逐
// 字节一致，含空白的名字不做 trim 归一。
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
        if !declares_signal_expanding_to(
            &referenced_stage.stage.send_signals,
            &referenced_stage.stage_identifier,
            &dependency.signal_name,
        ) {
            // 目标 stage 未声明 sendSignals 时任何引用都是悬空引用：文档要求
            // 引用存在，放行会把死依赖从编译期推迟为运行期静默 init。
            issues.push(format!(
                "{path} references unknown signal {stage_identifier}.{signal_name}"
            ));
        }
    }
    issues
}

/// sendSignals 声明是否展开为给定的全名（task.stage.signal）：裸名展开为
/// 声明阶段前缀 + 信号名；canonical 三段式（强制自指，见
/// parse_signal_capability）本身就是全名。引用存在性（本函数）、
/// capability 去重与 D014 一律按展开后的全名统一比较——只比第三段会把
/// canonical 声明判成悬空引用（"声明即死"）。`<target>::<signal>`
/// triggerOrigin 声明不参与该比较：它声明的是跨源触发能力（relation=1，
/// 合约消费），不是本阶段 current 事实。
pub(crate) fn declares_signal_expanding_to(
    send_signals: &[String],
    stage_identifier: &str,
    full_signal_name: &str,
) -> bool {
    send_signals.iter().any(|declared| {
        if declared.contains("::") {
            return false;
        }
        if declared == full_signal_name {
            return true;
        }
        !declared.contains('.') && format!("{stage_identifier}.{declared}") == full_signal_name
    })
}

fn parse_signal_reference(signal_name: &str) -> Option<(String, String)> {
    let parts = signal_name.split('.').collect::<Vec<_>>();
    if parts.len() != 3 {
        return None;
    }
    Some((format!("{}.{}", parts[0], parts[1]), parts[2].to_string()))
}

/// sendSignals 信号声明的形态闸：裸名或 task.stage.signal 三段式，每段与
/// task/stage 名同文法（valid_identifier_part——云轨事实入口的
/// ValidateIdentifierPart 同口径：编译期放行数字/'-'/'_' 开头的段只会在
/// 执行器发送时被拒，阶段没有报错出口地静默死）。
pub(crate) fn valid_signal_declaration(value: &str) -> bool {
    let parts: Vec<&str> = value.split('.').collect();
    matches!(parts.len(), 1 | 3) && parts.iter().all(|part| valid_identifier_part(part))
}

fn has_static_executor(executor: Option<&ZhixuExecutor>) -> bool {
    match executor {
        None => false,
        // zhixu 委托执行器本身就是静态锚定（配置合法性由 dock 模块校验）。
        Some(executor) if executor.supplier_type == "zhixu" => true,
        Some(executor) => executor
            .supplier_id
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty()),
    }
}
