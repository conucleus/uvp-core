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
                        // 物化门：零 hook 阶段在链上永不可物化、信号
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
    // 能力表规模闸：hook_plan 与 cloud 共用 build_signal_capabilities，
    // 与 TS 编译+反序列化边界同值同文案；256 条放行、257 条拒绝。
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
    let at_limit = compile_zhixu_hook_plan(&definition_with(MAX_SIGNAL_CAPABILITIES), None, true)
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
    let plan =
        compile_zhixu_hook_plan(&parent, Some(&manifest), false).expect("linked parent compiles");
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
fn rejects_parent_without_manifest() {
    let error = compile_zhixu_hook_plan(&parent_settlement_definition(TARGET_NAME), None, false)
        .expect_err("unresolved dock target must fail");
    assert!(
        error.to_string().contains("UNRESOLVED_DOCK_TARGET"),
        "unexpected error: {error}"
    );
}

#[test]
fn dock_link_compile_target_is_retired() {
    // dock link 编译 target（uvp.dock-link v1 产物面）
    // 无消费方，直接删除、无兼容形态——出现即按未知 target 响亮拒绝
    // （link 校验由 hook_plan/cloud/parse 在 manifest 在场时同一链路
    // 承担）。
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
    let value = compile_zhixu_hook_plan(&parent_settlement_definition(TARGET_NAME), None, true)
        .expect("parse target allows unresolved routes");
    assert_eq!(value["dockRoutes"].as_array().unwrap().len(), 0);
    // 静态目标 route 不因无 manifest 而从声明面消失——
    // parse 产物如实携带全部委托形态，静态条目携带作者声明的
    // target.zhixu（name 引用，非派生身份）。
    let unresolved = value["unresolvedDockRoutes"].as_array().unwrap();
    assert_eq!(unresolved.len(), 1);
    let route = &unresolved[0];
    assert_eq!(
        route["schemaVersion"],
        dock::DOCK_ROUTE_UNRESOLVED_SCHEMA_VERSION
    );
    assert_eq!(route["stageIdentifier"], "settlement.execute_payment");
    assert_eq!(route["target"], json!({ "zhixu": TARGET_NAME }));
    assert_eq!(route["interfaceName"], "payment_service");
    assert_eq!(route["orderMode"], "new");
    assert_eq!(
        route["inputBindings"],
        json!([{ "hookId": "settlement.execute_payment#EXECUTE", "port": "execute" }])
    );
}

#[test]
fn parse_product_declaration_face_is_complete_with_manifest() {
    // parse-only 产物带 manifest：静态 route 照常解析进 dockRoutes，
    // 同时保留声明面条目（与动态目标对称——manifest 在场时 null-target
    // route 也不退出声明面，见 manifest_present_null_target_route_stays_
    // unresolved）。
    let target = target_payment_definition();
    let manifest = manifest_for(&target);
    let artifact = compile_cloud_artifact(
        &parent_settlement_definition(TARGET_NAME),
        Some(&manifest),
        true,
    )
    .expect("parse-only compilation with a manifest links and declares");
    assert_eq!(artifact["dockRoutes"].as_array().unwrap().len(), 1);
    assert_eq!(
        artifact["dockRoutes"][0]["target"]["name"],
        json!(TARGET_NAME)
    );
    let unresolved = artifact["unresolvedDockRoutes"].as_array().unwrap();
    assert_eq!(unresolved.len(), 1);
    assert_eq!(
        unresolved[0]["stageIdentifier"],
        "settlement.execute_payment"
    );
    assert_eq!(unresolved[0]["target"], json!({ "zhixu": TARGET_NAME }));

    // 可运行产物（allow_unresolved=false）不做声明面冗余：静态 route
    // 全量解析后 unresolvedDockRoutes 不落字段（既有口径不变）。
    let runnable = compile_cloud_artifact(
        &parent_settlement_definition(TARGET_NAME),
        Some(&manifest),
        false,
    )
    .expect("runnable compilation keeps the lean declaration face");
    assert!(runnable.get("unresolvedDockRoutes").is_none());
}

#[test]
fn rejects_unsupported_executor_config_shapes() {
    // triggerEntrance：不受支持的调用方字段，D002 未知字段硬错误。
    let mut parent = parent_settlement_definition(TARGET_NAME);
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
    let error =
        compile_zhixu_hook_plan(&parent, None, false).expect_err("dotted signalMap key must fail");
    assert!(
        error.to_string().contains("D006") && error.to_string().contains("must not contain '.'"),
        "{}",
        error.to_string()
    );

    // 组合维度：stage 标识符 + key 超 signal_name 列宽（100）。
    let mut parent = parent_settlement_definition(TARGET_NAME);
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
    let manifest = manifest_for(&target);

    // ① signalMap 裸名 key 引用 canonical 自指声明：key "str" 展开后与
    // settlement.execute_payment.str 同一全名，链接照常。
    let mut parent = parent_settlement_definition(TARGET_NAME);
    parent["spec"]["taskPatterns"][1]["stages"][0]["sendSignals"] =
        json!(["cmp", "err", "cxl", "settlement.execute_payment.str"]);
    let plan = compile_zhixu_hook_plan(&parent, Some(&manifest), false).expect(
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
    let mut parent = parent_settlement_definition(TARGET_NAME);
    parent["spec"]["taskPatterns"][1]["stages"][0]["sendSignals"] = json!(["cmp", "err", "cxl"]);
    let error = compile_zhixu_hook_plan(&parent, Some(&manifest), false)
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
            // 自发种子入口钩子，避免零 hook 阶段被物化门拒绝。
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
        // 37 字节即拒：壳层上限与 hook 标头/订阅目标的 source 类上限
        // 收敛到 36（37-100 字节的 source 是"声明即死"命名空间）。
        ("oversized", "s".repeat(37)),
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
    // 36 字节边界恰好放行（超出在形状层拒绝，边界值走到后续 link 才失败）。
    let mut parent = parent_settlement_definition(TARGET_NAME);
    parent["spec"]["taskPatterns"][1]["stages"][0]["source"] = json!("s".repeat(36));
    let error = compile_zhixu_hook_plan(&parent, None, false)
        .expect_err("boundary source must pass shape checks and fail later on linking");
    assert!(
        !error.to_string().contains("exceeds 36 bytes"),
        "36-byte source is legal: {error}"
    );
    // 37 字节：形状层响亮拒绝并指向 36 上限。
    let mut parent = parent_settlement_definition(TARGET_NAME);
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
    let mut parent = parent_settlement_definition(TARGET_NAME);
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
    let mut parent = parent_settlement_definition(TARGET_NAME);
    parent["spec"]["taskPatterns"][1]["stages"][0]["executor"]["supplierID"] = json!("  ");
    let error = compile_zhixu_hook_plan(&parent, None, false)
        .expect_err("empty supplierID on a zhixu executor must fail");
    assert!(
        error.to_string().contains("D001") && error.to_string().contains("supplierID is forbidden"),
        "{}",
        error.to_string()
    );

    // D019：无任何映射。
    let mut parent = parent_settlement_definition(TARGET_NAME);
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
    let error =
        compile_zhixu_hook_plan(&parent, None, false).expect_err("non-slug target name must fail");
    assert!(
        error.to_string().contains("D003") && error.to_string().contains("metadata.name"),
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
    let mut parent = parent_settlement_definition(TARGET_NAME);
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
        let mut parent = parent_settlement_definition(TARGET_NAME);
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
        let mut parent = parent_settlement_definition(TARGET_NAME);
        parent["spec"]["taskPatterns"][0]["stages"][0]["executor"]["supplierType"] =
            json!(supplier_type);
        compile_zhixu_hook_plan(&parent, None, true)
            .unwrap_or_else(|err| panic!("{supplier_type} must pass the closed set: {err}"));
    }
}

#[test]
fn rejects_non_zhixu_executor_with_delegation_config() {
    // organization executor 携带完整 zhixuExecutorConfig：编译期响亮拒绝
    // （D001"拼错字段同罪"口径）——静默放行会把委托配置原文烧进
    // executorRoutes（链上承诺面），"既静态执行者又委托对接"是矛盾声明。
    let mut parent = parent_settlement_definition(TARGET_NAME);
    parent["spec"]["taskPatterns"][0]["stages"][0]["executor"]["zhixuExecutorConfig"] = json!({
        "target": { "zhixu": TARGET_NAME },
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
    // D015 同源防线前移：manifest 声明面可成的环在
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
    three_node["definitions"]
        .as_array_mut()
        .unwrap()
        .push(json!({
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
    let error =
        compile_zhixu_hook_plan(&parent_with_route_to(TARGET_NAME), Some(&self_edge), false)
            .expect_err("manifest self-edge cycle must fail");
    assert!(
        error.to_string().contains("D015")
            && error
                .to_string()
                .contains("payment_execution -> payment_execution"),
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
fn send_signal_declarations_use_a_single_exact_surface() {
    // 声明面单一精确口径：declared 与 capability 同一原文（不 trim），
    // `<target>::<signal>` 与裸名都按信号名同款标识符文法闸——空白、
    // `::` 再现、非标识符字符、数字开头在此响亮拒绝（capability 侧
    // trim 归一会产出死能力并与其它声明撞 duplicate）。
    for (label, signal) in [
        ("leading whitespace bare", " str"),
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
            .push(json!(signal));
        let error = compile_zhixu_hook_plan(&definition, None, true)
            .expect_err(&format!("{label} ({signal:?}) must be rejected"));
        assert!(
            error.to_string().contains(".sendSignals"),
            "{label} ({signal:?}): {error}"
        );
    }

    // 合法形态钉住：`<target>::<signal>` 携带合法标识符文法（大写与
    // task/stage 名同文法合法——身份大小写敏感、无折叠）照常编译，
    // capability 携带与声明逐字节相同的精确值。
    let mut definition = target_payment_definition();
    definition["spec"]["taskPatterns"][0]["stages"][0]["sendSignals"]
        .as_array_mut()
        .unwrap()
        .push(json!("Seller::NOTED"));
    let plan = compile_zhixu_hook_plan(&definition, None, true)
        .expect("identifier-grammar target signal compiles");
    let capability = plan["signalCapabilities"]
        .as_array()
        .unwrap()
        .iter()
        .find(|capability| capability["declaredSignal"] == json!("Seller::NOTED"))
        .expect("capability carries the exact declared value");
    assert_eq!(capability["targetSource"], json!("Seller"));
    assert_eq!(capability["targetSignalName"], json!("NOTED"));
    assert_eq!(capability["targetOrderRelation"], json!("triggerOrigin"));

    // "str" 与 " str" 不再撞 duplicate 误判：" str" 在形态闸被拒绝。
    let mut definition = target_payment_definition();
    definition["spec"]["taskPatterns"][0]["stages"][0]["sendSignals"]
        .as_array_mut()
        .unwrap()
        .push(json!(" str"));
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
        json!(["payment_flow.init.str"]);
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
                "sendSignals": ["payment_flow.init.str"],
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
        json!(["str", "payment_flow.init.str"]);
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
        json!(["payment_flow.init.str"]);
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
    // 零 hook 阶段不过物化门；seed 是执行者自发入口信号。
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
