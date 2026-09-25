//! dock 状态编译：一次编译内的接口声明、未链接/已链接 route 与 hook 标记
//! 输入（对接接口/路由链接的权威实现在 `crate::dock` 模块）。

use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use uvp_model::{ZhixuDefinition, ZhixuStage};

use crate::{dock, CompilerError, Result};

fn issues_from_dock(issues: &[dock::DockIssue]) -> CompilerError {
    let messages = issues
        .iter()
        .map(std::string::ToString::to_string)
        .collect::<Vec<_>>();
    CompilerError::Issues(crate::join_issues_bounded(&messages))
}

/// 一次编译中的 dock 状态：接口声明、未链接/已链接 route、hook 标记输入。
pub(crate) struct DockState {
    pub(crate) interface_json: Option<Value>,
    pub(crate) routes_json: Vec<Value>,
    /// 声明面产物（unresolvedDockRoutes）：target:null 动态选择 route 恒入
    /// （目标空缺，不进 link）；parse-only 产物中静态目标 route 同面携带
    /// （与动态目标对称）。
    pub(crate) unresolved_json: Vec<Value>,
    /// dockInterface input port 引用的本地 hook（`<task>.<stage>#<hook>`），
    /// 这些 mailbox hook 不走普通依赖引用校验（dock 模块按端口约束校验）。
    pub(crate) input_port_hook_ids: BTreeSet<String>,
    /// 可作为 new 模式出生锚的 input 端口（orderModes 含 new 的接口）
    /// 引用的本地 hook：编译为 orderTriggerKind=dock。
    pub(crate) entrance_hook_ids: BTreeSet<String>,
    /// entrance 端口 atom 的事实键 (source, task.stage.signal) → 发布端口
    /// 路径：出生通道键并集查重（mint 出生键 ∪ dock entrance 键）的
    /// dock 侧输入。
    pub(crate) entrance_fact_keys: BTreeMap<(String, String), Vec<String>>,
    /// signalMap 绑定的本地信号全名（`<task>.<stage>.<signal>`，静态与
    /// target:null 动态 route 一并收集）：dock 输出回传的父侧落点。回传
    /// 事实由引擎内部事务入口写入，不经发射适格面——对这些信号声明
    /// validWhen 是永不求值的死代码，build_signal_admissions 按 D029 拒绝。
    pub(crate) output_relay_signals: BTreeSet<String>,
}

pub(crate) fn compile_dock_state(
    definition: &ZhixuDefinition,
    stage_pairs: &[(String, ZhixuStage)],
    resolution_manifest: Option<&Value>,
    allow_unresolved: bool,
) -> Result<DockState> {
    let unlinked =
        dock::collect_unlinked_routes(stage_pairs).map_err(|issues| issues_from_dock(&issues))?;

    // 回传落点从声明面全量收集（解析后、分流静态/动态之前）：动态选择
    // route 的 signalMap 一旦被选择记录补齐目标即参与回传，与静态 route
    // 同一面。
    let output_relay_signals = unlinked
        .iter()
        .flat_map(|route| {
            route
                .config
                .signal_map
                .keys()
                .map(|key| format!("{}.{}", route.stage_identifier, key))
        })
        .collect();

    // 声明面收集：target:null 的动态选择 route 不进
    // link（目标空缺，无 D008 可言），改入未解析清单随产物携带（云轨
    // 运行时由选择记录补齐）；parse-only 产物（allow_unresolved）的静态
    // 目标 route 同样进入声明面——解析产物如实携带全部委托形态，与动态
    // 目标对称（静态条目携带作者声明的 target.zhixu）。
    let mut static_routes = Vec::new();
    let mut unresolved_json = Vec::new();
    for route in unlinked {
        match route.config.target_name.as_ref() {
            Some(_) => {
                if allow_unresolved {
                    unresolved_json.push(route.unresolved_json());
                }
                static_routes.push(route);
            }
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
        .as_ref()
        .map(|compiled| dock::input_port_hook_ids(&compiled.declarations))
        .unwrap_or_default();
    let entrance_hook_ids = interfaces
        .as_ref()
        .map(|compiled| dock::entrance_hook_ids(&compiled.declarations))
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
    let interface_json = interfaces
        .as_ref()
        .map(|compiled| dock::interface_declarations_json(&compiled.declarations));
    let entrance_fact_keys = interfaces
        .map(|compiled| compiled.entrance_fact_keys)
        .unwrap_or_default();
    Ok(DockState {
        interface_json,
        routes_json,
        unresolved_json,
        input_port_hook_ids,
        entrance_hook_ids,
        entrance_fact_keys,
        output_relay_signals,
    })
}

// 可作为 new 模式出生锚的 dockInterface input 端口（orderModes 含 new 的
// 接口的全部 input 端口——new 模式 route 的唯一 input 绑定可落在其中任意
// 一个）引用的本地 hook（`<task>.<stage>#<hook>`）。这些钩子编译为
// orderTriggerKind=dock（flags=dock|emitReady=6），既是阶段物化门里的合法
// 物化路径，也是 compile_stage_hooks 打 order-trigger 标记的依据——两处
// 共用同一来源，避免判定口径漂移。
pub(crate) fn dock_entrance_hook_ids(dock_state: &DockState) -> BTreeSet<String> {
    dock_state.entrance_hook_ids.clone()
}
