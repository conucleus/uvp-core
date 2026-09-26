use super::*;
use serde_json::json;

/// uid 形态的未发布目标占位（形态合法、link 不可达：不在注册表中）。
const UNKNOWN_TARGET: &str = "zx-eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";

/// 注册表中冒充"本地定义自身"的目标 uid（linker 不知道本地定义的 uid，
/// 按内容查找只看相等性）。
const SELF_TARGET_UID: &str = "zx-ffffffffffffffffffffffffffffffff";

/// mid 链节点的 uid 形态占位（D015 深度/环链用，与上面三个常量不相交）。
fn mid_uid(index: usize) -> String {
    format!("zx-{index:032x}")
}

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
                            "sendSignals": [{ "name": "str" }],
                            "executor": { "supplierType": "organization", "supplierID": "payment-gateway" }
                        },
                        {
                            "name": "control",
                            "source": "payment",
                            "receiveSignals": {
                                "DOCK_CANCEL": "payment::payment_flow.control.cancel"
                            },
                            "sendSignals": [{ "name": "cxl" }],
                            "executor": { "supplierType": "organization", "supplierID": "payment-gateway" }
                        },
                        {
                            "name": "settle",
                            "source": "payment",
                            "receiveSignals": {
                                "SETTLE": "payment::payment_flow.init.str"
                            },
                            "sendSignals": [
    { "name": "cmp" },
    { "name": "err" }
    ],
                            "executor": { "supplierType": "organization", "supplierID": "payment-gateway" }
                        }
                    ]}
                ]
            }
        })
}

const TARGET_UID: &str = "zx-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

// ------------------------------------------------------------------
// 调用方示例（settlement）：new 模式静态指定生产委托。
// ------------------------------------------------------------------
fn parent_settlement_definition(target_uid: &str) -> Value {
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
                            // 物化门：零 hook 阶段在链上永不可物化、信号
                            // 没有钩子可挂；seed 是执行者自发入口信号。
                            "receiveSignals": { "PLACE": "buyer::checkout.confirm.seed" },
                            "sendSignals": [
    { "name": "cmp" },
    { "name": "seed" }
    ],
                            "executor": { "supplierType": "organization", "supplierID": "buyer-app" }
                        },
                        {
                            "name": "cancel",
                            "source": "buyer",
                            "receiveSignals": { "ABORT": "buyer::checkout.cancel.seed" },
                            "sendSignals": [
    { "name": "cmp" },
    { "name": "seed" }
    ],
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
                            "sendSignals": [
    { "name": "str" },
    { "name": "cmp" },
    { "name": "err" },
    { "name": "cxl" }
    ],
                            "executor": {
                                "supplierType": "zhixu",
                                "zhixuExecutorConfig": {
                                    "target": { "zhixu": target_uid },
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
fn parent_recycling_definition(target_uid: &str) -> Value {
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
                            "sendSignals": [
    { "name": "cmp" },
    { "name": "seed" }
    ],
                            "executor": {
                                "supplierType": "zhixu",
                                "zhixuExecutorConfig": {
                                    "target": { "zhixu": target_uid },
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

/// 构造 dockTargets 注册表：条目 {uid, definition}——目标定义原文入场，
/// 接口声明与静态出边由 core 从原文单源提取；uid 用形态合法的固定占位
/// （派生过程是各轨内务）。
fn dock_target_entry(uid: &str, target: &Value) -> Value {
    json!({ "uid": uid, "definition": target })
}

fn dock_targets_for(target: &Value) -> Value {
    json!([dock_target_entry(TARGET_UID, target)])
}

/// 给定义的第一个 stage 换上带静态 target 的 zhixu executor：注册表条目
/// 的静态出边（D015 启动图）由 core 从该 executor config 提取。
fn with_static_dock_edge(mut definition: Value, edge_target: Option<&str>) -> Value {
    definition["spec"]["taskPatterns"][0]["stages"][0]["executor"] = json!({
        "supplierType": "zhixu",
        "zhixuExecutorConfig": {
            "target": edge_target
                .map(|uid| json!({ "zhixu": uid }))
                .unwrap_or(Value::Null),
            "interface": "payment_service",
            "order": { "mode": "existing" },
            "signalMap": { "cmp": "completed" }
        }
    });
    definition
}

/// 链/环节点的最小目标定义：单 stage，带静态 target 的 zhixu executor
/// 指向 `edge_target`（None = 无静态出边）；接口 svc 供承诺面完整性。
fn chain_link_definition(edge_target: Option<&str>) -> Value {
    let executor = match edge_target {
        Some(uid) => json!({
            "supplierType": "zhixu",
            "zhixuExecutorConfig": {
                "target": { "zhixu": uid },
                "interface": "svc",
                "order": { "mode": "existing" },
                "signalMap": { "cmp": "done" }
            }
        }),
        None => json!({ "supplierType": "organization", "supplierID": "org" }),
    };
    json!({
        "apiVersion": "uvp/v0",
        "kind": "Zhixu",
        "metadata": { "name": "chain_link" },
        "spec": {
            "platform": { "type": "cloud" },
            "nucleation": { "id": "chain-core" },
            "taskPatterns": [{ "name": "main", "stages": [
                { "name": "work", "source": "buyer", "executor": executor }
            ]}],
            "dockInterface": {
                "svc": {
                    "orderModes": ["existing"],
                    "outputs": { "done": { "signal": "buyer::main.work.cmp" } }
                }
            }
        }
    })
}

/// 注册表条目：uid + 链节点定义。
fn chain_link_entry(uid: String, edge_target: Option<&str>) -> Value {
    json!({ "uid": uid, "definition": chain_link_definition(edge_target) })
}

#[test]
fn send_signals_total_is_uncapped() {
    // 能力表 Merkle 化：链上以 capabilitiesRoot 一次承诺，无逐条注册，
    // 无 256 规模上限。257 条照常编译、产物逐条保留，钉住规模不受限；
    // hook_plan 与 cloud 共用 build_signal_capabilities，两个 target
    // 同口径放行。
    let definition_with = |count: usize| {
        let signals: Vec<Value> = (0..count)
            .map(|index| json!({ "name": format!("sig{index:03}") }))
            .collect();
        json!({
            "apiVersion": "uvp/v0",
            "kind": "Zhixu",
            "metadata": { "name": "capability_scale" },
            "spec": {
                "platform": { "type": "cloud" },
                "nucleation": { "id": "cap-core" },
                "taskPatterns": [
                    { "name": "main", "stages": [
                        {
                            "name": "work",
                            "source": "buyer",
                            // 零 hook 阶段不过物化门，给一条自发
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
    let plan = compile_zhixu_hook_plan(&definition_with(257), None, true)
        .expect("capability count is uncapped");
    assert_eq!(
        plan["signalCapabilities"].as_array().map(Vec::len),
        Some(257)
    );
    compile_cloud_artifact(&definition_with(257), None, true)
        .expect("cloud target accepts the same capability table");
}

#[test]
fn resolved_routes_carry_the_neutral_uid_keyed_shape() {
    // 壳上无派生身份：route 只携带本地声明与目标 uid/接口名/端口绑定，
    // 不携带任何哈希/root 字段（哈希承诺由各轨在此形状上自行计算）。
    let target = target_payment_definition();
    let dock_targets = dock_targets_for(&target);
    let plan = compile_zhixu_hook_plan(
        &parent_settlement_definition(TARGET_UID),
        Some(&dock_targets),
        false,
    )
    .expect("new-mode route links");
    let route = &plan["dockRoutes"][0];
    assert_eq!(route["schemaVersion"], "uvp.dockRoute.v3");
    assert_eq!(
        route["local"]["stageIdentifier"],
        "settlement.execute_payment"
    );
    assert_eq!(route["target"]["uid"], json!(TARGET_UID));
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
        &parent_recycling_definition(TARGET_UID),
        Some(&dock_targets),
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
    let mut parent = parent_settlement_definition(TARGET_UID);
    let second_dock = json!({
            "name": "second_dock",
            "source": "buyer",
            "receiveSignals": { "START": "buyer::checkout.confirm.cmp" },
            "sendSignals": [
    { "name": "str" },
    { "name": "cmp" },
    { "name": "err" }
    ],
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
    // 同一 dockTargets：settlement 的目标存在，second_dock 的目标缺失。
    let dock_targets = dock_targets_for(&target);
    let error = compile_zhixu_hook_plan(&parent, Some(&dock_targets), false)
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
    let parent = parent_settlement_definition(TARGET_UID);
    let dock_targets = dock_targets_for(&target);
    let plan = compile_zhixu_hook_plan(&parent, Some(&dock_targets), false)
        .expect("linked parent compiles");
    assert_eq!(plan["schemaVersion"], "uvp.hookPlan.v4");
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
    assert_eq!(route["schemaVersion"], "uvp.dockRoute.v3");
    assert_eq!(
        route["local"]["stageIdentifier"],
        "settlement.execute_payment"
    );
    assert_eq!(route["orderMode"], "new");
    assert_eq!(route["target"]["uid"], json!(TARGET_UID));
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
    // 原文（source 是 input 侧的单源 seam 观测面）。
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
fn rejects_parent_without_dock_targets() {
    let error = compile_zhixu_hook_plan(&parent_settlement_definition(TARGET_UID), None, false)
        .expect_err("unresolved dock target must fail");
    assert!(
        error.to_string().contains("UNRESOLVED_DOCK_TARGET"),
        "unexpected error: {error}"
    );
}

#[test]
fn dock_link_compile_target_is_unknown() {
    // 不存在 dock link 编译 target（uvp.dock-link v1 产物面）
    // ——出现即按未知 target 响亮拒绝（link 校验由 hook_plan/cloud/parse
    // 在 dockTargets 在场时同一链路承担）。
    let request = json!({
        "target": "dock_link",
        "definition": target_payment_definition(),
    });
    let envelope: Value =
        serde_json::from_str(&compile_json(&request.to_string())).expect("envelope");
    assert_eq!(envelope["ok"], json!(false));
    let message = envelope["diagnostics"][0]["message"].as_str().unwrap();
    assert!(
        message.contains("unsupported compile target") && message.contains("dock_link"),
        "{message}"
    );
}

#[test]
fn parse_target_allows_unresolved() {
    let value = compile_zhixu_hook_plan(&parent_settlement_definition(TARGET_UID), None, true)
        .expect("parse target allows unresolved routes");
    assert_eq!(value["dockRoutes"].as_array().unwrap().len(), 0);
    // 静态目标 route 不因无 dockTargets 而从声明面消失——
    // parse 产物如实携带全部委托形态，静态条目携带作者声明的
    // target.zhixu（目标定义 uid 引用）。
    let unresolved = value["unresolvedDockRoutes"].as_array().unwrap();
    assert_eq!(unresolved.len(), 1);
    let route = &unresolved[0];
    assert_eq!(
        route["schemaVersion"],
        dock::DOCK_ROUTE_UNRESOLVED_SCHEMA_VERSION
    );
    assert_eq!(route["stageIdentifier"], "settlement.execute_payment");
    assert_eq!(route["target"], json!({ "zhixu": TARGET_UID }));
    assert_eq!(route["interfaceName"], "payment_service");
    assert_eq!(route["orderMode"], "new");
    assert_eq!(
        route["inputBindings"],
        json!([{ "hookId": "settlement.execute_payment#EXECUTE", "port": "execute" }])
    );
}

#[test]
fn parse_product_declaration_face_is_complete_with_dock_targets() {
    // parse-only 产物带 dockTargets：静态 route 照常解析进 dockRoutes，
    // 同时保留声明面条目（与动态目标对称——dockTargets 在场时 null-target
    // route 也不退出声明面，见 dock_targets_present_null_target_route_stays_
    // unresolved）。
    let target = target_payment_definition();
    let dock_targets = dock_targets_for(&target);
    let artifact = compile_cloud_artifact(
        &parent_settlement_definition(TARGET_UID),
        Some(&dock_targets),
        true,
    )
    .expect("parse-only compilation with dock targets links and declares");
    assert_eq!(artifact["dockRoutes"].as_array().unwrap().len(), 1);
    assert_eq!(
        artifact["dockRoutes"][0]["target"]["uid"],
        json!(TARGET_UID)
    );
    let unresolved = artifact["unresolvedDockRoutes"].as_array().unwrap();
    assert_eq!(unresolved.len(), 1);
    assert_eq!(
        unresolved[0]["stageIdentifier"],
        "settlement.execute_payment"
    );
    assert_eq!(unresolved[0]["target"], json!({ "zhixu": TARGET_UID }));

    // 可运行产物（allow_unresolved=false）不做声明面冗余：静态 route
    // 全量解析后 unresolvedDockRoutes 不落字段（既有口径不变）。
    let runnable = compile_cloud_artifact(
        &parent_settlement_definition(TARGET_UID),
        Some(&dock_targets),
        false,
    )
    .expect("runnable compilation keeps the lean declaration face");
    assert!(runnable.get("unresolvedDockRoutes").is_none());
}

#[test]
fn rejects_unsupported_executor_config_shapes() {
    // triggerEntrance：不受支持的调用方字段，D002 未知字段硬错误。
    let mut parent = parent_settlement_definition(TARGET_UID);
    parent["spec"]["taskPatterns"][1]["stages"][0]["executor"]["zhixuExecutorConfig"]
        .as_object_mut()
        .unwrap()
        .insert("triggerEntrance".to_string(), json!("payment_flow.init"));
    let error =
        compile_zhixu_hook_plan(&parent, None, false).expect_err("triggerEntrance must hard-fail");
    let message = error.to_string();
    assert!(
        message.contains("D002") && message.contains("triggerEntrance"),
        "{message}"
    );
    assert!(message.contains("unknown field"), "{message}");

    // schemaVersion 残留键：作者面没有 schemaVersion，按未知字段硬拒绝。
    let mut parent = parent_settlement_definition(TARGET_UID);
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
    let mut parent = parent_settlement_definition(TARGET_UID);
    parent["spec"]["taskPatterns"][1]["stages"][0]["executor"]["zhixuExecutorConfig"]
        .as_object_mut()
        .unwrap()["signalMap"]["str"] = json!("payment::payment_flow.init.str");
    let error = compile_zhixu_hook_plan(&parent, None, false)
        .expect_err("hook-DSL signalMap value must hard-fail");
    assert!(error.to_string().contains("D006"), "{}", error.to_string());

    // supplierID + zhixu：D001。
    let mut parent = parent_settlement_definition(TARGET_UID);
    parent["spec"]["taskPatterns"][1]["stages"][0]["executor"]["supplierID"] =
        json!("payment-zhixu");
    let error = compile_zhixu_hook_plan(&parent, None, false)
        .expect_err("supplierID on zhixu executor must fail");
    assert!(error.to_string().contains("D001"), "{}", error.to_string());
}

#[test]
fn rejects_signal_map_keys_outside_hook_name_budget() {
    // D006：key 超 26 字节（hook_name = "signalMap." + key 落 VARCHAR(36)）。
    let mut parent = parent_settlement_definition(TARGET_UID);
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
    let mut parent = parent_settlement_definition(TARGET_UID);
    parent["spec"]["taskPatterns"][1]["stages"][0]["executor"]["zhixuExecutorConfig"]
        .as_object_mut()
        .unwrap()["signalMap"]
        .as_object_mut()
        .unwrap()
        .insert("bad.key".to_string(), json!("cancelled"));
    let error =
        compile_zhixu_hook_plan(&parent, None, false).expect_err("dotted signalMap key must fail");
    assert!(
        error.to_string().contains("D006") && error.to_string().contains("must not contain '.'"),
        "{}",
        error.to_string()
    );

    // 组合维度：stage 标识符 + key 超 signal_name 列宽（100）。
    let mut parent = parent_settlement_definition(TARGET_UID);
    let long_stage = "s".repeat(90);
    parent["spec"]["taskPatterns"][1]["stages"][0]["name"] = json!(long_stage);
    let error =
        compile_zhixu_hook_plan(&parent, None, false).expect_err("combined signal name must fail");
    assert!(
        error.to_string().contains("individual_record.signal_name"),
        "{}",
        error.to_string()
    );
}

#[test]
fn signal_map_keys_match_expanded_full_signal_names() {
    // D006 存在性与 D014/引用面同口径：按展开后的全名比较（crate::
    // declares_signal_expanding_to）——裸名 key 展开为
    // <task>.<stage>.<key>，canonical 显式自指声明的信号即可被裸名
    // key 引用命中；裸名精确匹配会把 canonical 声明判成"声明即不可
    // 投递"。
    let target = target_payment_definition();
    let dock_targets = dock_targets_for(&target);

    // ① signalMap 裸名 key 引用 canonical 自指声明：key "str" 展开后与
    // settlement.execute_payment.str 同一全名，链接照常。
    let mut parent = parent_settlement_definition(TARGET_UID);
    parent["spec"]["taskPatterns"][1]["stages"][0]["sendSignals"] = json!([{ "name": "cmp" }, { "name": "err" }, { "name": "cxl" }, { "name": "settlement.execute_payment.str" }]);
    let plan = compile_zhixu_hook_plan(&parent, Some(&dock_targets), false).expect(
        "canonical self-reference declaration must be deliverable via a bare signalMap key",
    );
    assert_eq!(
        plan["dockRoutes"][0]["outputBindings"],
        json!([
            { "signal": "cmp", "port": "completed" },
            { "signal": "err", "port": "failed" },
            { "signal": "str", "port": "started" }
        ])
    );

    // ② 展开后仍无主的 key：sendSignals 里的裸名 cmp 不展开成
    // settlement.execute_payment.str，D006 悬空引用照拒。
    let mut parent = parent_settlement_definition(TARGET_UID);
    parent["spec"]["taskPatterns"][1]["stages"][0]["sendSignals"] =
        json!([{ "name": "cmp" }, { "name": "err" }, { "name": "cxl" }]);
    let error = compile_zhixu_hook_plan(&parent, Some(&dock_targets), false)
        .expect_err("dangling signalMap key must fail");
    let message = error.to_string();
    assert!(
        message.contains("D006")
            && message
                .contains("key is not a sendSignals signal of stage settlement.execute_payment"),
        "{message}"
    );
}

#[test]
fn rejects_leftover_target_version_field() {
    // target 只携带 zhixu（name 即完整目标引用）。残留 version 键按
    // D002 未知字段硬拒绝——静默忽略会让调用方误以为版本钉扎仍生效。
    let mut parent = parent_settlement_definition(TARGET_UID);
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
fn dock_targets_uid_is_the_resolution_key() {
    // uid 是 linker 的唯一解析键：注册表缺被引用的 uid 即 D008；
    // 注册表内重复 uid 是注入数据错误，响亮拒绝。
    let target = target_payment_definition();
    let dock_targets = dock_targets_for(&target);
    let plan = compile_zhixu_hook_plan(
        &parent_settlement_definition(TARGET_UID),
        Some(&dock_targets),
        false,
    )
    .expect("honest dock targets link");
    assert_eq!(plan["dockRoutes"][0]["target"]["uid"], json!(TARGET_UID));

    let mut renamed = dock_targets_for(&target);
    renamed[0]["uid"] = json!(UNKNOWN_TARGET);
    let error = compile_zhixu_hook_plan(
        &parent_settlement_definition(TARGET_UID),
        Some(&renamed),
        false,
    )
    .expect_err("dock targets without the referenced uid must fail");
    let message = error.to_string();
    assert!(
        message.contains("D008")
            && message.contains("no published definition with uid")
            && message.contains(TARGET_UID),
        "{message}"
    );

    let mut duplicated = dock_targets_for(&target);
    let entry = duplicated[0].clone();
    duplicated.as_array_mut().unwrap().push(entry);
    let error =
        dock::parse_dock_targets(&duplicated).expect_err("duplicate dock target uids must fail");
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
                    "sendSignals": [
        { "name": "str" },
        { "name": "cmp" }
        ],
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
                    // 自发种子入口钩子，避免零 hook 阶段被物化门拒绝。
                    "receiveSignals": { "PUBLISH": "other::anchor_task.emit.seed" },
                    "sendSignals": [
        { "name": "cmp" },
        { "name": "seed" }
        ],
                    "executor": { "supplierType": "organization", "supplierID": "other-org" }
                }),
    ];
    if with_anchor {
        stages.push(json!({
            "name": "anchor",
            "source": "anchoredcls",
            "mint": "per-fact",
            "receiveSignals": { "SPAWN": "::ANCHOR(@other::anchor_task.emit.cmp)" },
            "sendSignals": [{ "name": "str" }],
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
        // 37 字节即拒：壳层上限与 hook 标头/订阅目标的 source 类上限
        // 收敛到 36（37-100 字节的 source 是"声明即死"命名空间）。
        ("oversized", "s".repeat(37)),
    ] {
        let mut parent = parent_settlement_definition(TARGET_UID);
        parent["spec"]["taskPatterns"][1]["stages"][0]["source"] = json!(source);
        let error = compile_zhixu_hook_plan(&parent, None, false)
            .expect_err("invalid stage source must be rejected");
        assert!(
            error.to_string().contains(".source"),
            "source {label:?}: {error}"
        );
    }
    // 36 字节边界恰好放行（超出在形状层拒绝，边界值走到后续 link 才失败）。
    let mut parent = parent_settlement_definition(TARGET_UID);
    parent["spec"]["taskPatterns"][1]["stages"][0]["source"] = json!("s".repeat(36));
    let error = compile_zhixu_hook_plan(&parent, None, false)
        .expect_err("boundary source must pass shape checks and fail later on linking");
    assert!(
        !error.to_string().contains("exceeds 36 bytes"),
        "36-byte source is legal: {error}"
    );
    // 37 字节：形状层响亮拒绝并指向 36 上限。
    let mut parent = parent_settlement_definition(TARGET_UID);
    parent["spec"]["taskPatterns"][1]["stages"][0]["source"] = json!("s".repeat(37));
    let error = compile_zhixu_hook_plan(&parent, None, false)
        .expect_err("37-byte source must be rejected at the shape layer");
    assert!(
        error.to_string().contains("exceeds 36 bytes"),
        "37-byte source rejection must cite the 36-byte cap: {error}"
    );
}

#[test]
fn rejects_unknown_spec_and_executor_fields() {
    // spec 顶层未知字段（含不受支持的 trigger/externalSignals）不被静默
    // 忽略/透传（对齐 Go 入口 decodeObjectStrict）。
    let mut parent = parent_settlement_definition(TARGET_UID);
    parent["spec"]["trigger"] = json!([]);
    let error = compile_zhixu_hook_plan(&parent, None, false)
        .expect_err("unsupported spec-level trigger key must fail");
    assert!(
        error.to_string().contains("unknown field `trigger`"),
        "{error}"
    );

    let mut parent = parent_settlement_definition(TARGET_UID);
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
    let mut parent = parent_settlement_definition(TARGET_UID);
    parent["spec"]["taskPatterns"][1]["stages"][0]["executor"]["handlerType"] = json!("http");
    let error = compile_zhixu_hook_plan(&parent, None, false)
        .expect_err("unknown executor field must fail");
    assert!(
        error.to_string().contains("unknown field `handlerType`"),
        "{error}"
    );

    // metadata 层未知字段（如 description）同样拒绝。
    let mut parent = parent_settlement_definition(TARGET_UID);
    parent["metadata"]["description"] = json!("demo");
    let error = compile_zhixu_hook_plan(&parent, None, false)
        .expect_err("metadata-level unknown field must fail");
    assert!(
        error.to_string().contains("unknown field `description`"),
        "{error}"
    );

    // metadata.uid 不是作者可写字段，出现即未知字段响亮拒绝。
    let mut parent = parent_settlement_definition(TARGET_UID);
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
        assert!(error.to_string().contains("D013"), "{expression}: {error}");
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
    let error =
        compile_zhixu_hook_plan(&target, None, true).expect_err("unknown output signal must fail");
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
    let error =
        compile_zhixu_hook_plan(&target, None, true).expect_err("invalid interface name must fail");
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
        let error =
            compile_zhixu_hook_plan(&target, None, true).expect_err("orderModes {label} must fail");
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
        error.to_string().contains("D025") && error.to_string().contains("at least one input port"),
        "{}",
        error.to_string()
    );
}

#[test]
fn rejects_config_mapping_violations() {
    // D004：order.mode 闭集。
    let mut parent = parent_settlement_definition(TARGET_UID);
    parent["spec"]["taskPatterns"][1]["stages"][0]["executor"]["zhixuExecutorConfig"]
        .as_object_mut()
        .unwrap()["order"]["mode"] = json!("reused");
    let error =
        compile_zhixu_hook_plan(&parent, None, false).expect_err("unknown order mode must fail");
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
    let mut parent = parent_settlement_definition(TARGET_UID);
    parent["spec"]["taskPatterns"][1]["stages"][0]["executor"]["supplierID"] = json!("  ");
    let error = compile_zhixu_hook_plan(&parent, None, false)
        .expect_err("empty supplierID on a zhixu executor must fail");
    assert!(
        error.to_string().contains("D001") && error.to_string().contains("supplierID is forbidden"),
        "{}",
        error.to_string()
    );

    // D019：无任何映射。
    let mut parent = parent_settlement_definition(TARGET_UID);
    let config = parent["spec"]["taskPatterns"][1]["stages"][0]["executor"]["zhixuExecutorConfig"]
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
    let mut parent = parent_settlement_definition(TARGET_UID);
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
    let mut parent = parent_settlement_definition(TARGET_UID);
    parent["spec"]["taskPatterns"][1]["stages"][0]["executor"]["zhixuExecutorConfig"]
        .as_object_mut()
        .unwrap()
        .remove("inputMap");
    let error = compile_zhixu_hook_plan(&parent, None, false)
        .expect_err("new mode without an input binding must fail");
    assert!(error.to_string().contains("D010"), "{}", error.to_string());

    // D003：target.zhixu 非 uid 形态。
    let mut parent = parent_settlement_definition(TARGET_UID);
    parent["spec"]["taskPatterns"][1]["stages"][0]["executor"]["zhixuExecutorConfig"]
        .as_object_mut()
        .unwrap()["target"]["zhixu"] = json!("Payment_Execution");
    let error =
        compile_zhixu_hook_plan(&parent, None, false).expect_err("non-uid target must fail");
    assert!(
        error.to_string().contains("D003")
            && error
                .to_string()
                .contains("content-derived uid, matching ^zx-[0-9a-f]{32}$"),
        "{}",
        error.to_string()
    );
}

#[test]
fn accepts_dynamic_target_null_for_parse_only_compilation() {
    // target:null 表示运行时选择补齐：本地校验通过，
    // 无 dockTargets 的 parse 编译可过；静态目标缺失注册目标才是
    // UNRESOLVED_DOCK_TARGET（见 rejects_parent_without_dock_targets）。
    let mut parent = parent_settlement_definition(TARGET_UID);
    parent["spec"]["taskPatterns"][1]["stages"][0]["executor"]["zhixuExecutorConfig"]
        .as_object_mut()
        .unwrap()["target"] = json!(null);
    let parsed = compile_zhixu_hook_plan(&parent, None, true)
        .expect("parse-only compilation accepts a null target");
    assert_eq!(parsed["dockRoutes"].as_array().unwrap().len(), 0);
}

/// target:null 的父定义（无 dockTargets）：本地声明面完整进产物，两个
/// 可运行 target 都放行——链轨以同一 unresolvedDockRoutes 声明面承接
/// （唯一保留的动态拒绝是 orderMode=new，TS onchain 边界按
/// UNRESOLVED_DOCK_MODE 口径），云轨运行时由选择记录补齐。
fn null_target_parent() -> Value {
    let mut parent = parent_settlement_definition(TARGET_UID);
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
    let plan =
        compile_zhixu_hook_plan(&parent, None, false).expect("hook_plan compiles a null target");
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
    let dock_targets = dock_targets_for(&target);
    let artifact = compile_cloud_artifact(
        &parent_settlement_definition(TARGET_UID),
        Some(&dock_targets),
        false,
    )
    .expect("static-only parent compiles");
    assert!(artifact.get("unresolvedDockRoutes").is_none());
}

#[test]
fn dock_targets_present_null_target_route_stays_unresolved() {
    // dockTargets 在场时 null-target route 不进 link（不报 D008），静态
    // route 照常解析：两类 route 各归其位。
    let target = target_payment_definition();
    let dock_targets = dock_targets_for(&target);
    let mut parent = null_target_parent();
    parent["spec"]["taskPatterns"][1]["stages"]
        .as_array_mut()
        .unwrap()
        .push(json!({
            "name": "static_dock",
            "source": "buyer",
            "receiveSignals": { "START": "buyer::checkout.confirm.cmp" },
            "sendSignals": [{ "name": "str" }],
            "executor": {
                "supplierType": "zhixu",
                "zhixuExecutorConfig": {
                    "target": { "zhixu": TARGET_UID },
                    "interface": "payment_service",
                    "order": { "mode": "new" },
                    "inputMap": { "START": "execute" },
                    "signalMap": { "str": "started" }
                }
            }
        }));
    let artifact = compile_cloud_artifact(&parent, Some(&dock_targets), false)
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
    let config = parent["spec"]["taskPatterns"][1]["stages"][0]["executor"]["zhixuExecutorConfig"]
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
        .map(|(index, channel)| (channel.clone(), Value::String(format!("p{index}"))))
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
                        "sendSignals": [{ "name": "seed" }],
                        "executor": { "supplierType": "organization", "supplierID": "buyer-app" }
                    },
                    { "name": "dock", "source": "buyer",
                      "receiveSignals": receive_signals,
                      "sendSignals": [{ "name": "out" }],
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
    let config = parent["spec"]["taskPatterns"][0]["stages"][1]["executor"]["zhixuExecutorConfig"]
        .as_object_mut()
        .unwrap();
    config["inputMap"].as_object_mut().unwrap().remove("CH8");
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
    let mut parent = parent_settlement_definition(TARGET_UID);
    parent["spec"]["taskPatterns"][0]["stages"][0]["executor"]["supplierType"] = json!("org");
    let error = compile_zhixu_hook_plan(&parent, None, false)
        .expect_err("unknown supplierType must fail before entering executorRoutes");
    assert!(
        error.to_string().contains("supplierType must be one of"),
        "{}",
        error
    );

    // 闭集精确匹配（Go 侧严格枚举闸同口径）：带首尾空白的变体按闭集
    // 外拒绝——trim 放行会让产物携带原文、比对侧按精确值分叉。
    for supplier_type in [" organization ", "zhixu ", " individual"] {
        let mut parent = parent_settlement_definition(TARGET_UID);
        parent["spec"]["taskPatterns"][0]["stages"][0]["executor"]["supplierType"] =
            json!(supplier_type);
        let error = compile_zhixu_hook_plan(&parent, None, true)
            .err()
            .unwrap_or_else(|| panic!("{supplier_type:?} must be rejected"));
        assert!(
            error.to_string().contains("supplierType must be one of")
                && error.to_string().contains("whitespace variants rejected"),
            "{supplier_type:?}: {error}"
        );
    }

    // 闭集内取值（zhixu 形态由 dock 系列测试覆盖）照常编译。
    for supplier_type in ["individual", "organization"] {
        let mut parent = parent_settlement_definition(TARGET_UID);
        parent["spec"]["taskPatterns"][0]["stages"][0]["executor"]["supplierType"] =
            json!(supplier_type);
        compile_zhixu_hook_plan(&parent, None, true)
            .unwrap_or_else(|err| panic!("{supplier_type} must pass the closed set: {err}"));
    }
}

#[test]
fn rejects_file_resources_outside_the_closed_file_type_set() {
    // fileResources 条目随 route 进链上承诺（resourcesHash）：fileType
    // 闭集 {local, http, txcloud, plain_text} 之外（含带空白变体与缺失）
    // 都在编译期拒绝，不静默烧进承诺。
    for (label, file_type) in [
        ("misspelled", json!("locale")),
        ("padded", json!(" local ")),
        ("missing", Value::Null),
    ] {
        let mut parent = parent_settlement_definition(TARGET_UID);
        parent["spec"]["taskPatterns"][0]["stages"][0]["fileResources"] =
            json!({ "contract_template": { "fileType": file_type } });
        let error = compile_zhixu_hook_plan(&parent, None, true)
            .err()
            .unwrap_or_else(|| panic!("{label} fileType must be rejected"));
        assert!(
            error.to_string().contains("fileResources")
                && error.to_string().contains("fileType must be one of")
                && error.to_string().contains("whitespace variants rejected"),
            "{label}: {error}"
        );
    }

    // 闭集内取值照常编译。
    let mut parent = parent_settlement_definition(TARGET_UID);
    parent["spec"]["taskPatterns"][0]["stages"][0]["fileResources"] = json!({
        "contract_template": { "fileType": "local", "localFile": { "path": "./t.md" } }
    });
    compile_zhixu_hook_plan(&parent, None, true)
        .unwrap_or_else(|err| panic!("closed-set fileType must compile: {err}"));
}

#[test]
fn rejects_selectable_resource_outside_the_closed_file_type_set() {
    // executor.selectableResource 与 fileResources 同为 FileResource 面，
    // 且整体经 executorHash 进链上承诺：词表外 fileType（含带空白变体）
    // 与非 map 形态都在编译期拒绝。
    for (label, value) in [
        (
            "misspelled",
            json!({ "dataset": { "fileType": "tx_cloud" } }),
        ),
        ("padded", json!({ "dataset": { "fileType": " http" } })),
        ("not-a-map", json!(["dataset"])),
    ] {
        let mut parent = parent_settlement_definition(TARGET_UID);
        parent["spec"]["taskPatterns"][0]["stages"][0]["executor"]["selectableResource"] = value;
        let error = compile_zhixu_hook_plan(&parent, None, true)
            .err()
            .unwrap_or_else(|| panic!("{label} selectableResource must be rejected"));
        assert!(
            error.to_string().contains("selectableResource")
                && (label == "not-a-map" || error.to_string().contains("fileType must be one of")),
            "{label}: {error}"
        );
    }

    // 闭集内取值照常编译。
    let mut parent = parent_settlement_definition(TARGET_UID);
    parent["spec"]["taskPatterns"][0]["stages"][0]["executor"]["selectableResource"] = json!({
        "dataset": { "fileType": "plain_text", "plainText": { "content": "x" } }
    });
    compile_zhixu_hook_plan(&parent, None, true)
        .unwrap_or_else(|err| panic!("closed-set selectableResource must compile: {err}"));
}

#[test]
fn rejects_non_zhixu_executor_with_delegation_config() {
    // organization executor 携带完整 zhixuExecutorConfig：编译期响亮拒绝
    // （D001"拼错字段同罪"口径）——静默放行会把委托配置原文烧进
    // executorRoutes（链上承诺面），"既静态执行者又委托对接"是矛盾声明。
    let mut parent = parent_settlement_definition(TARGET_UID);
    parent["spec"]["taskPatterns"][0]["stages"][0]["executor"]["zhixuExecutorConfig"] = json!({
        "target": { "zhixu": TARGET_UID },
        "interface": "payment_service",
        "order": { "mode": "new" },
        "inputMap": { "PLACE": "execute" },
        "signalMap": { "cmp": "completed" }
    });
    for target in ["hook_plan", "cloud"] {
        let result = if target == "hook_plan" {
            compile_zhixu_hook_plan(&parent, None, true)
        } else {
            compile_cloud_artifact(&parent, None, true)
        };
        let error = result.err().unwrap_or_else(|| {
            panic!("{target}: must reject organization executor with delegation config")
        });
        assert!(
            error.to_string().contains("D002")
                && error
                    .to_string()
                    .contains("zhixuExecutorConfig is only valid when supplierType is zhixu"),
            "{target}: {error}"
        );
    }
}

#[test]
fn rejects_link_violations() {
    let target = target_payment_definition();

    // D008：目标不在注册表（被引用 uid 无对应条目）。
    let mut unregistered = dock_targets_for(&target);
    unregistered[0]["uid"] = json!(UNKNOWN_TARGET);
    let error = compile_zhixu_hook_plan(
        &parent_settlement_definition(TARGET_UID),
        Some(&unregistered),
        false,
    )
    .expect_err("missing target must fail");
    assert!(error.to_string().contains("D008"), "{}", error.to_string());

    // D009：引用不在所选接口上的输出端口（cancelled 只在 evidence 上）。
    let mut parent = parent_settlement_definition(TARGET_UID);
    parent["spec"]["taskPatterns"][1]["stages"][0]["executor"]["zhixuExecutorConfig"]
        .as_object_mut()
        .unwrap()["signalMap"]["cxl"] = json!("cancelled");
    let dock_targets = dock_targets_for(&target);
    let error = compile_zhixu_hook_plan(&parent, Some(&dock_targets), false)
        .expect_err("unknown output port must fail");
    assert!(
        error.to_string().contains("D009") && error.to_string().contains("payment_service"),
        "{}",
        error.to_string()
    );

    // D009：接口不存在。
    let mut parent = parent_settlement_definition(TARGET_UID);
    parent["spec"]["taskPatterns"][1]["stages"][0]["executor"]["zhixuExecutorConfig"]
        .as_object_mut()
        .unwrap()["interface"] = json!("payment_archive");
    let error = compile_zhixu_hook_plan(&parent, Some(&dock_targets), false)
        .expect_err("unknown interface must fail");
    assert!(
        error.to_string().contains("D009") && error.to_string().contains("payment_archive"),
        "{}",
        error.to_string()
    );

    // D020：mode 不在接口 orderModes 内（payment_service 只允许 new）。
    let mut parent = parent_settlement_definition(TARGET_UID);
    parent["spec"]["taskPatterns"][1]["stages"][0]["executor"]["zhixuExecutorConfig"]
        .as_object_mut()
        .unwrap()["order"]["mode"] = json!("existing");
    let error = compile_zhixu_hook_plan(&parent, Some(&dock_targets), false)
        .expect_err("mode not allowed by interface must fail");
    assert!(
        error.to_string().contains("D020") && error.to_string().contains("allows orderModes"),
        "{}",
        error.to_string()
    );

    // D015：route 目标定义的 executor 静态出边自指（启动图自环，route 边
    // 使环从本地可达）。
    let self_edge = dock_targets_for(&with_static_dock_edge(target.clone(), Some(TARGET_UID)));
    let error = compile_zhixu_hook_plan(
        &parent_settlement_definition(TARGET_UID),
        Some(&self_edge),
        false,
    )
    .expect_err("self-referencing dock edge must fail");
    assert!(
        error.to_string().contains("D015") && error.to_string().contains("cycle"),
        "{}",
        error.to_string()
    );

    // D015：route 目标为注册表条目、该条目的静态出边回指 route 目标
    // ——环经 route 边与声明边闭合（本地的参与节点是 route 边的起点）。
    let mutual = json!([
        { "uid": TARGET_UID,
          "definition": with_static_dock_edge(target.clone(), Some(SELF_TARGET_UID)) },
        chain_link_entry(SELF_TARGET_UID.to_string(), Some(TARGET_UID)),
    ]);
    let error = compile_zhixu_hook_plan(
        &parent_settlement_definition(TARGET_UID),
        Some(&mutual),
        false,
    )
    .expect_err("route plus dock-targets edge cycle must fail");
    assert!(error.to_string().contains("D015"), "{}", error.to_string());
}

#[test]
fn rejects_startup_depth_beyond_limit_via_dock_edges() {
    // 链长 = settlement(1) + TARGET_UID(1) + mid-1..mid-7(7) = 9
    // > MAX_DOCK_DEPTH(8)；深度按定义 executor 声明的静态 uid 边累计。
    let mut deep = dock_targets_for(&with_static_dock_edge(
        target_payment_definition(),
        Some(&mid_uid(1)),
    ));
    for index in 1..7 {
        deep.as_array_mut()
            .unwrap()
            .push(chain_link_entry(mid_uid(index), Some(&mid_uid(index + 1))));
    }
    // 尾节点无静态出边：executor 不携带 target。
    deep.as_array_mut()
        .unwrap()
        .push(chain_link_entry(mid_uid(7), None));
    let error = compile_zhixu_hook_plan(
        &parent_settlement_definition(TARGET_UID),
        Some(&deep),
        false,
    )
    .expect_err("startup depth beyond MAX_DOCK_DEPTH must fail");
    assert!(
        error.to_string().contains("D015") && error.to_string().contains("depth"),
        "{}",
        error.to_string()
    );

    // 截短到限内（settlement + TARGET_UID + mid-1..mid-4 = 6）同
    // 一父定义照常编译。
    let mut shallow = dock_targets_for(&with_static_dock_edge(
        target_payment_definition(),
        Some(&mid_uid(1)),
    ));
    for index in 1..4 {
        shallow
            .as_array_mut()
            .unwrap()
            .push(chain_link_entry(mid_uid(index), Some(&mid_uid(index + 1))));
    }
    shallow
        .as_array_mut()
        .unwrap()
        .push(chain_link_entry(mid_uid(4), None));
    compile_zhixu_hook_plan(
        &parent_settlement_definition(TARGET_UID),
        Some(&shallow),
        false,
    )
    .expect("in-limit startup depth compiles");
}

#[test]
fn rejects_dock_startup_graph_cycles_at_dock_targets_level() {
    // D015 同源防线前移：dockTargets 声明面可成的环在
    // link 期环检测一律拒绝——互为目标两节点环、经中间定义三节点环、
    // 自指 dockEdges 自环，不要求本地定义参与成环。
    let target = target_payment_definition();
    let parent = parent_settlement_definition(TARGET_UID);
    let parent_with_route_to = |target_uid: &str| {
        let mut parent = parent_settlement_definition(TARGET_UID);
        parent["spec"]["taskPatterns"][1]["stages"][0]["executor"]["zhixuExecutorConfig"]
            .as_object_mut()
            .unwrap()["target"]["zhixu"] = json!(target_uid);
        parent
    };

    // A→B→A：注册表内两定义互为目标（本地未参与成环也要拒绝）。
    let mutual = json!([
        { "uid": TARGET_UID,
          "definition": with_static_dock_edge(target.clone(), Some(&mid_uid(1))) },
        chain_link_entry(mid_uid(1), Some(TARGET_UID)),
    ]);
    let error = compile_zhixu_hook_plan(&parent, Some(&mutual), false)
        .expect_err("mutual two-definition cycle must fail");
    assert!(
        error.to_string().contains("D015") && error.to_string().contains("cycle"),
        "{error}"
    );

    // A→B→C→A：注册表内三定义的静态出边成环（本地 route 边同时指向
    // 环成员 A，环成员与回指形态照常报全）。
    let three_node = json!([
        { "uid": TARGET_UID,
          "definition": with_static_dock_edge(target.clone(), Some(&mid_uid(2))) },
        chain_link_entry(mid_uid(2), Some(&mid_uid(3))),
        chain_link_entry(mid_uid(3), Some(TARGET_UID)),
    ]);
    let error = compile_zhixu_hook_plan(&parent, Some(&three_node), false)
        .expect_err("three-definition cycle must fail");
    let message = error.to_string();
    // 环路径的起点按 BTreeMap 节点序确定（mid-2 的 uid 最小），断言钉
    // 完整三段回指形态（环成员 + 起点回指，walk 顺序确定）。
    assert!(
        message.contains("D015")
            && message.contains(&format!(
                "{} -> {} -> {} -> {}",
                mid_uid(2),
                mid_uid(3),
                TARGET_UID,
                mid_uid(2)
            )),
        "{message}"
    );

    // A→A：注册表定义的静态出边自指（自环）。route 目标定义出边自指的
    // 形态由 rejects_link_violations 覆盖。
    let self_edge = dock_targets_for(&with_static_dock_edge(target.clone(), Some(TARGET_UID)));
    let error = compile_zhixu_hook_plan(&parent_with_route_to(TARGET_UID), Some(&self_edge), false)
        .expect_err("dock-targets self-edge cycle must fail");
    assert!(
        error.to_string().contains("D015")
            && error
                .to_string()
                .contains(&format!("{TARGET_UID} -> {TARGET_UID}")),
        "{error}"
    );
}

#[test]
fn cloud_artifact_uses_resolved_routes() {
    let target = target_payment_definition();
    let dock_targets = dock_targets_for(&target);
    let artifact = compile_cloud_artifact(
        &parent_settlement_definition(TARGET_UID),
        Some(&dock_targets),
        false,
    )
    .expect("cloud artifact compiles with dock targets");
    assert_eq!(artifact["schemaVersion"], "uvp.cloudArtifact.v4");
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
        .push(json!({ "name": "" }));
    for target in ["hook_plan", "cloud"] {
        let error = (if target == "hook_plan" {
            compile_zhixu_hook_plan(&empty, None, true)
        } else {
            compile_cloud_artifact(&empty, None, true)
        })
        .expect_err("empty sendSignal must fail");
        assert!(error.to_string().contains("D026"), "{target}: {error}");
    }

    let mut duplicate = target_payment_definition();
    duplicate["spec"]["taskPatterns"][0]["stages"][0]["sendSignals"]
        .as_array_mut()
        .unwrap()
        .push(json!({ "name": "str" }));
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
fn send_signal_declarations_use_a_single_exact_surface() {
    // 声明面单一精确口径：declared 与 capability 同一原文（不 trim），
    // 裸名与 canonical 名都按信号名同款标识符文法闸——空白、非标识符
    // 字符、数字开头、错误的段数在此响亮拒绝（capability 侧 trim 归一
    // 会产出死能力并与其它声明撞 duplicate）。
    for (label, signal) in [
        ("leading whitespace bare", " str"),
        ("target separator form", "Seller::NOTED"),
        ("trailing whitespace target", "buyer::cmp "),
        ("whitespace inside target", "buy er::cmp"),
        ("double separator", "a::b::c"),
        ("empty signal half", "buyer::"),
        ("empty target half", "::cmp"),
        ("digit-leading bare", "1abc"),
        ("digit-leading target", "1buyer::cmp"),
        ("two-part canonical", "task.stage"),
        ("four-part canonical", "a.b.c.d"),
    ] {
        let mut definition = target_payment_definition();
        definition["spec"]["taskPatterns"][0]["stages"][0]["sendSignals"]
            .as_array_mut()
            .unwrap()
            .push(json!({ "name": signal }));
        let error = compile_zhixu_hook_plan(&definition, None, true)
            .expect_err(&format!("{label} ({signal:?}) must be rejected"));
        assert!(
            error.to_string().contains(".sendSignals"),
            "{label} ({signal:?}): {error}"
        );
    }

    // "str" 与 " str" 不再撞 duplicate 误判：" str" 在形态闸被拒绝。
    let mut definition = target_payment_definition();
    definition["spec"]["taskPatterns"][0]["stages"][0]["sendSignals"]
        .as_array_mut()
        .unwrap()
        .push(json!({ "name": " str" }));
    let error = compile_zhixu_hook_plan(&definition, None, true)
        .expect_err("whitespace-padded signal must be rejected by the shape gate");
    assert!(
        !error.to_string().contains("duplicate capability"),
        "whitespace variant must fail on charset, not duplicate: {error}"
    );
}

#[test]
fn canonical_send_signals_are_explicit_self_references() {
    // 互锁修复：三段式 canonical 声明只是显式自指形态——前缀必须落在
    // 声明阶段自身；引用存在性 / capability 去重 / D014 一律按展开后
    // 的全名统一比较，canonical 声明不再"声明即死"。

    // ① canonical 自指声明通过编译，settle 的裸名形态引用
    // （payment::payment_flow.init.str）命中同一全名——修复前引用
    // 校验只比第三段裸名，canonical 声明被判悬空引用。
    let mut definition = target_payment_definition();
    definition["spec"]["taskPatterns"][0]["stages"][0]["sendSignals"] =
        json!([{ "name": "payment_flow.init.str" }]);
    let plan = compile_zhixu_hook_plan(&definition, None, true)
        .expect("canonical self-reference declaration compiles");
    let capability = plan["signalCapabilities"]
        .as_array()
        .unwrap()
        .iter()
        .find(|capability| capability["declaredSignal"] == json!("payment_flow.init.str"))
        .expect("capability carries the canonical declaration");
    assert_eq!(
        capability["targetSignalName"],
        json!("payment_flow.init.str")
    );
    assert_eq!(capability["targetSource"], json!("payment"));
    assert_eq!(capability["targetOrderRelation"], json!("current"));

    // ② 双属主反例：阶段 A 裸名 + 阶段 B 三段式指向 A 的命名空间 →
    // 相同 (targetSource, signal) capability 双属主（绕过一事一能力、
    // 链上 _signalStageId 归属二义）——canonical 分支的前缀闸拒绝。
    let mut dual = target_payment_definition();
    dual["spec"]["taskPatterns"]
        .as_array_mut()
        .unwrap()
        .push(json!({
            "name": "mirror",
            "stages": [{
                "name": "echo",
                "source": "payment",
                "receiveSignals": { "GO": "payment::payment_flow.init.str" },
                "sendSignals": [{ "name": "payment_flow.init.str" }],
                "executor": { "supplierType": "organization", "supplierID": "payment-gateway" }
            }]
        }));
    let error = compile_zhixu_hook_plan(&dual, None, true)
        .expect_err("canonical declaration addressing another stage must fail");
    assert!(
        error
            .to_string()
            .contains("does not address the declaring stage"),
        "unexpected error: {error}"
    );
    let error = compile_cloud_artifact(&dual, None, true)
        .expect_err("cloud target must reject the same shape");
    assert!(
        error
            .to_string()
            .contains("does not address the declaring stage"),
        "cloud: {error}"
    );

    // ③ 裸名与自指 canonical 是同一 capability 的两种写法：同报
    // duplicate（展开后的全名统一比较）。
    let mut both = target_payment_definition();
    both["spec"]["taskPatterns"][0]["stages"][0]["sendSignals"] =
        json!([{ "name": "str" }, { "name": "payment_flow.init.str" }]);
    let error = compile_zhixu_hook_plan(&both, None, true)
        .expect_err("bare + canonical self-reference must collide");
    assert!(
        error.to_string().contains("duplicate capability"),
        "unexpected error: {error}"
    );

    // ④ D014：输出端口引用 canonical 声明的信号按全名解析照常编译
    // （target_payment_definition 的 started 端口即
    // payment::payment_flow.init.str）。
    let mut target = target_payment_definition();
    target["spec"]["taskPatterns"][0]["stages"][0]["sendSignals"] =
        json!([{ "name": "payment_flow.init.str" }]);
    compile_zhixu_hook_plan(&target, None, true)
        .expect("D014 resolves canonical declarations by expanded full name");
}

#[test]
fn nucleation_id_must_be_non_blank() {
    // 文法两册 §2.2：spec.nucleation.id 必填——字段缺失由 serde 必填闸
    // 拒绝（presence），空白值在此响亮拒绝（与 stage.source 同纪律，
    // 不 trim 归一放行）；云/链两 target 同口径。
    for blank in ["", "   ", "\t"] {
        let mut definition = target_payment_definition();
        definition["spec"]["nucleation"]["id"] = json!(blank);
        let error = compile_zhixu_hook_plan(&definition, None, true)
            .expect_err("blank nucleation.id must fail");
        assert!(
            error
                .to_string()
                .contains("spec.nucleation.id must be non-empty"),
            "unexpected error: {error}"
        );
        let error = compile_cloud_artifact(&definition, None, true)
            .expect_err("cloud target must reject the same shape");
        assert!(
            error
                .to_string()
                .contains("spec.nucleation.id must be non-empty"),
            "cloud: {error}"
        );
    }
    let mut missing = target_payment_definition();
    missing["spec"]
        .as_object_mut()
        .unwrap()
        .remove("nucleation");
    let error = compile_zhixu_hook_plan(&missing, None, true)
        .expect_err("missing nucleation must fail at the serde required-field gate");
    assert!(
        error.to_string().contains("nucleation"),
        "unexpected error: {error}"
    );
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
                        "sendSignals": [{ "name": "executor_selected" }],
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
    // name 是作者技术标签，slug 形态（小写开头，小写字母/数字/
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
    compile_zhixu_hook_plan(&definition, None, true).expect("slug-shaped metadata.name compiles");
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
                        "sendSignals": [{ "name": "executor_selected" }],
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
                        "sendSignals": [{ "name": "executor_selected" }],
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
            && message.contains("must not contain '.', '#' or whitespace"),
        "unexpected error: {message}"
    );

    let mut definition = target_payment_definition();
    definition["spec"]["taskPatterns"][0]["stages"][0]["receiveSignals"]["BAD#KEY"] =
        json!("payment::payment_flow.init.execute");
    let error = compile_zhixu_hook_plan(&definition, None, true)
        .expect_err("receiveSignals key containing '#' must fail");
    assert!(
        error
            .to_string()
            .contains("must not contain '.', '#' or whitespace"),
        "unexpected error: {error}"
    );

    // 含空白的通道名与 hook-dsl validate_hook_name 同口径拒绝：通道名
    // 进 hookId（stage#hook_name），两侧必须逐字节一致，不 trim 归一。
    let mut definition = target_payment_definition();
    definition["spec"]["taskPatterns"][0]["stages"][0]["receiveSignals"]["BAD KEY"] =
        json!("payment::payment_flow.init.execute");
    let error = compile_zhixu_hook_plan(&definition, None, true)
        .expect_err("receiveSignals key containing whitespace must fail");
    assert!(
        error
            .to_string()
            .contains("must not contain '.', '#' or whitespace"),
        "unexpected error: {error}"
    );
}

#[test]
fn send_signal_combined_length_counts_canonical_full_name_exactly() {
    // 对拍 Go validateDDLDimensions（dimensions.go）：canonical 三段式声明
    // 本身即全名（task.stage.signal），按原文精确计长；裸名才拼 stage
    // 前缀。三段式若再拼一次 stage 前缀，会把 task.stage 段重复计入、
    // 误伤恰 100 字节列宽边界内的合法 canonical 名，两侧结论必须一致。
    let stage_name = "s".repeat(48);
    let canonical_at_limit = format!("t.{}.{}", stage_name, "x".repeat(49));
    assert_eq!(canonical_at_limit.len(), 100);
    let canonical_over_limit = format!("t.{}.{}", stage_name, "x".repeat(50));
    assert_eq!(canonical_over_limit.len(), 101);
    // stage 标识符 "t.<48 字节>" = 50 字节：裸名组合边界 50+1+49=100。
    let bare_at_limit = "y".repeat(49);
    let bare_over_limit = "y".repeat(50);

    let definition_with = |signals: Vec<String>| {
        json!({
            "apiVersion": "uvp/v0",
            "kind": "Zhixu",
            "metadata": { "name": "dimension-parity" },
            "spec": {
                "platform": { "type": "cloud" },
                "nucleation": { "id": "core" },
                "taskPatterns": [{
                    "name": "t",
                    "stages": [{
                        "name": stage_name,
                        "source": "parity",
                        "receiveSignals": {
                            "PUBLISH": format!("parity::t.{stage_name}.seed")
                        },
                        "sendSignals": signals
                            .iter()
                            .map(|name| json!({ "name": name }))
                            .collect::<Vec<_>>(),
                        "executor": {
                            "supplierType": "organization",
                            "supplierID": "parity-executor"
                        }
                    }]
                }]
            }
        })
    };

    // 恰 100 字节的 canonical 三段式：列宽边界内，两个 target 都放行
    // （Go 侧 full = len(signal) = 100，同结论）。
    let signals = vec!["seed".to_string(), canonical_at_limit];
    compile_zhixu_hook_plan(&definition_with(signals.clone()), None, true)
        .expect("100-byte canonical sendSignal sits exactly at the column width");
    compile_cloud_artifact(&definition_with(signals), None, true)
        .expect("100-byte canonical sendSignal sits exactly at the column width (cloud)");

    // 101 字节的 canonical 三段式：两侧都拒。
    let signals = vec!["seed".to_string(), canonical_over_limit];
    for target in ["hook_plan", "cloud"] {
        let result = if target == "hook_plan" {
            compile_zhixu_hook_plan(&definition_with(signals.clone()), None, true)
        } else {
            compile_cloud_artifact(&definition_with(signals.clone()), None, true)
        };
        let error = result.expect_err("101-byte canonical sendSignal exceeds the column width");
        assert!(
            error.to_string().contains("exceeds 100 bytes combined"),
            "{target}: {error}"
        );
    }

    // 裸名（无 '.'）：Go 同款拼 stage 前缀——组合恰 100 放行、101 拒。
    let signals = vec!["seed".to_string(), bare_at_limit];
    compile_zhixu_hook_plan(&definition_with(signals), None, true)
        .expect("bare name combining to exactly 100 bytes is at the column width");
    let signals = vec!["seed".to_string(), bare_over_limit];
    let error = compile_zhixu_hook_plan(&definition_with(signals), None, true)
        .expect_err("bare name combining past 100 bytes must fail");
    assert!(
        error.to_string().contains("exceeds 100 bytes combined"),
        "{error}"
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

#[test]
fn rejects_two_mint_stages_declaring_the_same_birth_fact() {
    // 一事一单：两个 mint 阶段把同一 (source, signal) 声明为出生入口，
    // 云轨会按阶段各铸一单、链上一事实物化多单——编译期拒绝（含
    // cloud/hook_plan 两个 target）。
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
        (
            "cellar",
            mint_stage_value(
                "cellar",
                "store",
                "cellar",
                json!({ "SPAWN": "::ANCHOR(@producer::dispatch.main.smart_contract)" }),
                &["shelve"],
            ),
        ),
    ]);
    let error = compile_zhixu_hook_plan(&definition, None, true)
        .expect_err("two mint stages on the same birth fact must fail");
    let message = error.to_string();
    assert!(
        message.contains("producer::dispatch.main.smart_contract")
            && message.contains("一事一单：同一事实至多铸一单；多阶段消费请改用 hook 依赖"),
        "unexpected error: {message}"
    );
    let error = compile_cloud_artifact(&definition, None, true)
        .expect_err("cloud target must reject the duplicate birth fact too");
    assert!(
        error
            .to_string()
            .contains("一事一单：同一事实至多铸一单；多阶段消费请改用 hook 依赖"),
        "unexpected error: {error}"
    );
}

#[test]
fn allows_two_mint_stages_with_distinct_birth_facts() {
    // 两个 mint 阶段各自声明不同的出生事实：一事一单未被触碰，照常放行。
    let definition = mint_definitions(&[
        (
            "dispatch",
            emitter_stage_value("dispatch", "main", "producer", &["smart_contract"]),
        ),
        (
            "depot",
            emitter_stage_value("depot", "ship", "distributor", &["manifest"]),
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
        (
            "cellar",
            mint_stage_value(
                "cellar",
                "store",
                "cellar",
                json!({ "SPAWN": "::ANCHOR(@distributor::depot.ship.manifest)" }),
                &["shelve"],
            ),
        ),
    ]);
    compile_zhixu_hook_plan(&definition, None, true)
        .expect("distinct birth facts per mint stage must compile for hook_plan");
    compile_cloud_artifact(&definition, None, true)
        .expect("distinct birth facts per mint stage must compile for cloud");
}

#[test]
fn allows_single_mint_stage_with_multiple_birth_facts() {
    // 单个 mint 阶段声明多个出生事实仍是一阶段一单语义，不在拒绝面。
    let definition = mint_definitions(&[
        (
            "dispatch",
            emitter_stage_value("dispatch", "main", "producer", &["smart_contract"]),
        ),
        (
            "depot",
            emitter_stage_value("depot", "ship", "distributor", &["manifest"]),
        ),
        (
            "orchard",
            mint_stage_value(
                "orchard",
                "retail",
                "buyer",
                json!({
                    "SPAWN": "::ANCHOR(@producer::dispatch.main.smart_contract)",
                    "STORE": "::ANCHOR(@distributor::depot.ship.manifest)"
                }),
                &["ack", "shelve"],
            ),
        ),
    ]);
    compile_zhixu_hook_plan(&definition, None, true)
        .expect("one mint stage with multiple birth facts must compile for hook_plan");
    compile_cloud_artifact(&definition, None, true)
        .expect("one mint stage with multiple birth facts must compile for cloud");
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
        "sendSignals": send_signals
            .iter()
            .map(|name| json!({ "name": name }))
            .collect::<Vec<_>>(),
        "mint": "per-fact",
        "executor": {
            "supplierType": "organization",
            "supplierID": format!("{task}-{name}-executor")
        }
    })
}

fn emitter_stage_value(task: &str, name: &str, source: &str, send_signals: &[&str]) -> Value {
    let mut signals = send_signals.to_vec();
    // 零 hook 阶段不过物化门；seed 是执行者自发入口信号。
    signals.push("seed");
    json!({
        "name": name,
        "source": source,
        "receiveSignals": { "PUBLISH": format!("{source}::{task}.{name}.seed") },
        "sendSignals": signals
            .iter()
            .map(|signal| json!({ "name": signal }))
            .collect::<Vec<_>>(),
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

#[test]
fn rejects_mint_birth_key_colliding_with_dock_entrance_key() {
    // 出生通道键并集查重：mint 阶段的 ANCHOR 出生事实键与本地
    // dockInterface entrance 端口（orderModes 含 new）的 atom 事实键相同
    // ——同一事实同时是 mint 出生入口与 dock 出生锚，outside 开放提交与
    // dock 建单竞争同一事实的出生通道，编译期与协议侧同义拒绝（两个
    // target 同口径）。
    let mut definition = target_payment_definition();
    // payment_service[new].inputs.execute 的 atom 是
    // payment::payment_flow.init.execute——mint 阶段订阅同一事实。
    definition["spec"]["taskPatterns"]
        .as_array_mut()
        .unwrap()
        .push(json!({
            "name": "orchard",
            "stages": [mint_stage_value(
                "orchard",
                "retail",
                "buyer",
                json!({ "SPAWN": "::ANCHOR(@payment::payment_flow.init.execute)" }),
                &["ack"],
            )]
        }));
    let error = compile_zhixu_hook_plan(&definition, None, true)
        .expect_err("mint birth key colliding with a dock entrance key must fail");
    let message = error.to_string();
    assert!(
        message.contains("payment::payment_flow.init.execute")
            && message.contains("dock entrance port(s) (payment_service.execute)")
            && message.contains("mint stage(s) (orchard.retail)")
            && message.contains("出生通道键并集查重"),
        "unexpected error: {message}"
    );
    let error = compile_cloud_artifact(&definition, None, true)
        .expect_err("cloud target must reject the colliding birth channel key too");
    assert!(
        error
            .to_string()
            .contains("出生通道键并集查重：mint 出生键 ∪ dock entrance 键内不得重复"),
        "unexpected error: {error}"
    );

    // 正例对照：mint 出生键指向本 plan 内另一事实（settle.cmp），与
    // entrance 键不相交——并集无重复，照常编译。
    let mut definition = target_payment_definition();
    definition["spec"]["taskPatterns"]
        .as_array_mut()
        .unwrap()
        .push(json!({
            "name": "orchard",
            "stages": [mint_stage_value(
                "orchard",
                "retail",
                "buyer",
                json!({ "SPAWN": "::ANCHOR(@payment::payment_flow.settle.cmp)" }),
                &["ack"],
            )]
        }));
    compile_zhixu_hook_plan(&definition, None, true)
        .expect("a mint birth key disjoint from dock entrance keys must compile");
    compile_cloud_artifact(&definition, None, true)
        .expect("a mint birth key disjoint from dock entrance keys must compile (cloud)");
}

#[test]
fn rejects_dock_entrance_key_published_twice_across_new_interfaces() {
    // 并集查重的 dock∪dock 面：两个 new 型接口的 input 端口引用同一
    // stage 上 atom 相同的两个 mailbox hook——hooks_claimed 只封同一 hook
    // 引用重复发布，不同 hook 名承载同一 atom 的事实键仍构成出生通道键
    // 重复，按并集规则拒绝。
    let mut definition = target_payment_definition();
    // 第二个接收通道与 DOCK_EXECUTE 同 atom。
    definition["spec"]["taskPatterns"][0]["stages"][0]["receiveSignals"]["DOCK_EXECUTE_2"] =
        json!("payment::payment_flow.init.execute");
    definition["spec"]["dockInterface"]["payment_retry"] = json!({
        "orderModes": ["new"],
        "inputs": {
            "execute": { "hook": "payment_flow.init#DOCK_EXECUTE_2" }
        }
    });
    let error = compile_zhixu_hook_plan(&definition, None, true)
        .expect_err("the same entrance fact key published by two new-mode ports must fail");
    let message = error.to_string();
    assert!(
        message.contains("payment::payment_flow.init.execute")
            && message
                .contains("dock entrance port(s) (payment_retry.execute, payment_service.execute)")
            && message.contains("出生通道键并集查重"),
        "unexpected error: {message}"
    );

    // 对照：第二接口是 existing 型——其 input 端口不是出生锚，不进并集。
    let mut definition = target_payment_definition();
    definition["spec"]["taskPatterns"][0]["stages"][0]["receiveSignals"]["DOCK_EXECUTE_2"] =
        json!("payment::payment_flow.init.execute");
    definition["spec"]["dockInterface"]["payment_retry"] = json!({
        "orderModes": ["existing"],
        "inputs": {
            "execute": { "hook": "payment_flow.init#DOCK_EXECUTE_2" }
        }
    });
    compile_zhixu_hook_plan(&definition, None, true)
        .expect("existing-mode input ports are not birth anchors and stay outside the union");
}

// ------------------------------------------------------------------
// 定义/接口/dockTargets 计数上限与错误串截断。
// ------------------------------------------------------------------

/// 计数闸探针基底：shape 层合法的最小 stage（后续校验不跑——计数错误在
/// validate_zhixu_shape 即返回）。
fn counted_stage_value(name: &str, receive_keys: &[&str]) -> Value {
    let receive_signals: serde_json::Map<String, Value> = receive_keys
        .iter()
        .map(|key| (key.to_string(), json!("src::count.task.seed")))
        .collect();
    json!({
        "name": name,
        "source": "src",
        "receiveSignals": receive_signals,
        "sendSignals": [{ "name": "seed" }],
        "executor": { "supplierType": "organization", "supplierID": "counter" }
    })
}

fn counted_definition(tasks: Vec<(String, Vec<Value>)>) -> Value {
    json!({
        "apiVersion": "uvp/v0",
        "kind": "Zhixu",
        "metadata": { "name": "counted" },
        "spec": {
            "platform": { "type": "cloud" },
            "nucleation": { "id": "core" },
            "taskPatterns": tasks
                .into_iter()
                .map(|(name, stages)| json!({ "name": name, "stages": stages }))
                .collect::<Vec<_>>()
        }
    })
}

#[test]
fn rejects_definition_counts_beyond_limits() {
    // taskPatterns > 64。
    let tasks = (0..65)
        .map(|index| (format!("t{index}"), vec![counted_stage_value("s0", &[])]))
        .collect();
    let error = compile_zhixu_hook_plan(&counted_definition(tasks), None, true)
        .expect_err("65 task patterns exceed the cap");
    assert!(
        error.to_string().contains("65 task patterns, limit is 64"),
        "{error}"
    );

    // 摊平阶段总数 > 256（60 task × 5 stage = 300，task 数在限内）。
    let tasks = (0..60)
        .map(|index| {
            (
                format!("t{index}"),
                (0..5)
                    .map(|stage| counted_stage_value(&format!("s{stage}"), &[]))
                    .collect::<Vec<_>>(),
            )
        })
        .collect();
    let error = compile_zhixu_hook_plan(&counted_definition(tasks), None, true)
        .expect_err("300 flattened stages exceed the cap");
    assert!(
        error
            .to_string()
            .contains("flattens to 300 stages across taskPatterns, limit is 256"),
        "{error}"
    );

    // receiveSignals 通道总数 > 512（256 stage × 3 通道 = 768，其余在限内）。
    let stages = (0..256)
        .map(|stage| counted_stage_value(&format!("s{stage:03}"), &["a1", "a2", "a3"]))
        .collect::<Vec<_>>();
    let error = compile_zhixu_hook_plan(
        &counted_definition(vec![("t".to_string(), stages)]),
        None,
        true,
    )
    .expect_err("768 receiveSignals channels exceed the cap");
    assert!(
        error
            .to_string()
            .contains("declares 768 receiveSignals channels (compiled hooks) across taskPatterns, limit is 512"),
        "{error}"
    );
}

#[test]
fn rejects_interface_with_too_many_ports() {
    // D016（声明面计数闸）：单接口 inputs+outputs > 64。
    let mut definition = target_payment_definition();
    let mut inputs = serde_json::Map::new();
    for index in 0..65 {
        inputs.insert(
            format!("p{index:02}"),
            json!({ "hook": "payment_flow.init#DOCK_EXECUTE" }),
        );
    }
    definition["spec"]["dockInterface"]["wide"] = json!({
        "orderModes": ["existing"],
        "inputs": inputs,
    });
    let error = compile_zhixu_hook_plan(&definition, None, true)
        .expect_err("a 65-port interface exceeds the cap");
    let message = error.to_string();
    assert!(
        message.contains("D016")
            && message.contains("exposes 65 ports (65 inputs + 0 outputs), limit is 64"),
        "{message}"
    );
}

#[test]
fn dock_targets_reject_too_many_definitions() {
    // D008（dockTargets 计数闸）：条目 > 256——注入数据错误的
    // 规模面在解析期收口。
    let targets = (0..257)
        .map(|index| json!({ "uid": mid_uid(index) }))
        .collect::<Vec<_>>();
    let issues = crate::dock::parse_dock_targets(&Value::Array(targets)).unwrap_err();
    assert!(
        issues.iter().any(|issue| issue.code == "D008"
            && issue
                .message
                .contains("carries 257 definitions, limit is 256")),
        "{issues:?}"
    );
}

#[test]
fn error_string_is_truncated_at_the_boundary() {
    // 毒定义（每个 stage 一条 source 字符集错误 × 256 个 stage）产出远超
    // 16KB 的 issues——错误串在拼装边界截断并标注省略条数，FFI/NAPI 信封
    // 不随 plan 输入无界膨胀。
    let stages = (0..256)
        .map(|index| {
            json!({
                "name": format!("s{index:03}"),
                "source": "bad source",
                "receiveSignals": { "PUBLISH": "src::count.task.seed" },
                "sendSignals": [{ "name": "seed" }],
                "executor": { "supplierType": "organization", "supplierID": "counter" }
            })
        })
        .collect::<Vec<_>>();
    let definition = counted_definition(vec![("count".to_string(), stages)]);
    let error = compile_zhixu_hook_plan(&definition, None, true)
        .expect_err("256 invalid-source stages must fail");
    let message = error.to_string();
    assert!(
        message.contains("issues truncated: error string capped at 16384 bytes"),
        "{message}"
    );
    assert!(
        message.len() <= crate::MAX_ISSUES_STRING_BYTES + 200,
        "truncated error string must stay bounded: {}",
        message.len()
    );
}

// ------------------------------------------------------------------
// 发射适格面（admissions）：过滤档编译与编译期拒绝面（D026-D031）。
// ------------------------------------------------------------------

/// settle.cmp 的适格表达式（14d 窗口否决 control.cxl）：三段式寻址、
/// 衰减否决位在合取直接子项——两档都合法的保守形态。
const SETTLE_ADMISSION: &str = "payment::payment_flow.init.str & ~(payment_flow.control.cxl +14d)";

#[test]
fn admissions_compile_into_both_targets() {
    // 条目镜像 hook 条目形态：hook_plan 携带 normalizedExpression/ast/全量
    // 依赖（含 timer）；cloud 携带 cloudAst 与 (signalName, dependencyKind)
    // 两维依赖（timer 不进云侧消费面）。无条件条目不产 admission。
    let mut definition = target_payment_definition();
    definition["spec"]["taskPatterns"][0]["stages"][2]["sendSignals"][0]["validWhen"] =
        json!(SETTLE_ADMISSION);
    let plan = compile_zhixu_hook_plan(&definition, None, true)
        .expect("gated sendSignal compiles for hook_plan");
    let admissions = plan["admissions"].as_array().unwrap();
    assert_eq!(
        admissions.len(),
        1,
        "only the validWhen-bearing entry admits: {admissions:?}"
    );
    let admission = &admissions[0];
    assert_eq!(admission["stageIdentifier"], json!("payment_flow.settle"));
    assert_eq!(admission["signalName"], json!("payment_flow.settle.cmp"));
    assert_eq!(admission["rawExpression"], json!(SETTLE_ADMISSION));
    assert_eq!(
        admission["normalizedExpression"],
        json!("payment::payment_flow.init.str&~(payment_flow.control.cxl+14d)")
    );
    assert!(
        admission["ast"].is_object(),
        "hook_plan carries the parsed AST"
    );
    // 否决位内层是否定延时：负向依赖照出、timer 不出（衰减不为变假调度）。
    assert_eq!(
        admissions[0]["dependencies"],
        json!([
            { "kind": "negative", "source": "payment", "signalName": "payment_flow.control.cxl" },
            { "kind": "positive", "source": "payment", "signalName": "payment_flow.init.str" },
        ]),
        "hook_plan carries the full dependency form: {admission:?}"
    );

    let cloud = compile_cloud_artifact(&definition, None, true)
        .expect("cloud target compiles the same admission");
    let cloud_admissions = cloud["admissions"].as_array().unwrap();
    assert_eq!(cloud_admissions.len(), 1);
    let cloud_admission = &cloud_admissions[0];
    assert_eq!(
        cloud_admission["signalName"],
        json!("payment_flow.settle.cmp")
    );
    assert_eq!(
        cloud_admission["cloudAst"]["schemaVersion"],
        json!("uvp.cloudAst.v1")
    );
    assert_eq!(
        cloud_admission["dependencies"],
        json!([
            { "signalName": "payment_flow.control.cxl", "dependencyKind": "negative" },
            { "signalName": "payment_flow.init.str", "dependencyKind": "positive" },
        ]),
        "cloud dependencies drop the timer dimension: {cloud_admission:?}"
    );
}

#[test]
fn admission_self_reference_is_rejected_in_both_targets() {
    // pre-state 不含本发：表达式引用本信号自身是自证无效（D028）——按
    // 展开后的全名比对，与标头 source 无关（换一个 source 类标头引用
    // 本信号的全名同样是自引用）。
    for target in ["hook_plan", "cloud"] {
        for header in ["payment", "seller"] {
            let mut definition = target_payment_definition();
            definition["spec"]["taskPatterns"][0]["stages"][2]["sendSignals"][0]["validWhen"] =
                json!(format!("{header}::payment_flow.settle.cmp & payment_flow.init.str"));
            let result = if target == "hook_plan" {
                compile_zhixu_hook_plan(&definition, None, true)
            } else {
                compile_cloud_artifact(&definition, None, true)
            };
            let error = result.expect_err("self-referencing validWhen must fail");
            assert!(
                error.to_string().contains(
                    "D028 payment_flow.settle.sendSignals[cmp].validWhen: expression addresses the declaring signal itself (payment_flow.settle.cmp)"
                ),
                "{target}/{header}: {error}"
            );
        }
    }

    let mut definition = target_payment_definition();
    definition["spec"]["taskPatterns"][0]["stages"][2]["sendSignals"][0]["validWhen"] =
        json!("seller::payment_flow.settle.cmp & payment_flow.init.str");
    let error = compile_zhixu_hook_plan(&definition, None, true)
        .expect_err("a same-named fact under another source class still addresses the declaring signal's fact key");
    assert!(
        error.to_string().contains("D028 payment_flow.settle.sendSignals[cmp].validWhen"),
        "{error}"
    );
}

#[test]
fn admission_dangling_references_are_rejected() {
    // validWhen 引用存在性与 receiveSignals 同口径：悬空 stage / 目标
    // stage 存在但信号未声明 / 标头 source 未在本域声明，三个形态在两个
    // target 的编译期都响亮拒绝——放行会把死依赖推迟为运行期静默 init
    // （正锚永不 Ready）或静默失活的负门。
    for (label, expression, needle) in [
        (
            "unknown stage",
            "payment::task.nothere.nosignal",
            "references unknown stage task.nothere",
        ),
        (
            "undeclared signal",
            "payment::payment_flow.init.never_declared",
            "references unknown signal payment_flow.init.never_declared",
        ),
        (
            "undeclared source",
            "ghost::payment_flow.init.str",
            "is not a declared source in this zhixu",
        ),
    ] {
        for target in ["hook_plan", "cloud"] {
            let mut definition = target_payment_definition();
            definition["spec"]["taskPatterns"][0]["stages"][2]["sendSignals"][0]["validWhen"] =
                json!(expression);
            let result = if target == "hook_plan" {
                compile_zhixu_hook_plan(&definition, None, true)
            } else {
                compile_cloud_artifact(&definition, None, true)
            };
            let error = result.expect_err(&format!("{label} ({target}) must be rejected"));
            assert!(
                error.to_string().contains(".sendSignals[cmp].validWhen"),
                "{label} ({target}): {error}"
            );
            assert!(
                error.to_string().contains(needle),
                "{label} ({target}): {error}"
            );
        }
    }
}

#[test]
fn admission_on_birth_anchors_is_rejected() {
    // 出生写入不经适格面，声明即死代码（D029）。三个编译可见面：
    // mint SPAWN 出生目标 / dock new 模式出生锚输入端口 / 无锚通道阶段。
    //
    // mint SPAWN 出生目标：orchard.retail 订阅 payment_flow.init.str，
    // 该事实同时由 init 声明为 sendSignal——其 validWhen 即死代码。
    let mut minted = target_payment_definition();
    minted["spec"]["taskPatterns"][0]["stages"][0]["sendSignals"][0]["validWhen"] =
        json!(SETTLE_ADMISSION);
    minted["spec"]["taskPatterns"]
        .as_array_mut()
        .unwrap()
        .push(json!({
            "name": "orchard",
            "stages": [mint_stage_value(
                "orchard",
                "retail",
                "buyer",
                json!({ "SPAWN": "::ANCHOR(@payment::payment_flow.init.str)" }),
                &["ack"],
            )]
        }));
    let error = compile_zhixu_hook_plan(&minted, None, true)
        .expect_err("validWhen on a mint SPAWN birth target must fail");
    assert!(
        error.to_string().contains(
            "D029 payment_flow.init.sendSignals[str].validWhen: payment_flow.init.str is a birth-anchor signal"
        ),
        "{error}"
    );

    // dock 出生锚输入端口：payment_service[new].inputs.execute 的 atom 是
    // payment::payment_flow.init.execute（本单信号名），声明该名字的
    // sendSignals 条目不得携带 validWhen。
    let mut dock_anchored = target_payment_definition();
    dock_anchored["spec"]["taskPatterns"][0]["stages"][0]["sendSignals"]
        .as_array_mut()
        .unwrap()
        .push(json!({ "name": "execute", "validWhen": SETTLE_ADMISSION }));
    let error = compile_cloud_artifact(&dock_anchored, None, true)
        .expect_err("validWhen on a dock birth-anchor input must fail");
    assert!(
        error.to_string().contains(
            "D029 payment_flow.init.sendSignals[execute].validWhen: payment_flow.init.execute is a birth-anchor signal"
        ),
        "{error}"
    );

    // 无锚通道阶段：订阅阶段所在 source 类无 mint 声明 → 扇入投递无单
    // 可判，其信号全部不得声明适格面。
    let mut channel = target_payment_definition();
    channel["spec"]["taskPatterns"]
        .as_array_mut()
        .unwrap()
        .push(json!({
            "name": "audit",
            "stages": [{
                "name": "watch",
                "source": "auditor",
                "receiveSignals": { "SPAWN": "::ANCHOR(@payment::payment_flow.init.str)" },
                "sendSignals": [{ "name": "seen", "validWhen": SETTLE_ADMISSION }],
                "executor": { "supplierType": "organization", "supplierID": "audit-watcher" }
            }]
        }));
    let error = compile_zhixu_hook_plan(&channel, None, true)
        .expect_err("validWhen on an anchorless channel stage must fail");
    assert!(
        error.to_string().contains(
            "D029 audit.watch.sendSignals[seen].validWhen: audit.watch is an anchorless channel stage"
        ),
        "{error}"
    );

    // 正例对照：本域 source 类有 mint 声明的订阅阶段按单投递（route=
    // order），其信号过适格面照常编译。
    let mut anchored_channel = channel;
    anchored_channel["spec"]["taskPatterns"]
        .as_array_mut()
        .unwrap()
        .push(json!({
            "name": "orchard",
            "stages": [mint_stage_value(
                "orchard",
                "retail",
                "auditor",
                json!({ "SPAWN": "::ANCHOR(@payment::payment_flow.init.str)" }),
                &["ack"],
            )]
        }));
    compile_zhixu_hook_plan(&anchored_channel, None, true)
        .expect("an order-anchored subscription stage may declare admissions");
}

#[test]
fn admission_on_signal_map_relay_targets_is_rejected() {
    // 回传经引擎内部事务入口（ownsTx=false）进场，整段绕过适格筛：
    // signalMap 绑定的本地信号声明 validWhen 是"想设闸没设成"的死代码，
    // 与出生锚同族（D029），两个 target 一致拒绝。settlement.execute_payment
    // 的 signalMap 绑定 str/cmp/err，cxl 未绑定。
    let mut parent = parent_settlement_definition(TARGET_UID);
    parent["spec"]["taskPatterns"][1]["stages"][0]["sendSignals"][0]["validWhen"] =
        json!("buyer::checkout.confirm.cmp");
    for target in ["hook_plan", "cloud"] {
        let result = if target == "hook_plan" {
            compile_zhixu_hook_plan(&parent, None, true)
        } else {
            compile_cloud_artifact(&parent, None, true)
        };
        let error = result.expect_err("validWhen on a signalMap relay target must fail");
        assert!(
            error.to_string().contains(
                "D029 settlement.execute_payment.sendSignals[str].validWhen: settlement.execute_payment.str is a signalMap relay target"
            ),
            "{target}: {error}"
        );
    }

    // 正例对照：同一阶段未被 signalMap 绑定的外部信号带 validWhen 合法，
    // 适格面照常产出条目。
    let mut plain = parent_settlement_definition(TARGET_UID);
    plain["spec"]["taskPatterns"][1]["stages"][0]["sendSignals"][3]["validWhen"] =
        json!("buyer::checkout.confirm.cmp");
    let plan = compile_zhixu_hook_plan(&plain, None, true)
        .expect("validWhen on a signal outside signalMap stays legal");
    assert_eq!(plan["admissions"].as_array().map(Vec::len), Some(1));
}

#[test]
fn admission_subscription_atom_is_rejected() {
    // 适格是本单状态判定，订阅原子（ANCHOR）是逐事件投递通道——
    // 语义面互斥，编译期拒绝（D030）。过滤档解析放行订阅形态，拒绝
    // 只能落在此处（解析器无定义上下文）。
    let mut definition = target_payment_definition();
    definition["spec"]["taskPatterns"][0]["stages"][2]["sendSignals"][0]["validWhen"] =
        json!("::ANCHOR(@payment::payment_flow.init.str)");
    for target in ["hook_plan", "cloud"] {
        let result = if target == "hook_plan" {
            compile_zhixu_hook_plan(&definition, None, true)
        } else {
            compile_cloud_artifact(&definition, None, true)
        };
        let error = result.expect_err("subscription atoms must not ride the admission face");
        assert!(
            error.to_string().contains(
                "D030 payment_flow.settle.sendSignals[cmp].validWhen: admission is a per-order state judgment and must not contain subscription atoms"
            ),
            "{target}: {error}"
        );
    }
}

#[test]
fn admission_name_blank_expression_duplicate_and_unknown_key_faces() {
    // D026 空 name；D027 空白 validWhen（声明即必填，空白是笔误面）；
    // D031 重复 capability（裸名与 canonical 自指同键）；未知键由
    // typed model 的 deny_unknown_fields 在定义解码期响亮拒绝。
    let mut empty_name = target_payment_definition();
    empty_name["spec"]["taskPatterns"][0]["stages"][0]["sendSignals"]
        .as_array_mut()
        .unwrap()
        .push(json!({ "name": "" }));
    let error = compile_zhixu_hook_plan(&empty_name, None, true)
        .expect_err("an entry without a name must fail");
    assert!(
        error
            .to_string()
            .contains("D026 payment_flow.init.sendSignals: name is required"),
        "{error}"
    );

    let mut blank = target_payment_definition();
    blank["spec"]["taskPatterns"][0]["stages"][0]["sendSignals"][0]["validWhen"] = json!("   ");
    let error =
        compile_cloud_artifact(&blank, None, true).expect_err("a blank validWhen must fail loudly");
    assert!(
        error.to_string().contains(
            "D027 payment_flow.init.sendSignals[str].validWhen: must be a non-blank expression"
        ),
        "{error}"
    );

    let mut duplicate = target_payment_definition();
    duplicate["spec"]["taskPatterns"][0]["stages"][0]["sendSignals"]
        .as_array_mut()
        .unwrap()
        .push(json!({ "name": "payment_flow.init.str" }));
    let error = compile_zhixu_hook_plan(&duplicate, None, true)
        .expect_err("bare + canonical self-reference must collide on the expanded capability");
    assert!(
        error.to_string().contains("D031") && error.to_string().contains("duplicate capability"),
        "{error}"
    );

    let mut unknown_key = target_payment_definition();
    unknown_key["spec"]["taskPatterns"][0]["stages"][0]["sendSignals"]
        .as_array_mut()
        .unwrap()
        .push(json!({ "name": "str", "validwhen": SETTLE_ADMISSION }));
    let error = compile_zhixu_hook_plan(&unknown_key, None, true)
        .expect_err("a misspelled key must fail at decode, not fold to the zero value");
    assert!(
        error.to_string().contains("unknown field") && error.to_string().contains("validwhen"),
        "{error}"
    );
}

#[test]
fn admission_invalid_expression_reports_the_declaring_signal() {
    // 表达式方言 = 钩子方言本身：解析失败按声明信号上下文上报
    // （目标信号名作为上下文传入），不是裸 serde 报错。
    let mut definition = target_payment_definition();
    definition["spec"]["taskPatterns"][0]["stages"][2]["sendSignals"][0]["validWhen"] =
        json!("payment::settle.cmp");
    let error = compile_zhixu_hook_plan(&definition, None, true)
        .expect_err("a two-part signal name inside validWhen must fail");
    assert!(
        error
            .to_string()
            .contains("payment_flow.settle.sendSignals[cmp].validWhen is invalid")
            && error.to_string().contains("task.stage.signal"),
        "{error}"
    );
}
