use super::*;
use crate::validate::validate_supplier_dimensions;
use serde_json::json;

const UNKNOWN_TARGET: &str = "zx-eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";

const SELF_TARGET_UID: &str = "zx-ffffffffffffffffffffffffffffffff";

fn mid_uid(index: usize) -> String {
    format!("zx-{index:032x}")
}

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
                            "execute": { "hook": "init#DOCK_EXECUTE" },
                            "cancel": { "hook": "control#DOCK_CANCEL" }
                        },
                        "outputs": {
                            "started": { "signal": "payment::init.str" },
                            "completed": { "signal": "payment::settle.cmp" },
                            "failed": { "signal": "payment::settle.err" }
                        }
                    },
                    "payment_evidence": {
                        "orderModes": ["existing"],
                        "outputs": {
                            "cancelled": { "signal": "payment::control.cxl" }
                        }
                    }
                },
                "stages": [{
                            "name": "init",
                            "source": "payment",
                            "receiveSignals": {
                                "DOCK_EXECUTE": "payment::init.execute"
                            },
                            "sendSignals": [{ "name": "str" }],
                            "executor": { "supplierType": "organization", "supplierID": "payment-gateway" }
                        },
                        {
                            "name": "control",
                            "source": "payment",
                            "receiveSignals": {
                                "DOCK_CANCEL": "payment::control.cancel"
                            },
                            "sendSignals": [{ "name": "cxl" }],
                            "executor": { "supplierType": "organization", "supplierID": "payment-gateway" }
                        },
                        {
                            "name": "settle",
                            "source": "payment",
                            "receiveSignals": {
                                "SETTLE": "payment::init.str"
                            },
                            "sendSignals": [
    { "name": "cmp" },
    { "name": "err" }
    ],
                            "executor": { "supplierType": "organization", "supplierID": "payment-gateway" }
                        }]
            }
        })
}

const TARGET_UID: &str = "zx-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

fn parent_settlement_definition(target_uid: &str) -> Value {
    json!({
                "apiVersion": "uvp/v0",
                "kind": "Zhixu",
                "metadata": { "name": "settlement" },
                "spec": {
                    "platform": { "type": "cloud" },
                    "nucleation": { "id": "settlement-core" },
                    "stages": [{
                                "name": "confirm",
                                "source": "buyer",
                                "receiveSignals": { "PLACE": "buyer::confirm.seed" },
                                "sendSignals": [
        { "name": "cmp" },
        { "name": "seed" }
        ],
                                "executor": { "supplierType": "organization", "supplierID": "buyer-app" }
                            },
                            {
                                "name": "cancel",
                                "source": "buyer",
                                "receiveSignals": { "ABORT": "buyer::cancel.seed" },
                                "sendSignals": [
        { "name": "cmp" },
        { "name": "seed" }
        ],
                                "executor": { "supplierType": "organization", "supplierID": "buyer-app" }
                            },
    {
                                "name": "execute_payment",
                                "source": "buyer",
                                "receiveSignals": {
                                    "EXECUTE": "buyer::confirm.cmp",
                                    "CANCEL": "buyer::cancel.cmp"
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
                            }]
                }
            })
}

fn parent_recycling_definition(target_uid: &str) -> Value {
    json!({
            "apiVersion": "uvp/v0",
            "kind": "Zhixu",
            "metadata": { "name": "recycling" },
            "spec": {
                "platform": { "type": "cloud" },
                "nucleation": { "id": "recycling-core" },
                "stages": [{
                            "name": "source_evidence",
                            "source": "recycler",
                            "receiveSignals": { "READ": "recycler::source_evidence.seed" },
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
                        }]
            }
        })
}

fn dock_target_entry(uid: &str, target: &Value) -> Value {
    json!({ "uid": uid, "definition": target })
}

fn dock_targets_for(target: &Value) -> Value {
    json!([dock_target_entry(TARGET_UID, target)])
}

fn with_static_dock_edge(mut definition: Value, edge_target: Option<&str>) -> Value {
    definition["spec"]["stages"][0]["executor"] = json!({
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
            "stages": [{ "name": "work", "source": "buyer", "executor": executor }],
            "dockInterface": {
                "svc": {
                    "orderModes": ["existing"],
                    "outputs": { "done": { "signal": "buyer::work.cmp" } }
                }
            }
        }
    })
}

fn chain_link_entry(uid: String, edge_target: Option<&str>) -> Value {
    json!({ "uid": uid, "definition": chain_link_definition(edge_target) })
}

#[test]
fn send_signals_total_is_uncapped() {
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
                "stages": [{
                            "name": "work",
                            "source": "buyer",
                            "receiveSignals": { "START": "buyer::work.sig000" },
                            "sendSignals": signals,
                            "executor": { "supplierType": "organization", "supplierID": "buyer-app" }
                        }]
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
fn link_reports_every_routes_issues_in_one_pass() {
    let target = target_payment_definition();
    let mut parent = parent_settlement_definition(TARGET_UID);
    let second_dock = json!({
            "name": "second_dock",
            "source": "buyer",
            "receiveSignals": { "START": "buyer::confirm.cmp" },
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
    parent["spec"]["stages"]
        .as_array_mut()
        .unwrap()
        .push(second_dock);
    let dock_targets = dock_targets_for(&target);
    let error = compile_zhixu_hook_plan(&parent, Some(&dock_targets), false)
        .expect_err("unresolvable second route must fail");
    let message = error.to_string();
    assert!(
        message.contains("D008")
            && message.contains("second_dock")
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

    let hooks = plan["compiledHooks"].as_array().unwrap();
    let hook_ids = hooks
        .iter()
        .map(|hook| hook["hookId"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert!(!hook_ids.iter().any(|id| id.starts_with("signalMap.")));
    let execute = hooks
        .iter()
        .find(|hook| hook["hookId"] == "execute_payment#EXECUTE")
        .unwrap();
    assert_eq!(execute["orderTriggerKind"], "none");
    assert_eq!(execute["emitReady"], true);

    let routes = plan["dockRoutes"].as_array().unwrap();
    assert_eq!(routes.len(), 1);
    let route = &routes[0];
    assert_eq!(route["schemaVersion"], "uvp.dockRoute.v3");
    assert_eq!(route["local"]["stageIdentifier"], "execute_payment");
    assert_eq!(route["orderMode"], "new");
    assert_eq!(route["target"]["uid"], json!(TARGET_UID));
    assert_eq!(route["target"]["interfaceName"], json!("payment_service"));
    assert!(route.get("orderIdPolicy").is_none());
    assert_eq!(route["inputBindings"].as_array().unwrap().len(), 1);
    assert_eq!(
        route["inputBindings"][0]["hookId"],
        "execute_payment#EXECUTE"
    );
    assert_eq!(route["inputBindings"][0]["port"], "execute");
    assert_eq!(route["outputBindings"].as_array().unwrap().len(), 3);
    assert!(plan["executorRoutes"]
        .as_object()
        .unwrap()
        .get("execute_payment")
        .is_none());

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
fn target_compiles_named_interfaces_and_dock_trigger_flags() {
    let plan = compile_zhixu_hook_plan(&target_payment_definition(), None, true)
        .expect("target compiles standalone");
    let interface = &plan["dockInterface"];
    let interfaces = interface.as_array().unwrap();
    assert_eq!(interfaces.len(), 2);
    assert_eq!(interfaces[0]["name"], json!("payment_evidence"));
    assert_eq!(interfaces[1]["name"], json!("payment_service"));
    assert_eq!(interfaces[0]["orderModes"], json!(["existing"]));
    assert_eq!(interfaces[1]["orderModes"], json!(["new"]));
    assert_eq!(
        interfaces[1]["inputs"],
        json!({
            "cancel": { "source": "payment", "hook": "control#DOCK_CANCEL" },
            "execute": { "source": "payment", "hook": "init#DOCK_EXECUTE" }
        })
    );
    assert_eq!(
        interfaces[1]["outputs"],
        json!({
            "completed": { "signal": "payment::settle.cmp" },
            "failed": { "signal": "payment::settle.err" },
            "started": { "signal": "payment::init.str" }
        })
    );
    assert!(
        interfaces[0].get("interfaceRoot").is_none()
            && interfaces[0].get("inputsRoot").is_none()
            && interface.get("interfaceRoot").is_none(),
        "neutral interface declaration must not carry roots"
    );

    let hooks = plan["compiledHooks"].as_array().unwrap();
    let entrance = hooks
        .iter()
        .find(|hook| hook["hookId"] == "init#DOCK_EXECUTE")
        .unwrap();
    assert_eq!(entrance["orderTriggerKind"], "dock");
    assert_eq!(entrance["emitReady"], true);
    let cancel = hooks
        .iter()
        .find(|hook| hook["hookId"] == "control#DOCK_CANCEL")
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
    let unresolved = value["unresolvedDockRoutes"].as_array().unwrap();
    assert_eq!(unresolved.len(), 1);
    let route = &unresolved[0];
    assert_eq!(
        route["schemaVersion"],
        dock::DOCK_ROUTE_UNRESOLVED_SCHEMA_VERSION
    );
    assert_eq!(route["stageIdentifier"], "execute_payment");
    assert_eq!(route["target"], json!({ "zhixu": TARGET_UID }));
    assert_eq!(route["interfaceName"], "payment_service");
    assert_eq!(route["orderMode"], "new");
    assert_eq!(
        route["inputBindings"],
        json!([{ "hookId": "execute_payment#EXECUTE", "port": "execute" }])
    );
}

#[test]
fn parse_product_declaration_face_is_complete_with_dock_targets() {
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
    assert_eq!(unresolved[0]["stageIdentifier"], "execute_payment");
    assert_eq!(unresolved[0]["target"], json!({ "zhixu": TARGET_UID }));

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
    let mut parent = parent_settlement_definition(TARGET_UID);
    parent["spec"]["stages"][2]["executor"]["zhixuExecutorConfig"]
        .as_object_mut()
        .unwrap()
        .insert("triggerEntrance".to_string(), json!("init"));
    let error =
        compile_zhixu_hook_plan(&parent, None, false).expect_err("triggerEntrance must hard-fail");
    let message = error.to_string();
    assert!(
        message.contains("D002") && message.contains("triggerEntrance"),
        "{message}"
    );
    assert!(message.contains("unknown field"), "{message}");

    let mut parent = parent_settlement_definition(TARGET_UID);
    parent["spec"]["stages"][2]["executor"]["zhixuExecutorConfig"]
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

    let mut parent = parent_settlement_definition(TARGET_UID);
    parent["spec"]["stages"][2]["executor"]["zhixuExecutorConfig"]
        .as_object_mut()
        .unwrap()["signalMap"]["str"] = json!("payment::init.str");
    let error = compile_zhixu_hook_plan(&parent, None, false)
        .expect_err("hook-DSL signalMap value must hard-fail");
    assert!(error.to_string().contains("D006"), "{}", error.to_string());

    let mut parent = parent_settlement_definition(TARGET_UID);
    parent["spec"]["stages"][2]["executor"]["supplierID"] = json!("payment-zhixu");
    let error = compile_zhixu_hook_plan(&parent, None, false)
        .expect_err("supplierID on zhixu executor must fail");
    assert!(error.to_string().contains("D001"), "{}", error.to_string());
}

#[test]
fn signal_map_keys_match_expanded_full_signal_names() {
    let target = target_payment_definition();
    let dock_targets = dock_targets_for(&target);

    let mut parent = parent_settlement_definition(TARGET_UID);
    parent["spec"]["stages"][2]["sendSignals"] = json!([{ "name": "cmp" }, { "name": "err" }, { "name": "cxl" }, { "name": "execute_payment.str" }]);
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

    let mut parent = parent_settlement_definition(TARGET_UID);
    parent["spec"]["stages"][2]["sendSignals"] =
        json!([{ "name": "cmp" }, { "name": "err" }, { "name": "cxl" }]);
    let error = compile_zhixu_hook_plan(&parent, Some(&dock_targets), false)
        .expect_err("dangling signalMap key must fail");
    let message = error.to_string();
    assert!(
        message.contains("D006")
            && message.contains("key is not a sendSignals signal of stage execute_payment"),
        "{message}"
    );
}

#[test]
fn dock_targets_uid_is_the_resolution_key() {
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

fn delegation_subscription_definition(with_anchor: bool) -> Value {
    let mut stages = vec![
        json!({
                    "name": "fanin",
                    "source": "anchoredcls",
                    "receiveSignals": { "SUB": "::ANCHOR(@other::emit.cmp)" },
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
        json!({
                    "name": "emit",
                    "source": "other",
                    "receiveSignals": { "PUBLISH": "other::emit.seed" },
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
            "receiveSignals": { "SPAWN": "::ANCHOR(@other::emit.cmp)" },
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
            "stages": stages
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
    compile_zhixu_hook_plan(&delegation_subscription_definition(true), None, true)
        .expect("anchored subscription with zhixu executor compiles");
    compile_cloud_artifact(&delegation_subscription_definition(true), None, true)
        .expect("cloud target accepts the anchored combination");
}

#[test]
fn rejects_unknown_spec_and_executor_fields() {
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

    let mut parent = parent_settlement_definition(TARGET_UID);
    parent["spec"]["stages"][2]["executor"]["handlerType"] = json!("http");
    let error = compile_zhixu_hook_plan(&parent, None, false)
        .expect_err("unknown executor field must fail");
    assert!(
        error.to_string().contains("unknown field `handlerType`"),
        "{error}"
    );

    let mut parent = parent_settlement_definition(TARGET_UID);
    parent["metadata"]["description"] = json!("demo");
    let error = compile_zhixu_hook_plan(&parent, None, false)
        .expect_err("metadata-level unknown field must fail");
    assert!(
        error.to_string().contains("unknown field `description`"),
        "{error}"
    );

    let mut parent = parent_settlement_definition(TARGET_UID);
    parent["metadata"]["uid"] = json!("zx-hand-written");
    let error = compile_zhixu_hook_plan(&parent, None, false)
        .expect_err("metadata.uid must be rejected as an unknown field");
    assert!(error.to_string().contains("unknown field `uid`"), "{error}");
}

#[test]
fn rejects_interface_shape_violations() {
    let mut target = target_payment_definition();
    target["spec"]["stages"][0]["receiveSignals"]["DOCK_EXECUTE"] =
        json!("payment::init.execute & payment::control.cxl");
    let error = compile_zhixu_hook_plan(&target, None, true)
        .expect_err("composite input port hook must fail");
    assert!(error.to_string().contains("D013"), "{}", error.to_string());

    for expression in [
        "payment::init.execute & payment::init.execute",
        "payment::init.execute | payment::init.execute",
    ] {
        let mut target = target_payment_definition();
        target["spec"]["stages"][0]["receiveSignals"]["DOCK_EXECUTE"] = json!(expression);
        let error = compile_zhixu_hook_plan(&target, None, true)
            .expect_err("same-atom composition must fail");
        assert!(error.to_string().contains("D013"), "{expression}: {error}");
    }

    let mut target = target_payment_definition();
    target["spec"]["stages"][0]["receiveSignals"]["DOCK_EXECUTE"] = json!("payment::control.cxl");
    let error = compile_zhixu_hook_plan(&target, None, true)
        .expect_err("input atom addressing another stage must fail");
    assert!(
        error.to_string().contains("D013")
            && error.to_string().contains("must address the owning stage"),
        "{}",
        error.to_string()
    );

    let mut target = target_payment_definition();
    target["spec"]["dockInterface"]["payment_service"]["outputs"]["started"]["signal"] =
        json!("payment::init.nope");
    let error =
        compile_zhixu_hook_plan(&target, None, true).expect_err("unknown output signal must fail");
    assert!(error.to_string().contains("D014"), "{}", error.to_string());

    let mut target = target_payment_definition();
    let inputs = target["spec"]["dockInterface"]["payment_service"]["inputs"]
        .as_object_mut()
        .unwrap();
    let execute = inputs.remove("execute").unwrap();
    inputs.insert("BadPort".to_string(), execute);
    let error =
        compile_zhixu_hook_plan(&target, None, true).expect_err("invalid port name must fail");
    assert!(error.to_string().contains("D021"), "{}", error.to_string());

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

fn null_target_parent() -> Value {
    let mut parent = parent_settlement_definition(TARGET_UID);
    parent["spec"]["stages"][2]["executor"]["zhixuExecutorConfig"]
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
    assert_eq!(route["stageIdentifier"], "execute_payment");
    assert_eq!(route["localSource"], "buyer");
    assert_eq!(route["interfaceName"], "payment_service");
    assert_eq!(route["orderMode"], "new");
    assert_eq!(
        route["inputBindings"],
        json!([{ "hookId": "execute_payment#EXECUTE", "port": "execute" }])
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
fn dock_targets_present_null_target_route_stays_unresolved() {
    let target = target_payment_definition();
    let dock_targets = dock_targets_for(&target);
    let mut parent = null_target_parent();
    parent["spec"]["stages"]
        .as_array_mut()
        .unwrap()
        .push(json!({
            "name": "static_dock",
            "source": "buyer",
            "receiveSignals": { "START": "buyer::confirm.cmp" },
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
        "static_dock"
    );
    let unresolved = artifact["unresolvedDockRoutes"].as_array().unwrap();
    assert_eq!(unresolved.len(), 1);
    assert_eq!(unresolved[0]["stageIdentifier"], "execute_payment");
}

#[test]
fn dynamic_target_null_still_enforces_local_config_validation() {
    let mut parent = null_target_parent();
    parent["spec"]["stages"][2]["executor"]["zhixuExecutorConfig"]
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

    let mut parent = null_target_parent();
    let config = parent["spec"]["stages"][2]["executor"]["zhixuExecutorConfig"]
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
    let channels = (0..9).map(|index| format!("CH{index}")).collect::<Vec<_>>();
    let receive_signals: Map<String, Value> = channels
        .iter()
        .map(|channel| {
            (
                channel.clone(),
                Value::String("buyer::work.seed".to_string()),
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
            "stages": [{
                        "name": "work",
                        "source": "buyer",
                        "receiveSignals": { "START": "buyer::work.seed" },
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
                    }}]
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

    let mut parent = parent;
    let config = parent["spec"]["stages"][1]["executor"]["zhixuExecutorConfig"]
        .as_object_mut()
        .unwrap();
    config["inputMap"].as_object_mut().unwrap().remove("CH8");
    parent["spec"]["stages"][1]["receiveSignals"]
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
fn rejects_selectable_resource_outside_the_closed_file_type_set() {
    for (label, value) in [
        (
            "misspelled",
            json!({ "dataset": { "fileType": "tx_cloud" } }),
        ),
        ("padded", json!({ "dataset": { "fileType": " http" } })),
        ("not-a-map", json!(["dataset"])),
    ] {
        let mut parent = parent_settlement_definition(TARGET_UID);
        parent["spec"]["stages"][0]["executor"]["selectableResource"] = value;
        let error = compile_zhixu_hook_plan(&parent, None, true)
            .err()
            .unwrap_or_else(|| panic!("{label} selectableResource must be rejected"));
        assert!(
            error.to_string().contains("selectableResource")
                && (label == "not-a-map" || error.to_string().contains("fileType must be one of")),
            "{label}: {error}"
        );
    }

    let mut parent = parent_settlement_definition(TARGET_UID);
    parent["spec"]["stages"][0]["executor"]["selectableResource"] = json!({
        "dataset": { "fileType": "plain_text", "plainText": { "content": "x" } }
    });
    compile_zhixu_hook_plan(&parent, None, true)
        .unwrap_or_else(|err| panic!("closed-set selectableResource must compile: {err}"));
}

#[test]
fn rejects_non_zhixu_executor_with_delegation_config() {
    let mut parent = parent_settlement_definition(TARGET_UID);
    parent["spec"]["stages"][0]["executor"]["zhixuExecutorConfig"] = json!({
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

    let mut unregistered = dock_targets_for(&target);
    unregistered[0]["uid"] = json!(UNKNOWN_TARGET);
    let error = compile_zhixu_hook_plan(
        &parent_settlement_definition(TARGET_UID),
        Some(&unregistered),
        false,
    )
    .expect_err("missing target must fail");
    assert!(error.to_string().contains("D008"), "{}", error.to_string());

    let mut parent = parent_settlement_definition(TARGET_UID);
    parent["spec"]["stages"][2]["executor"]["zhixuExecutorConfig"]
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

    let mut parent = parent_settlement_definition(TARGET_UID);
    parent["spec"]["stages"][2]["executor"]["zhixuExecutorConfig"]
        .as_object_mut()
        .unwrap()["interface"] = json!("payment_archive");
    let error = compile_zhixu_hook_plan(&parent, Some(&dock_targets), false)
        .expect_err("unknown interface must fail");
    assert!(
        error.to_string().contains("D009") && error.to_string().contains("payment_archive"),
        "{}",
        error.to_string()
    );

    let mut parent = parent_settlement_definition(TARGET_UID);
    parent["spec"]["stages"][2]["executor"]["zhixuExecutorConfig"]
        .as_object_mut()
        .unwrap()["order"]["mode"] = json!("existing");
    let error = compile_zhixu_hook_plan(&parent, Some(&dock_targets), false)
        .expect_err("mode not allowed by interface must fail");
    assert!(
        error.to_string().contains("D020") && error.to_string().contains("allows orderModes"),
        "{}",
        error.to_string()
    );

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
    let mut deep = dock_targets_for(&with_static_dock_edge(
        target_payment_definition(),
        Some(&mid_uid(1)),
    ));
    for index in 1..7 {
        deep.as_array_mut()
            .unwrap()
            .push(chain_link_entry(mid_uid(index), Some(&mid_uid(index + 1))));
    }
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
    let target = target_payment_definition();
    let parent = parent_settlement_definition(TARGET_UID);
    let parent_with_route_to = |target_uid: &str| {
        let mut parent = parent_settlement_definition(TARGET_UID);
        parent["spec"]["stages"][2]["executor"]["zhixuExecutorConfig"]
            .as_object_mut()
            .unwrap()["target"]["zhixu"] = json!(target_uid);
        parent
    };

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

    let three_node = json!([
        { "uid": TARGET_UID,
          "definition": with_static_dock_edge(target.clone(), Some(&mid_uid(2))) },
        chain_link_entry(mid_uid(2), Some(&mid_uid(3))),
        chain_link_entry(mid_uid(3), Some(TARGET_UID)),
    ]);
    let error = compile_zhixu_hook_plan(&parent, Some(&three_node), false)
        .expect_err("three-definition cycle must fail");
    let message = error.to_string();
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
    assert_eq!(artifact["schemaVersion"], "uvp.cloudArtifact.v5");
    let hooks = artifact["hooks"].as_array().unwrap();
    assert!(hooks
        .iter()
        .all(|hook| hook["sourceZhixuRef"] == json!("self")));
    assert_eq!(artifact["dockRoutes"].as_array().unwrap().len(), 1);
    assert_eq!(artifact["zhixuName"], json!("settlement"));
}

#[test]
fn rejects_spec_with_empty_or_missing_stages() {
    for mutate in ["empty", "missing"] {
        let mut definition = target_payment_definition();
        match mutate {
            "empty" => definition["spec"]["stages"] = json!([]),
            _ => {
                definition["spec"].as_object_mut().unwrap().remove("stages");
            }
        }
        let error = compile_zhixu_hook_plan(&definition, None, true)
            .expect_err("stages-less spec must fail");
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
    let mut empty = target_payment_definition();
    empty["spec"]["stages"][0]["sendSignals"]
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
    duplicate["spec"]["stages"][0]["sendSignals"]
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
        ("three-part canonical", "a.b.c"),
        ("four-part canonical", "a.b.c.d"),
    ] {
        let mut definition = target_payment_definition();
        definition["spec"]["stages"][0]["sendSignals"]
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

    let mut definition = target_payment_definition();
    definition["spec"]["stages"][0]["sendSignals"]
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
    let mut definition = target_payment_definition();
    definition["spec"]["stages"][0]["sendSignals"] = json!([{ "name": "init.str" }]);
    let plan = compile_zhixu_hook_plan(&definition, None, true)
        .expect("canonical self-reference declaration compiles");
    let capability = plan["signalCapabilities"]
        .as_array()
        .unwrap()
        .iter()
        .find(|capability| capability["declaredSignal"] == json!("init.str"))
        .expect("capability carries the canonical declaration");
    assert_eq!(capability["targetSignalName"], json!("init.str"));
    assert_eq!(capability["targetSource"], json!("payment"));
    assert_eq!(capability["targetOrderRelation"], json!("current"));

    let mut dual = target_payment_definition();
    dual["spec"]["stages"].as_array_mut().unwrap().push(json!({
        "name": "echo",
        "source": "payment",
        "receiveSignals": { "GO": "payment::init.str" },
        "sendSignals": [{ "name": "init.str" }],
        "executor": { "supplierType": "organization", "supplierID": "payment-gateway" }
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

    let mut both = target_payment_definition();
    both["spec"]["stages"][0]["sendSignals"] = json!([{ "name": "str" }, { "name": "init.str" }]);
    let error = compile_zhixu_hook_plan(&both, None, true)
        .expect_err("bare + canonical self-reference must collide");
    assert!(
        error.to_string().contains("duplicate capability"),
        "unexpected error: {error}"
    );

    let mut target = target_payment_definition();
    target["spec"]["stages"][0]["sendSignals"] = json!([{ "name": "init.str" }]);
    compile_zhixu_hook_plan(&target, None, true)
        .expect("D014 resolves canonical declarations by expanded full name");
}

#[test]
fn nucleation_id_must_be_non_blank() {
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
    let definition = json!({
            "apiVersion": "uvp/v0",
            "kind": "Zhixu",
            "metadata": { "name": "supplier_id_required" },
            "spec": {
                "platform": { "type": "evm" },
                "nucleation": { "id": "core" },
                "stages": [{
                            "name": "assign",
                            "source": "buyer",
                            "selectedStages": ["main"],
                            "sendSignals": [{ "name": "executor_selected" }],
                            "executor": { "supplierType": "organization", "supplierID": "selector-org" }
                        },
    {
                            "name": "main",
                            "source": "buyer",
                            "receiveSignals": {
                                "GO": "buyer::assign.executor_selected"
                            },
                            "executor": { "supplierType": "organization" }
                        }]
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
fn rejects_subscription_stage_bound_only_through_selected_stages() {
    let definition = json!({
            "apiVersion": "uvp/v0",
            "kind": "Zhixu",
            "metadata": { "name": "subscription_static_executor" },
            "spec": {
                "platform": { "type": "cloud" },
                "nucleation": { "id": "core" },
                "stages": [{
                            "name": "assign",
                            "source": "buyer",
                            "selectedStages": ["main"],
                            "sendSignals": [{ "name": "executor_selected" }],
                            "executor": { "supplierType": "organization", "supplierID": "selector-org" }
                        },
    {
                            "name": "main",
                            "source": "buyer",
                            "receiveSignals": {
                                "OBS": "::ANCHOR(@buyer::assign.executor_selected)"
                            }
                        }]
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
    let definition = json!({
            "apiVersion": "uvp/v0",
            "kind": "Zhixu",
            "metadata": { "name": "unmaterializable_watcher" },
            "spec": {
                "platform": { "type": "evm" },
                "nucleation": { "id": "core" },
                "stages": [{
                            "name": "assign",
                            "source": "buyer",
                            "selectedStages": ["main"],
                            "sendSignals": [{ "name": "executor_selected" }],
                            "executor": { "supplierType": "organization", "supplierID": "selector-org" }
                        },
    {
                            "name": "main",
                            "source": "buyer",
                            "receiveSignals": {
                                "OBS": "buyer::assign.executor_selected"
                            }
                        }]
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

    compile_cloud_artifact(&definition, None, true)
        .expect("cloud target must not enforce on-chain materialization");
}

#[test]
fn rejects_receive_signal_keys_with_separators() {
    let mut definition = target_payment_definition();
    definition["spec"]["stages"][0]["receiveSignals"]["BAD.KEY"] = json!("payment::init.execute");
    let error = compile_zhixu_hook_plan(&definition, None, true)
        .expect_err("receiveSignals key containing '.' must fail");
    let message = error.to_string();
    assert!(
        message.contains("receiveSignals.BAD.KEY")
            && message.contains("must not contain '.', '#' or whitespace"),
        "unexpected error: {message}"
    );

    let mut definition = target_payment_definition();
    definition["spec"]["stages"][0]["receiveSignals"]["BAD#KEY"] = json!("payment::init.execute");
    let error = compile_zhixu_hook_plan(&definition, None, true)
        .expect_err("receiveSignals key containing '#' must fail");
    assert!(
        error
            .to_string()
            .contains("must not contain '.', '#' or whitespace"),
        "unexpected error: {error}"
    );

    let mut definition = target_payment_definition();
    definition["spec"]["stages"][0]["receiveSignals"]["BAD KEY"] = json!("payment::init.execute");
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
    let definition = mint_definitions(&[
        mint_stage_value(
            "main",
            "producer",
            json!({ "SPAWN": "::ANCHOR(@buyer::retail.ack)" }),
            &["smart_contract"],
        ),
        mint_stage_value(
            "retail",
            "buyer",
            json!({ "SPAWN": "::ANCHOR(@producer::main.smart_contract)" }),
            &["ack"],
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
    let definition = mint_definitions(&[
        emitter_stage_value("main", "producer", &["smart_contract"]),
        mint_stage_value(
            "retail",
            "buyer",
            json!({ "SPAWN": "::ANCHOR(@producer::main.smart_contract)" }),
            &["ack"],
        ),
    ]);
    compile_zhixu_hook_plan(&definition, None, true)
        .expect("acyclic mint subscription chain must compile for hook_plan");
    compile_cloud_artifact(&definition, None, true)
        .expect("acyclic mint subscription chain must compile for cloud");
}

#[test]
fn still_rejects_mint_stage_subscribing_its_own_source() {
    let definition = mint_definitions(&[
        emitter_stage_value("main", "producer", &["smart_contract"]),
        mint_stage_value(
            "retail",
            "producer",
            json!({ "SPAWN": "::ANCHOR(@producer::main.smart_contract)" }),
            &["ack"],
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
fn allows_single_mint_stage_with_multiple_birth_facts() {
    let definition = mint_definitions(&[
        emitter_stage_value("main", "producer", &["smart_contract"]),
        emitter_stage_value("ship", "distributor", &["manifest"]),
        mint_stage_value(
            "retail",
            "buyer",
            json!({
                "SPAWN": "::ANCHOR(@producer::main.smart_contract)",
                "STORE": "::ANCHOR(@distributor::ship.manifest)"
            }),
            &["ack", "shelve"],
        ),
    ]);
    compile_zhixu_hook_plan(&definition, None, true)
        .expect("one mint stage with multiple birth facts must compile for hook_plan");
    compile_cloud_artifact(&definition, None, true)
        .expect("one mint stage with multiple birth facts must compile for cloud");
}

fn mint_stage_value(
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
            "supplierID": format!("{name}-executor")
        }
    })
}

fn emitter_stage_value(name: &str, source: &str, send_signals: &[&str]) -> Value {
    let mut signals = send_signals.to_vec();
    signals.push("seed");
    json!({
        "name": name,
        "source": source,
        "receiveSignals": { "PUBLISH": format!("{source}::{name}.seed") },
        "sendSignals": signals
            .iter()
            .map(|signal| json!({ "name": signal }))
            .collect::<Vec<_>>(),
        "executor": {
            "supplierType": "organization",
            "supplierID": format!("{name}-executor")
        }
    })
}

fn mint_definitions(stages: &[Value]) -> Value {
    json!({
        "apiVersion": "uvp/v0",
        "kind": "Zhixu",
        "metadata": { "name": "mint_cycle" },
        "spec": {
            "platform": { "type": "cloud" },
            "nucleation": { "id": "core" },
            "stages": stages.to_vec()
        }
    })
}

#[test]
fn rejects_mint_birth_key_colliding_with_dock_entrance_key() {
    let mut definition = target_payment_definition();
    definition["spec"]["stages"]
        .as_array_mut()
        .unwrap()
        .push(mint_stage_value(
            "retail",
            "buyer",
            json!({ "SPAWN": "::ANCHOR(@payment::init.execute)" }),
            &["ack"],
        ));
    let error = compile_zhixu_hook_plan(&definition, None, true)
        .expect_err("mint birth key colliding with a dock entrance key must fail");
    let message = error.to_string();
    assert!(
        message.contains("payment::init.execute")
            && message.contains("dock entrance port(s) (payment_service.execute)")
            && message.contains("mint stage(s) (retail)")
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

    let mut definition = target_payment_definition();
    definition["spec"]["stages"]
        .as_array_mut()
        .unwrap()
        .push(mint_stage_value(
            "retail",
            "buyer",
            json!({ "SPAWN": "::ANCHOR(@payment::settle.cmp)" }),
            &["ack"],
        ));
    compile_zhixu_hook_plan(&definition, None, true)
        .expect("a mint birth key disjoint from dock entrance keys must compile");
    compile_cloud_artifact(&definition, None, true)
        .expect("a mint birth key disjoint from dock entrance keys must compile (cloud)");
}

#[test]
fn rejects_dock_entrance_key_published_twice_across_new_interfaces() {
    let mut definition = target_payment_definition();
    definition["spec"]["stages"][0]["receiveSignals"]["DOCK_EXECUTE_2"] =
        json!("payment::init.execute");
    definition["spec"]["dockInterface"]["payment_retry"] = json!({
        "orderModes": ["new"],
        "inputs": {
            "execute": { "hook": "init#DOCK_EXECUTE_2" }
        }
    });
    let error = compile_zhixu_hook_plan(&definition, None, true)
        .expect_err("the same entrance fact key published by two new-mode ports must fail");
    let message = error.to_string();
    assert!(
        message.contains("payment::init.execute")
            && message
                .contains("dock entrance port(s) (payment_retry.execute, payment_service.execute)")
            && message.contains("出生通道键并集查重"),
        "unexpected error: {message}"
    );

    let mut definition = target_payment_definition();
    definition["spec"]["stages"][0]["receiveSignals"]["DOCK_EXECUTE_2"] =
        json!("payment::init.execute");
    definition["spec"]["dockInterface"]["payment_retry"] = json!({
        "orderModes": ["existing"],
        "inputs": {
            "execute": { "hook": "init#DOCK_EXECUTE_2" }
        }
    });
    compile_zhixu_hook_plan(&definition, None, true)
        .expect("existing-mode input ports are not birth anchors and stay outside the union");
}

fn counted_definition(stages: Vec<Value>) -> Value {
    json!({
        "apiVersion": "uvp/v0",
        "kind": "Zhixu",
        "metadata": { "name": "counted" },
        "spec": {
            "platform": { "type": "cloud" },
            "nucleation": { "id": "core" },
            "stages": stages
        }
    })
}

#[test]
fn error_string_is_truncated_at_the_boundary() {
    let stages = (0..256)
        .map(|index| {
            json!({
                "name": format!("s{index:03}"),
                "source": "bad source",
                "receiveSignals": { "PUBLISH": "src::main.seed" },
                "sendSignals": [{ "name": "seed" }],
                "executor": { "supplierType": "organization", "supplierID": "counter" }
            })
        })
        .collect::<Vec<_>>();
    let definition = counted_definition(stages);
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

const SETTLE_ADMISSION: &str = "payment::init.str & ~(control.cxl +14d)";

#[test]
fn admissions_compile_into_both_targets() {
    let mut definition = target_payment_definition();
    definition["spec"]["stages"][2]["sendSignals"][0]["validWhen"] = json!([SETTLE_ADMISSION]);
    let plan = compile_zhixu_hook_plan(&definition, None, true)
        .expect("gated sendSignal compiles for hook_plan");
    let admissions = plan["admissions"].as_array().unwrap();
    assert_eq!(
        admissions.len(),
        1,
        "only the validWhen-bearing entry admits: {admissions:?}"
    );
    let admission = &admissions[0];
    assert_eq!(admission["stageIdentifier"], json!("settle"));
    assert_eq!(admission["signalName"], json!("settle.cmp"));
    assert_eq!(admission["rawExpression"], json!([SETTLE_ADMISSION]));
    assert_eq!(
        admission["normalizedExpression"],
        json!("payment::init.str&~(control.cxl+14d)")
    );
    assert!(
        admission["ast"].is_object(),
        "hook_plan carries the parsed AST"
    );
    assert_eq!(
        admissions[0]["dependencies"],
        json!([
            { "kind": "negative", "source": "payment", "signalName": "control.cxl" },
            { "kind": "positive", "source": "payment", "signalName": "init.str" },
        ]),
        "hook_plan carries the full dependency form: {admission:?}"
    );

    let cloud = compile_cloud_artifact(&definition, None, true)
        .expect("cloud target compiles the same admission");
    let cloud_admissions = cloud["admissions"].as_array().unwrap();
    assert_eq!(cloud_admissions.len(), 1);
    let cloud_admission = &cloud_admissions[0];
    assert_eq!(cloud_admission["signalName"], json!("settle.cmp"));
    assert_eq!(
        cloud_admission["cloudAst"]["schemaVersion"],
        json!("uvp.cloudAst.v1")
    );
    assert_eq!(
        cloud_admission["dependencies"],
        json!([
            { "signalName": "control.cxl", "dependencyKind": "negative" },
            { "signalName": "init.str", "dependencyKind": "positive" },
        ]),
        "cloud dependencies drop the timer dimension: {cloud_admission:?}"
    );
}

#[test]
fn admission_self_reference_is_rejected_in_both_targets() {
    for target in ["hook_plan", "cloud"] {
        for header in ["payment", "seller"] {
            let mut definition = target_payment_definition();
            definition["spec"]["stages"][2]["sendSignals"][0]["validWhen"] =
                json!([format!("{header}::settle.cmp & init.str")]);
            let result = if target == "hook_plan" {
                compile_zhixu_hook_plan(&definition, None, true)
            } else {
                compile_cloud_artifact(&definition, None, true)
            };
            let error = result.expect_err("self-referencing validWhen must fail");
            assert!(
                error.to_string().contains(
                    "D028 settle.sendSignals[cmp].validWhen[0]: expression addresses the declaring signal itself (settle.cmp)"
                ),
                "{target}/{header}: {error}"
            );
        }
    }

    let mut definition = target_payment_definition();
    definition["spec"]["stages"][2]["sendSignals"][0]["validWhen"] =
        json!(["seller::settle.cmp & init.str"]);
    let error = compile_zhixu_hook_plan(&definition, None, true)
        .expect_err("a same-named fact under another source class still addresses the declaring signal's fact key");
    assert!(
        error
            .to_string()
            .contains("D028 settle.sendSignals[cmp].validWhen[0]"),
        "{error}"
    );
}

#[test]
fn admission_dangling_references_are_rejected() {
    for (label, expression, needle) in [
        (
            "unknown stage",
            "payment::nothere.nosignal",
            "references unknown stage nothere",
        ),
        (
            "undeclared signal",
            "payment::init.never_declared",
            "references unknown signal init.never_declared",
        ),
        (
            "undeclared source",
            "ghost::init.str",
            "is not a declared source in this zhixu",
        ),
    ] {
        for target in ["hook_plan", "cloud"] {
            let mut definition = target_payment_definition();
            definition["spec"]["stages"][2]["sendSignals"][0]["validWhen"] = json!([expression]);
            let result = if target == "hook_plan" {
                compile_zhixu_hook_plan(&definition, None, true)
            } else {
                compile_cloud_artifact(&definition, None, true)
            };
            let error = result.expect_err(&format!("{label} ({target}) must be rejected"));
            assert!(
                error.to_string().contains(".sendSignals[cmp].validWhen[0]"),
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
    let mut minted = target_payment_definition();
    minted["spec"]["stages"][0]["sendSignals"][0]["validWhen"] = json!([SETTLE_ADMISSION]);
    minted["spec"]["stages"]
        .as_array_mut()
        .unwrap()
        .push(mint_stage_value(
            "retail",
            "buyer",
            json!({ "SPAWN": "::ANCHOR(@payment::init.str)" }),
            &["ack"],
        ));
    let error = compile_zhixu_hook_plan(&minted, None, true)
        .expect_err("validWhen on a mint SPAWN birth target must fail");
    assert!(
        error
            .to_string()
            .contains("D029 init.sendSignals[str].validWhen: init.str is a birth-anchor signal"),
        "{error}"
    );

    let mut dock_anchored = target_payment_definition();
    dock_anchored["spec"]["stages"][0]["sendSignals"]
        .as_array_mut()
        .unwrap()
        .push(json!({ "name": "execute", "validWhen": [SETTLE_ADMISSION] }));
    let error = compile_cloud_artifact(&dock_anchored, None, true)
        .expect_err("validWhen on a dock birth-anchor input must fail");
    assert!(
        error.to_string().contains(
            "D029 init.sendSignals[execute].validWhen: init.execute is a birth-anchor signal"
        ),
        "{error}"
    );

    let mut channel = target_payment_definition();
    channel["spec"]["stages"]
        .as_array_mut()
        .unwrap()
        .push(json!({
            "name": "watch",
            "source": "auditor",
            "receiveSignals": { "SPAWN": "::ANCHOR(@payment::init.str)" },
            "sendSignals": [{ "name": "seen", "validWhen": [SETTLE_ADMISSION] }],
            "executor": { "supplierType": "organization", "supplierID": "audit-watcher" }
        }));
    let error = compile_zhixu_hook_plan(&channel, None, true)
        .expect_err("validWhen on an anchorless channel stage must fail");
    assert!(
        error.to_string().contains(
            "D029 watch.sendSignals[seen].validWhen: watch is an anchorless channel stage"
        ),
        "{error}"
    );

    let mut anchored_channel = channel;
    anchored_channel["spec"]["stages"]
        .as_array_mut()
        .unwrap()
        .push(mint_stage_value(
            "retail",
            "auditor",
            json!({ "SPAWN": "::ANCHOR(@payment::init.str)" }),
            &["ack"],
        ));
    compile_zhixu_hook_plan(&anchored_channel, None, true)
        .expect("an order-anchored subscription stage may declare admissions");
}

#[test]
fn admission_on_signal_map_relay_targets_is_rejected() {
    let mut parent = parent_settlement_definition(TARGET_UID);
    parent["spec"]["stages"][2]["sendSignals"][0]["validWhen"] = json!(["buyer::confirm.cmp"]);
    for target in ["hook_plan", "cloud"] {
        let result = if target == "hook_plan" {
            compile_zhixu_hook_plan(&parent, None, true)
        } else {
            compile_cloud_artifact(&parent, None, true)
        };
        let error = result.expect_err("validWhen on a signalMap relay target must fail");
        assert!(
            error.to_string().contains(
                "D029 execute_payment.sendSignals[str].validWhen: execute_payment.str is a signalMap relay target"
            ),
            "{target}: {error}"
        );
    }

    let mut plain = parent_settlement_definition(TARGET_UID);
    plain["spec"]["stages"][2]["sendSignals"][3]["validWhen"] = json!(["buyer::confirm.cmp"]);
    let plan = compile_zhixu_hook_plan(&plain, None, true)
        .expect("validWhen on a signal outside signalMap stays legal");
    assert_eq!(plan["admissions"].as_array().map(Vec::len), Some(1));
}

#[test]
fn admission_subscription_atom_is_rejected() {
    let mut definition = target_payment_definition();
    definition["spec"]["stages"][2]["sendSignals"][0]["validWhen"] =
        json!(["::ANCHOR(@payment::init.str)"]);
    for target in ["hook_plan", "cloud"] {
        let result = if target == "hook_plan" {
            compile_zhixu_hook_plan(&definition, None, true)
        } else {
            compile_cloud_artifact(&definition, None, true)
        };
        let error = result.expect_err("subscription atoms must not ride the admission face");
        assert!(
            error.to_string().contains(
                "D030 settle.sendSignals[cmp].validWhen[0]: admission is a per-order state judgment and must not contain subscription atoms"
            ),
            "{target}: {error}"
        );
    }
}

#[test]
fn admission_name_blank_expression_duplicate_and_unknown_key_faces() {
    let mut empty_name = target_payment_definition();
    empty_name["spec"]["stages"][0]["sendSignals"]
        .as_array_mut()
        .unwrap()
        .push(json!({ "name": "" }));
    let error = compile_zhixu_hook_plan(&empty_name, None, true)
        .expect_err("an entry without a name must fail");
    assert!(
        error
            .to_string()
            .contains("D026 init.sendSignals: name is required"),
        "{error}"
    );

    let mut blank = target_payment_definition();
    blank["spec"]["stages"][0]["sendSignals"][0]["validWhen"] = json!(["   "]);
    let error = compile_cloud_artifact(&blank, None, true)
        .expect_err("a blank validWhen item must fail loudly");
    assert!(
        error
            .to_string()
            .contains("D027 init.sendSignals[str].validWhen[0]: must be a non-blank item"),
        "{error}"
    );

    let mut empty_array = target_payment_definition();
    empty_array["spec"]["stages"][0]["sendSignals"][0]["validWhen"] = json!([]);
    let error = compile_cloud_artifact(&empty_array, None, true)
        .expect_err("an empty validWhen array must fail loudly (drop the key instead)");
    assert!(
        error.to_string().contains(
            "D027 init.sendSignals[str].validWhen: must be a non-empty array; drop the key to declare unconditional admission"
        ),
        "{error}"
    );

    let mut duplicate = target_payment_definition();
    duplicate["spec"]["stages"][0]["sendSignals"]
        .as_array_mut()
        .unwrap()
        .push(json!({ "name": "init.str" }));
    let error = compile_zhixu_hook_plan(&duplicate, None, true)
        .expect_err("bare + canonical self-reference must collide on the expanded capability");
    assert!(
        error.to_string().contains("D031") && error.to_string().contains("duplicate capability"),
        "{error}"
    );

    let mut unknown_key = target_payment_definition();
    unknown_key["spec"]["stages"][0]["sendSignals"]
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
    let mut definition = target_payment_definition();
    definition["spec"]["stages"][2]["sendSignals"][0]["validWhen"] =
        json!(["payment::a.settle.cmp"]);
    let error = compile_zhixu_hook_plan(&definition, None, true)
        .expect_err("a three-part signal name inside validWhen must fail");
    assert!(
        error
            .to_string()
            .contains("settle.sendSignals[cmp].validWhen[0] is invalid")
            && error.to_string().contains("stage.signal"),
        "{error}"
    );
}

#[test]
fn multi_item_admission_composes_one_and_root_and_unions_dependencies() {
    let mut definition = target_payment_definition();
    definition["spec"]["stages"][2]["sendSignals"][0]["validWhen"] =
        json!(["payment::init.str", "payment::~(control.cxl +14d)",]);
    let cloud = compile_cloud_artifact(&definition, None, true)
        .expect("a two-item validWhen compiles into one admission");
    let admissions = cloud["admissions"].as_array().unwrap();
    assert_eq!(
        admissions.len(),
        1,
        "N items still produce one admission row"
    );
    let admission = &admissions[0];
    assert_eq!(
        admission["rawExpression"],
        json!(["payment::init.str", "payment::~(control.cxl +14d)"])
    );
    assert_eq!(
        admission["cloudAst"]["root"],
        json!({
            "type": "and",
            "left": { "type": "signal", "signal": "init.str" },
            "right": {
                "type": "neg",
                "expr": {
                    "type": "delay",
                    "expr": { "type": "signal", "signal": "control.cxl" },
                    "rawDuration": "14d",
                    "durationSeconds": 14 * 24 * 60 * 60
                }
            }
        }),
        "the item roots fold under one and-root: {admission}"
    );
    assert_eq!(
        admission["dependencies"],
        json!([
            { "signalName": "init.str", "dependencyKind": "positive" },
            { "signalName": "control.cxl", "dependencyKind": "negative" },
        ]),
        "dependencies are the union across items in first-seen order: {admission}"
    );

    let plan = compile_zhixu_hook_plan(&definition, None, true)
        .expect("hook_plan composes the same items");
    let plan_admission = &plan["admissions"][0];
    assert_eq!(
        plan_admission["normalizedExpression"],
        json!("payment::init.str&~(control.cxl+14d)")
    );
    assert_eq!(
        plan_admission["ast"]["condition"]["kind"],
        json!("and"),
        "the hook_plan AST carries an and-condition over the item conditions"
    );
}

#[test]
fn multi_item_admission_requires_every_item_ready() {
    let mut definition = target_payment_definition();
    definition["spec"]["stages"][2]["sendSignals"][0]["validWhen"] =
        json!(["payment::init.str", "payment::control.cxl",]);
    let cloud = compile_cloud_artifact(&definition, None, true).unwrap();
    let ast = cloud["admissions"][0]["cloudAst"].clone();

    let evaluate = |facts: &[&str]| {
        let request = json!({
            "profile": "cloud_compat",
            "gate": "filter",
            "ast": ast,
            "now": "2026-01-01T00:00:00Z",
            "signals": facts
                .iter()
                .map(|signal| {
                    json!({
                        "source": "payment",
                        "signalName": signal,
                        "receivedAt": "2025-12-01T00:00:00Z",
                    })
                })
                .collect::<Vec<_>>(),
        });
        let output = uvp_hook_dsl::eval_compiled_hook_json(&request.to_string());
        let parsed: Value = serde_json::from_str(&output).unwrap();
        parsed["value"]["state"].clone()
    };

    assert_eq!(
        evaluate(&["init.str", "control.cxl"]),
        json!("ready"),
        "admission passes only when every item is ready"
    );
    assert_eq!(
        evaluate(&["init.str"]),
        json!("needs_more"),
        "one unsatisfied item holds the whole admission back (no per-item voting)"
    );
    assert_eq!(
        evaluate(&["control.cxl"]),
        json!("needs_more"),
        "the other single-item subset must not pass as ready either"
    );
}

#[test]
fn admission_items_must_share_one_header_source() {
    let mut definition = target_payment_definition();
    definition["spec"]["stages"]
        .as_array_mut()
        .unwrap()
        .push(json!({
            "name": "receipts",
            "source": "seller",
            "receiveSignals": { "NOTE": "seller::receipts.mark" },
            "sendSignals": [{ "name": "mark" }],
            "executor": { "supplierType": "organization", "supplierID": "receipt-writer" }
        }));
    definition["spec"]["stages"][2]["sendSignals"][0]["validWhen"] =
        json!(["payment::init.str", "seller::receipts.mark",]);
    let error = compile_cloud_artifact(&definition, None, true)
        .expect_err("items addressing different header sources must fail");
    assert!(
        error.to_string().contains(
            "D032 settle.sendSignals[cmp].validWhen: every item must address the same header source (payment); item 1 addresses seller"
        ),
        "{error}"
    );
}

#[test]
fn rejects_supplier_name_exceeding_ddl_limit() {
    let supplier = json!({ "metadata": { "name": format!(" {}", "a".repeat(101)) } });
    let message = validate_supplier_dimensions(&supplier)
        .expect_err("supplier name over 100 bytes after trim must fail");
    assert!(
        message.contains("supplier name \"aaa")
            && message.contains("exceeds 100 bytes (global_supplier.name)"),
        "unexpected error: {message}"
    );
}

#[test]
fn rejects_supplier_type_exceeding_ddl_limit() {
    let supplier = json!({
        "metadata": { "name": "escrow-bank" },
        "spec": { "supplierType": "t".repeat(61) }
    });
    let message =
        validate_supplier_dimensions(&supplier).expect_err("supplierType over 60 bytes must fail");
    assert!(
        message.contains("supplierType \"ttt")
            && message.contains("exceeds 60 bytes (global_supplier.type)"),
        "unexpected error: {message}"
    );
}

#[test]
fn rejects_supplier_real_id_type_exceeding_ddl_limit() {
    let supplier = json!({
        "metadata": { "name": "escrow-bank" },
        "spec": {
            "supplierType": "organization",
            "realIdType": "p".repeat(21)
        }
    });
    let message =
        validate_supplier_dimensions(&supplier).expect_err("realIdType over 20 bytes must fail");
    assert!(
        message.contains("realIdType \"ppp")
            && message.contains("exceeds 20 bytes (global_supplier.real_id_type)"),
        "unexpected error: {message}"
    );
}

#[test]
fn rejects_supplier_real_id_exceeding_ddl_limit() {
    let supplier = json!({
        "metadata": { "name": "escrow-bank" },
        "spec": {
            "supplierType": "organization",
            "realId": "i".repeat(101)
        }
    });
    let message =
        validate_supplier_dimensions(&supplier).expect_err("realId over 100 bytes must fail");
    assert!(
        message.contains("realId \"iii")
            && message.contains("exceeds 100 bytes (global_supplier.real_id)"),
        "unexpected error: {message}"
    );
}

#[test]
fn rejects_supplier_with_blank_name() {
    let supplier = json!({ "metadata": { "name": "   " } });
    let message = validate_supplier_dimensions(&supplier).expect_err("blank name must fail");
    assert!(
        message.contains("metadata.name is required and cannot be blank"),
        "unexpected error: {message}"
    );
}

#[test]
fn allows_supplier_at_ddl_dimension_limits() {
    let supplier = json!({
        "metadata": { "name": format!(" {}", "n".repeat(100)) },
        "spec": {
            "supplierType": "t".repeat(60),
            "realIdType": "p".repeat(20),
            "realId": "i".repeat(100)
        }
    });
    validate_supplier_dimensions(&supplier).expect("fields at the DDL limits must pass");
}

#[test]
fn rejects_metadata_name_violating_slug_pattern() {
    let definition = json!({
        "apiVersion": "uvp/v0",
        "kind": "Zhixu",
        "metadata": { "name": "Bad-Name" },
        "spec": {
            "platform": { "type": "cloud" },
            "nucleation": { "id": "core" },
            "stages": [{ "name": "main", "source": "buyer", "executor": { "supplierType": "organization", "supplierID": "org-1" } }]
        }
    });
    let error = compile_zhixu_hook_plan(&definition, None, true)
        .expect_err("metadata.name outside the slug pattern must fail");
    assert!(
        error
            .to_string()
            .contains("must match ^[a-z][a-z0-9_-]{0,99}$"),
        "unexpected error: {error}"
    );
}

#[test]
fn rejects_stage_source_exceeding_36_bytes() {
    let definition = json!({
        "apiVersion": "uvp/v0",
        "kind": "Zhixu",
        "metadata": { "name": "source_too_long" },
        "spec": {
            "platform": { "type": "cloud" },
            "nucleation": { "id": "core" },
            "stages": [{ "name": "main", "source": "a".repeat(37), "executor": { "supplierType": "organization", "supplierID": "org-1" } }]
        }
    });
    let error = compile_zhixu_hook_plan(&definition, None, true)
        .expect_err("stage source over 36 bytes must fail");
    assert!(
        error.to_string().contains("exceeds 36 bytes"),
        "unexpected error: {error}"
    );
    let error = compile_cloud_artifact(&definition, None, true)
        .expect_err("cloud target must enforce the same requirement");
    assert!(
        error.to_string().contains("exceeds 36 bytes"),
        "unexpected cloud error: {error}"
    );
}

#[test]
fn rejects_send_signal_exceeding_combined_limit() {
    let definition = json!({
        "apiVersion": "uvp/v0",
        "kind": "Zhixu",
        "metadata": { "name": "signal_too_long" },
        "spec": {
            "platform": { "type": "cloud" },
            "nucleation": { "id": "core" },
            "stages": [{
                    "name": "main",
                    "source": "buyer",
                    "sendSignals": [{ "name": "str" }, { "name": "s".repeat(96) }],
                    "receiveSignals": { "GO": "buyer::main.str" },
                    "executor": { "supplierType": "organization", "supplierID": "org-1" }
                }]
        }
    });
    let error = compile_cloud_artifact(&definition, None, true)
        .expect_err("combined sendSignal name over 100 bytes must fail");
    assert!(
        error
            .to_string()
            .contains("exceeds 100 bytes combined (individual_record.signal_name)"),
        "unexpected error: {error}"
    );
}

#[test]
fn rejects_receive_signal_key_exceeding_36_bytes() {
    let definition = json!({
        "apiVersion": "uvp/v0",
        "kind": "Zhixu",
        "metadata": { "name": "hook_key_too_long" },
        "spec": {
            "platform": { "type": "cloud" },
            "nucleation": { "id": "core" },
            "stages": [{
                    "name": "main",
                    "source": "buyer",
                    "sendSignals": [{ "name": "str" }],
                    "receiveSignals": { "K".repeat(37): "buyer::main.str" },
                    "executor": { "supplierType": "organization", "supplierID": "org-1" }
                }]
        }
    });
    let error = compile_zhixu_hook_plan(&definition, None, true)
        .expect_err("receiveSignals key over 36 bytes must fail");
    assert!(
        error.to_string().contains("key must be 1-36 bytes"),
        "unexpected error: {error}"
    );
}

#[test]
fn rejects_selected_stages_duplicate_target() {
    let definition = json!({
            "apiVersion": "uvp/v0",
            "kind": "Zhixu",
            "metadata": { "name": "dup_selected_target" },
            "spec": {
                "platform": { "type": "cloud" },
                "nucleation": { "id": "core" },
                "stages": [{
                            "name": "assign",
                            "source": "buyer",
                            "selectedStages": ["main", "main"],
                            "sendSignals": [{ "name": "executor_selected" }],
                            "executor": { "supplierType": "organization", "supplierID": "selector-org" }
                        },
    {
                            "name": "main",
                            "source": "buyer",
                            "receiveSignals": { "GO": "buyer::assign.executor_selected" },
                            "executor": { "supplierType": "organization", "supplierID": "exec-org" }
                        }]
            }
        });
    let error = compile_zhixu_hook_plan(&definition, None, true)
        .expect_err("duplicate selectedStages target must fail");
    assert!(
        error.to_string().contains("contains duplicate target main"),
        "unexpected error: {error}"
    );
}

#[test]
fn rejects_signal_map_key_exceeding_26_bytes() {
    let definition = json!({
        "apiVersion": "uvp/v0",
        "kind": "Zhixu",
        "metadata": { "name": "signal_map_key_too_long" },
        "spec": {
            "platform": { "type": "cloud" },
            "nucleation": { "id": "core" },
            "stages": [{
                    "name": "main",
                    "source": "buyer",
                    "sendSignals": [{ "name": "str" }, { "name": "done" }],
                    "receiveSignals": { "GO": "buyer::main.str" },
                    "executor": {
                        "supplierType": "zhixu",
                        "zhixuExecutorConfig": {
                            "target": null,
                            "interface": "service",
                            "order": { "mode": "new" },
                            "inputMap": { "GO": "go" },
                            "signalMap": { "k".repeat(27): "done" }
                        }
                    }
                }]
        }
    });
    let error = compile_zhixu_hook_plan(&definition, None, true)
        .expect_err("signalMap key over 26 bytes must fail");
    assert!(
        error
            .to_string()
            .contains("must not contain '.' and must be at most 26 bytes"),
        "unexpected error: {error}"
    );
    let error = compile_cloud_artifact(&definition, None, true)
        .expect_err("cloud target must enforce the same requirement");
    assert!(
        error
            .to_string()
            .contains("must not contain '.' and must be at most 26 bytes"),
        "unexpected cloud error: {error}"
    );
}

#[test]
fn rejects_executor_supplier_id_over_uuid_length() {
    let definition = json!({
        "apiVersion": "uvp/v0",
        "kind": "Zhixu",
        "metadata": { "name": "supplier_id_too_long" },
        "spec": {
            "platform": { "type": "cloud" },
            "nucleation": { "id": "core" },
            "stages": [{ "name": "main", "source": "buyer", "executor": { "supplierType": "organization", "supplierID": "a".repeat(37) } }]
        }
    });
    let error = compile_zhixu_hook_plan(&definition, None, true)
        .expect_err("executor supplierID over 36 bytes must fail");
    assert!(
        error.to_string().contains("exceeds 36 bytes"),
        "unexpected error: {error}"
    );
    let error = compile_cloud_artifact(&definition, None, true)
        .expect_err("cloud target must enforce the same requirement");
    assert!(
        error.to_string().contains("exceeds 36 bytes"),
        "unexpected cloud error: {error}"
    );
}
