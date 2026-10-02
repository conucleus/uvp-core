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

pub(crate) struct DockState {
    pub(crate) interface_json: Option<Value>,
    pub(crate) routes_json: Vec<Value>,
    pub(crate) unresolved_json: Vec<Value>,
    pub(crate) input_port_hook_ids: BTreeSet<String>,
    pub(crate) entrance_hook_ids: BTreeSet<String>,
    pub(crate) entrance_fact_keys: BTreeMap<(String, String), Vec<String>>,
    pub(crate) output_relay_signals: BTreeSet<String>,
}

pub(crate) fn compile_dock_state(
    definition: &ZhixuDefinition,
    stage_pairs: &[(String, ZhixuStage)],
    dock_targets: Option<&Value>,
    allow_unresolved: bool,
) -> Result<DockState> {
    let unlinked =
        dock::collect_unlinked_routes(stage_pairs).map_err(|issues| issues_from_dock(&issues))?;

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

    let mut static_routes = Vec::new();
    let mut unresolved_json = Vec::new();
    for route in unlinked {
        match route.config.target_uid.as_ref() {
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
        match dock_targets {
            Some(targets_value) => {
                let targets = dock::parse_dock_targets(targets_value)
                    .map_err(|issues| issues_from_dock(&issues))?;
                dock::link_dock_routes(&definition.metadata.name, &static_routes, &targets)
                    .map_err(|issues| issues_from_dock(&issues))?
            }
            None if allow_unresolved => Vec::new(),
            None => {
                return Err(CompilerError::Message(
                    "UNRESOLVED_DOCK_TARGET: definition contains zhixu executor routes with static targets but no registered targets were provided; runnable compilation requires linking against published target interfaces".to_string(),
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

pub(crate) fn dock_entrance_hook_ids(dock_state: &DockState) -> BTreeSet<String> {
    dock_state.entrance_hook_ids.clone()
}
