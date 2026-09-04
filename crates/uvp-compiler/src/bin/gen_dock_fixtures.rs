//! 生成 M0 兼容性 fixture（PRD96 §3）：
//! - `fixtures/dock/v1/manifest.json`：冻结常量、目标/父定义、resolution
//!   manifest、全部 leaf/root/hash/ID/envelope/permit golden vectors；
//! - `fixtures/zhixu/child_order_source_switch.json`：重写后的委托 fixture
//!   （目标接口 + resolution + 独立子订单语义向量）。
//!
//! 运行：`cargo run -p uvp-compiler --bin gen_dock_fixtures`（幂等重生成）。
//! Rust/TS/Solidity/Go 的兼容测试都从同一份 manifest 消费。

use serde_json::{json, Value};
use std::path::PathBuf;

use uvp_compiler::dock;

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

fn fixtures_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures")
}

fn build_manifest(target_plan: &Value) -> Value {
    let interface = target_plan["dockInterface"].clone();
    json!({
        "schemaVersion": "uvp.dock.resolution.v1",
        "definitions": [{
            "zhixu": "zx-payment-execution",
            "version": "1.2.0",
            "definitionRefHash": interface["definition"]["definitionRefHash"].clone(),
            "artifactHash": target_plan["planHash"].clone(),
            "published": true,
            "interface": interface,
            "evmPlanId": target_plan["planId"].clone(),
            "cloudArtifactId": format!(
                "artifact://{}",
                target_plan["planHash"].as_str().unwrap_or_default()
            )
        }]
    })
}

fn word(value: &Value) -> dock::Word {
    let text = value.as_str().expect("hex word");
    let body = text.strip_prefix("0x").expect("0x prefix");
    let mut out = [0u8; 32];
    for (index, chunk) in body.as_bytes().chunks(2).enumerate() {
        let high = (chunk[0] as char).to_digit(16).expect("hex") as u8;
        let low = (chunk[1] as char).to_digit(16).expect("hex") as u8;
        out[index] = (high << 4) | low;
    }
    out
}

fn main() {
    let target = target_payment_definition();
    let parent = parent_settlement_definition();

    let target_plan =
        uvp_compiler::compile_zhixu_hook_plan(&target, None, true).expect("target compiles");
    let manifest = build_manifest(&target_plan);
    let parent_plan = uvp_compiler::compile_zhixu_hook_plan(&parent, Some(&manifest), false)
        .expect("parent links");
    let routes = parent_plan["dockRoutes"].as_array().expect("one route");
    assert_eq!(routes.len(), 1, "parent exposes exactly one dock route");
    let route = &routes[0];
    let target_interface = manifest["definitions"][0]["interface"].clone();

    // ---- runtime domains & identity vectors ----
    let chain_id: u64 = 31337;
    let state_machine = "0x5FbDB2315678afecb367f032d93F642f64180aa3";
    let docking_module = "0xe7f1725E7734CE288F8367e1Bb143E90bb3F0512";
    let evm_domain = dock::evm_runtime_domain(chain_id, state_machine).expect("address");
    let cloud_domain =
        dock::cloud_runtime_domain("uvp-cloud-deployment-fixture", "uvp-cloud-security-fixture");
    let local_order_key = dock::local_order_key("order-fixture-001");
    let route_id = word(&route["routeId"]);
    let route_hash = word(&route["routeHash"]);
    let parent_ref = dock::definition_ref_hash("zx-settlement", "2.0.0");
    let target_ref = word(&target_interface["definition"]["definitionRefHash"]);
    let dock_instance = dock::dock_instance_id(
        &evm_domain,
        &parent_ref,
        &local_order_key,
        &route_id,
        &route_hash,
    );
    let linked_order = dock::linked_order_id(&dock_instance, &target_ref);

    // ---- input envelope ----
    let entrance_binding = route["entrance"].as_object().expect("entrance");
    let _ = entrance_binding;
    let entrance_input = route["inputs"]
        .as_array()
        .expect("inputs")
        .iter()
        .find(|input| input["kind"] == json!("entrance"))
        .expect("entrance input");
    let entrance_binding_hash = word(&entrance_input["bindingHash"]);
    let source_fact_set = dock::source_fact_set_hash(&[
        dock::canonical_signal_hash("buyer::checkout.confirm.cmp"),
        dock::canonical_signal_hash("buyer::checkout.cancel.cmp"),
    ]);
    let local_stage_key = word(&route["local"]["stageKey"]);
    let local_hook_key = dock::hook_key("settlement.execute_payment#EXECUTE");
    let target_plan_id = word(&manifest["definitions"][0]["evmPlanId"]);
    let target_port_key = dock::port_key("execute");
    let target_input_signal = word(&entrance_input["targetSignalId"]);
    let input_payload = dock::dock_input_payload_hash(
        &dock_instance,
        &route_hash,
        &dock::keccak_word(b"parent-plan-id-fixture"),
        &local_order_key,
        &local_stage_key,
        &local_hook_key,
        &target_plan_id,
        &linked_order,
        &target_port_key,
        &target_input_signal,
        0,
    );
    let input_idempotency =
        dock::dock_input_idempotency_key(&dock_instance, &entrance_binding_hash, 0);

    // ---- output envelope ----
    let completed_output = route["outputs"]
        .as_array()
        .expect("outputs")
        .iter()
        .find(|output| output["localSignalName"] == json!("cmp"))
        .expect("completed output");
    let output_binding_hash = word(&completed_output["bindingHash"]);
    let target_fact_id = dock::signal_key(
        &dock::keccak_word(b"payment"),
        &dock::keccak_word(b"payment_flow.settle.cmp"),
    );
    let output_idempotency =
        dock::dock_output_idempotency_key(&dock_instance, &output_binding_hash, &target_fact_id);

    // ---- entrance permit digest ----
    let permit_digest = dock::eip712_permit_digest(
        chain_id,
        docking_module,
        &target_plan_id,
        &target_port_key,
        &dock::keccak_word(b"parent-plan-id-fixture"),
        &route_hash,
        &dock_instance,
        &linked_order,
        1,
        2000000000,
    )
    .expect("permit digest");

    // ---- merkle proofs（供 TS/Solidity 测试对齐）----
    let route_leaf_proof = {
        let leaves = routes
            .iter()
            .map(|route| word(&route["routeHash"]))
            .collect::<Vec<_>>();
        dock::merkle_proof(&leaves, &route_hash).expect("route leaf in root")
    };
    let interface_leaf_proof = {
        let mut leaves = target_interface["inputs"]
            .as_array()
            .expect("interface inputs")
            .iter()
            .map(|port| word(&port["leafHash"]))
            .chain(
                target_interface["outputs"]
                    .as_array()
                    .expect("interface outputs")
                    .iter()
                    .map(|port| word(&port["leafHash"])),
            )
            .collect::<Vec<_>>();
        leaves.sort_unstable();
        leaves.dedup();
        let entrance_leaf = target_interface["inputs"]
            .as_array()
            .expect("interface inputs")
            .iter()
            .find(|port| port["port"] == json!("execute"))
            .map(|port| word(&port["leafHash"]))
            .expect("entrance leaf");
        dock::merkle_proof(&leaves, &entrance_leaf).expect("entrance leaf in interface root")
    };

    let compat = json!({
        "schemaVersion": dock::DOCK_COMPAT_SCHEMA_VERSION,
        "constants": {
            "schemaVersions": {
                "dock": dock::DOCK_SCHEMA_VERSION,
                "dockInterfaceArtifact": dock::DOCK_INTERFACE_ARTIFACT_SCHEMA_VERSION,
                "dockRoute": dock::DOCK_ROUTE_SCHEMA_VERSION,
                "resolution": dock::DOCK_RESOLUTION_SCHEMA_VERSION
            },
            "domains": {
                "definitionRef": dock::DOMAIN_DEFINITION_REF,
                "interfaceInput": dock::DOMAIN_INTERFACE_INPUT,
                "interfaceOutput": dock::DOMAIN_INTERFACE_OUTPUT,
                "routeId": dock::DOMAIN_ROUTE_ID,
                "inputBinding": dock::DOMAIN_INPUT_BINDING,
                "outputBinding": dock::DOMAIN_OUTPUT_BINDING,
                "route": dock::DOMAIN_ROUTE,
                "dockInstance": dock::DOMAIN_DOCK_INSTANCE,
                "dockOrder": dock::DOMAIN_DOCK_ORDER,
                "runtimeEip155": dock::DOMAIN_RUNTIME_EIP155,
                "runtimeCloud": dock::DOMAIN_RUNTIME_CLOUD,
                "inputPayload": dock::DOMAIN_INPUT_PAYLOAD,
                "inputIdempotency": dock::DOMAIN_INPUT_IDEMPOTENCY,
                "outputIdempotency": dock::DOMAIN_OUTPUT_IDEMPOTENCY,
                "sourceFactSet": dock::DOMAIN_SOURCE_FACT_SET
            },
            "limits": {
                "maxDockInputs": dock::MAX_DOCK_INPUTS,
                "maxDockOutputs": dock::MAX_DOCK_OUTPUTS,
                "maxDockDepth": dock::MAX_DOCK_DEPTH,
                "maxPortNameBytes": dock::MAX_PORT_NAME_BYTES
            },
            "merkle": {
                "emptyRoot": dock::word_hex(&dock::EMPTY_MERKLE_ROOT),
                "pairRule": "keccak256(min(a,b) || max(a,b)) bytewise",
                "leafOrder": "sorted-unique leaves, odd tail promoted"
            },
            "enumWords": {
                "inputKind": { "signal": 0, "entrance": 1 },
                "accessPolicy": { "open": 0, "permit": 1, "linked": 2 },
                "terminal": { "none": 0, "success": 1, "failure": 2, "cancelled": 3 },
                "orderIdPolicy": { "derived-v1": 0 }
            },
            "permitTypeHash": dock::PERMIT_TYPEHASH_SUFFIX
        },
        "inputs": {
            "chainId": chain_id,
            "stateMachineAddress": state_machine,
            "dockingModuleAddress": docking_module,
            "cloudDeploymentId": "uvp-cloud-deployment-fixture",
            "cloudSecurityDomain": "uvp-cloud-security-fixture",
            "localOrderId": "order-fixture-001",
            "parentPlanIdWord": dock::word_hex(&dock::keccak_word(b"parent-plan-id-fixture"))
        },
        "targetDefinition": target,
        "parentDefinition": parent,
        "resolutionManifest": manifest,
        "expected": {
            "targetDefinitionRefHash": dock::word_hex(&target_ref),
            "parentDefinitionRefHash": dock::word_hex(&parent_ref),
            "targetPlanId": manifest["definitions"][0]["evmPlanId"].clone(),
            "targetArtifactHash": manifest["definitions"][0]["artifactHash"].clone(),
            "interfaceArtifact": target_interface,
            "dockRoutes": routes.clone(),
            "dockRoutesRoot": parent_plan["dockRoutesRoot"].clone(),
            "dockInterfaceRoot": parent_plan["dockInterfaceRoot"].clone(),
            "evmRuntimeDomain": dock::word_hex(&evm_domain),
            "cloudRuntimeDomain": dock::word_hex(&cloud_domain),
            "localOrderKey": dock::word_hex(&local_order_key),
            "dockInstanceId": dock::word_hex(&dock_instance),
            "linkedOrderId": dock::word_hex(&linked_order),
            "sourceFactSetHash": dock::word_hex(&source_fact_set),
            "inputPayloadHash": dock::word_hex(&input_payload),
            "inputIdempotencyKey": dock::word_hex(&input_idempotency),
            "outputIdempotencyKey": dock::word_hex(&output_idempotency),
            "permitDigest": dock::word_hex(&permit_digest),
            "routeLeafProof": route_leaf_proof
                .iter()
                .map(dock::word_hex)
                .collect::<Vec<_>>(),
            "entranceInterfaceLeafProof": interface_leaf_proof
                .iter()
                .map(dock::word_hex)
                .collect::<Vec<_>>()
        }
    });

    let dock_dir = fixtures_root().join("dock/v1");
    std::fs::create_dir_all(&dock_dir).expect("create dock fixture dir");
    std::fs::write(
        dock_dir.join("manifest.json"),
        serde_json::to_string_pretty(&compat).expect("serialize compat manifest"),
    )
    .expect("write compat manifest");

    // 重写委托 profile fixture：独立子订单语义（PRD96 §11 处置表 REWRITE）。
    let parent_hooks = parent_plan["compiledHooks"]
        .as_array()
        .expect("compiled hooks")
        .iter()
        .map(|hook| hook["hookId"].as_str().expect("hookId").to_string())
        .collect::<Vec<_>>();
    let mut dependency_counts = serde_json::Map::new();
    for hook in parent_plan["compiledHooks"]
        .as_array()
        .expect("compiled hooks")
    {
        dependency_counts.insert(
            hook["hookId"].as_str().expect("hookId").to_string(),
            json!(hook["dependencies"]
                .as_array()
                .map(Vec::len)
                .unwrap_or_default()),
        );
    }
    let child_fixture = json!({
        "name": "delegation binds a resolved dock route with independent child identity",
        "semanticVersion": "uvp.semantic.v1",
        "target": "hook_plan",
        "portable": true,
        "input": parent,
        "resolutionManifest": manifest,
        "expect": {
            "platform": "cloud",
            "hookIds": parent_hooks,
            "hookDependencyCounts": dependency_counts,
            "dockRouteCount": 1
        }
    });
    std::fs::write(
        fixtures_root().join("zhixu/child_order_source_switch.json"),
        serde_json::to_string_pretty(&child_fixture).expect("serialize fixture"),
    )
    .expect("write child fixture");

    println!("dock fixtures regenerated at {}", fixtures_root().display());
}
