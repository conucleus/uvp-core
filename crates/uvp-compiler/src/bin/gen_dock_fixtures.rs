//! 生成 dock v2 兼容性 fixture（PRD_100/PRD_102）：
//! - `fixtures/dock/v1/manifest.json`：冻结常量、目标/父定义、resolution
//!   manifest v2、全部 leaf/root/hash/ID/envelope/permit golden vectors；
//! - `fixtures/zhixu/child_order_source_switch.json`：重写后的委托 fixture
//!   （目标接口 + resolution + 独立子订单语义向量）。
//!
//! 运行：`cargo run -p uvp-compiler --bin gen_dock_fixtures`（幂等重生成）。
//! Rust/TS/Solidity/Go 的兼容测试都从同一份 manifest 消费。

use serde_json::{json, Value};
use std::path::PathBuf;

use uvp_compiler::definition_uid;
use uvp_compiler::dock;

/// 目标定义：两个具名接口——production_service[new]（建单型服务）与
/// production_evidence[existing]（只读既有事实，PRD_100 §12.1）。
fn target_production_definition() -> Value {
    json!({
        "apiVersion": "uvp/v0",
        "kind": "Zhixu",
        "metadata": { "name": "friction_wheel_production" },
        "spec": {
            "platform": { "type": "cloud" },
            "nucleation": { "id": "production-core" },
            "dockInterface": {
                "production_service": {
                    "orderModes": ["new"],
                    "inputs": {
                        "execute": { "hook": "manufacturing.intake#EXECUTE" },
                        "amend": { "hook": "manufacturing.produce#DOCK_AMEND" }
                    },
                    "outputs": {
                        "started": { "signal": "factory::manufacturing.intake.str" },
                        "completed": { "signal": "factory::manufacturing.produce.cmp" }
                    }
                },
                "production_evidence": {
                    "orderModes": ["existing"],
                    "outputs": {
                        "scrap_declared": { "signal": "factory::manufacturing.produce.scrap_created" }
                    }
                }
            },
            "taskPatterns": [
                { "name": "manufacturing", "stages": [
                    {
                        "name": "intake",
                        "source": "factory",
                        "receiveSignals": {
                            "EXECUTE": "factory::manufacturing.intake.execute"
                        },
                        "sendSignals": ["str"],
                        "executor": { "supplierType": "organization", "supplierID": "friction-factory" }
                    },
                    {
                        "name": "produce",
                        "source": "factory",
                        "receiveSignals": {
                            "RUN": "factory::manufacturing.intake.str",
                            "DOCK_AMEND": "factory::manufacturing.produce.amend"
                        },
                        "sendSignals": ["cmp", "scrap_created"],
                        "executor": { "supplierType": "organization", "supplierID": "friction-factory" }
                    }
                ]}
            ]
        }
    })
}

/// 调用方定义：new 模式生产委托（PRD_100 §12.2）+ existing 模式既有事实
/// 引用（PRD_100 §12.3）。
fn parent_sourcing_definition(target_uid: &str) -> Value {
    json!({
        "apiVersion": "uvp/v0",
        "kind": "Zhixu",
        "metadata": { "name": "sourcing" },
        "spec": {
            "platform": { "type": "cloud" },
            "nucleation": { "id": "sourcing-core" },
            "taskPatterns": [
                { "name": "procurement", "stages": [
                    {
                        "name": "confirm",
                        "source": "purchaser",
                        // P0-4 物化门：零 hook 阶段在链上永不可物化、信号没有
                        // 钩子可挂；seed 是执行者自发入口信号。
                        "receiveSignals": { "ORDER": "purchaser::procurement.confirm.seed" },
                        "sendSignals": ["cmp", "seed"],
                        "executor": { "supplierType": "organization", "supplierID": "purchaser-app" }
                    }
                ]},
                { "name": "sourcing", "stages": [
                    {
                        "name": "manufacture",
                        "source": "purchaser",
                        "receiveSignals": { "EXECUTE": "purchaser::procurement.confirm.cmp" },
                        "sendSignals": ["str", "cmp"],
                        "executor": {
                            "supplierType": "zhixu",
                            "zhixuExecutorConfig": {
                                "target": { "zhixu": target_uid },
                                "interface": "production_service",
                                "order": { "mode": "new" },
                                "inputMap": { "EXECUTE": "execute" },
                                "signalMap": { "str": "started", "cmp": "completed" }
                            }
                        }
                    },
                    {
                        "name": "source_evidence",
                        "source": "recycler",
                        "receiveSignals": { "READ": "recycler::sourcing.source_evidence.seed" },
                        "sendSignals": ["cmp", "seed"],
                        "executor": {
                            "supplierType": "zhixu",
                            "zhixuExecutorConfig": {
                                "target": { "zhixu": target_uid },
                                "interface": "production_evidence",
                                "order": { "mode": "existing" },
                                "signalMap": { "cmp": "scrap_declared" }
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

fn build_manifest(target: &Value, target_plan: &Value) -> Value {
    let interface = target_plan["dockInterface"].clone();
    json!({
        "schemaVersion": dock::DOCK_RESOLUTION_SCHEMA_VERSION,
        "definitions": [{
            "zhixu": definition_uid(target).expect("target uid derives"),
            "definition": target,
            "definitionRefHash": interface["definition"]["definitionRefHash"].clone(),
            "artifactHash": target_plan["planHash"].clone(),
            "published": true,
            "interfaces": interface["interfaces"].clone(),
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

fn find_interface<'a>(artifact: &'a Value, name: &str) -> &'a Value {
    artifact["interfaces"]
        .as_array()
        .expect("interfaces array")
        .iter()
        .find(|interface| interface["name"] == json!(name))
        .unwrap_or_else(|| panic!("interface {name} present"))
}

fn find_route<'a>(routes: &'a [Value], interface_name: &str) -> &'a Value {
    routes
        .iter()
        .find(|route| route["target"]["interfaceName"] == json!(interface_name))
        .unwrap_or_else(|| panic!("route on {interface_name} present"))
}

fn main() {
    let target = target_production_definition();
    let target_uid = definition_uid(&target).expect("target uid derives");
    let parent = parent_sourcing_definition(&target_uid);
    let parent_uid = definition_uid(&parent).expect("parent uid derives");

    let target_plan =
        uvp_compiler::compile_zhixu_hook_plan(&target, None, true).expect("target compiles");
    let manifest = build_manifest(&target, &target_plan);
    let parent_plan = uvp_compiler::compile_zhixu_hook_plan(&parent, Some(&manifest), false)
        .expect("parent links");
    let routes = parent_plan["dockRoutes"].as_array().expect("dock routes");
    assert_eq!(routes.len(), 2, "parent exposes one route per interface");
    let service_route = find_route(routes, "production_service");
    let evidence_route = find_route(routes, "production_evidence");
    let target_interface = target_plan["dockInterface"].clone();
    let service_interface = find_interface(&target_interface, "production_service");

    // ---- runtime domains & identity vectors ----
    let chain_id: u64 = 31337;
    let state_machine = "0x5FbDB2315678afecb367f032d93F642f64180aa3";
    let docking_module = "0xe7f1725E7734CE288F8367e1Bb143E90bb3F0512";
    // 链侧 docking module EIP-712 域 version（abiVersion 3.0 线）。
    let permit_domain_version = "3";
    let evm_domain = dock::evm_runtime_domain(chain_id, state_machine).expect("address");
    let cloud_domain =
        dock::cloud_runtime_domain("uvp-cloud-deployment-fixture", "uvp-cloud-security-fixture");
    let local_order_key = dock::local_order_key("order-fixture-001");
    let service_route_id = word(&service_route["routeId"]);
    let service_route_hash = word(&service_route["routeHash"]);
    let parent_ref = dock::definition_ref_hash(&parent_uid);
    // The synthetic runtime vector must use the same local plan namespace
    // emitted in `route.local.planId`; otherwise a consumer can pass the
    // fixture while deriving a different dockInstanceId in production.
    let parent_plan_id = word(&parent_plan["planId"]);
    let target_ref = word(&service_route["target"]["definitionRefHash"]);
    let new_mode = dock::mode_word("new").expect("mode word");
    let existing_mode = dock::mode_word("existing").expect("mode word");
    let service_name_hash = dock::interface_name_key("production_service");
    let evidence_name_hash = dock::interface_name_key("production_evidence");
    // new 模式：route 身份 + 本地单（幂等建单锚，A06）。
    let dock_instance = dock::dock_instance_id(
        &evm_domain,
        &parent_plan_id,
        &parent_ref,
        &local_order_key,
        &service_route_id,
        &service_route_hash,
        &new_mode,
        &service_name_hash,
        None,
    );
    let linked_order = dock::linked_order_id(&dock_instance, &target_ref);
    // existing 模式：派生加入 target order 引用（A07；云轨运行时语义）。
    let existing_order_ref = dock::target_order_ref_key("factory-a/P001");
    let evidence_route_id = word(&evidence_route["routeId"]);
    let evidence_route_hash = word(&evidence_route["routeHash"]);
    let existing_dock_instance = dock::dock_instance_id(
        &cloud_domain,
        &parent_plan_id,
        &parent_ref,
        &local_order_key,
        &evidence_route_id,
        &evidence_route_hash,
        &existing_mode,
        &evidence_name_hash,
        Some(&existing_order_ref),
    );

    // ---- input envelope（new 模式：唯一 input 绑定即出生锚）----
    let birth_binding = service_route["inputBindings"]
        .as_array()
        .expect("input bindings")
        .iter()
        .find(|binding| binding["targetPort"] == json!("execute"))
        .expect("execute binding");
    let birth_binding_hash = word(&birth_binding["bindingHash"]);
    let source_fact_set = dock::source_fact_set_hash(&[dock::canonical_signal_hash(
        "purchaser::procurement.confirm.cmp",
    )]);
    let local_stage_key = word(&service_route["local"]["stageKey"]);
    let local_hook_key = dock::hook_key("sourcing.manufacture#EXECUTE");
    let target_plan_id = word(&manifest["definitions"][0]["evmPlanId"]);
    let target_port_key = dock::port_key("execute");
    let target_input_signal = word(&birth_binding["targetSignalId"]);
    let input_payload = dock::dock_input_payload_hash(
        &dock_instance,
        &service_route_hash,
        &parent_plan_id,
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
        dock::dock_input_idempotency_key(&dock_instance, &birth_binding_hash, 0);

    // ---- output envelope ----
    let completed_output = service_route["outputBindings"]
        .as_array()
        .expect("output bindings")
        .iter()
        .find(|binding| binding["localSignalName"] == json!("cmp"))
        .expect("completed output binding");
    let output_binding_hash = word(&completed_output["bindingHash"]);
    let target_fact_id = dock::signal_key(
        &dock::keccak_word(b"factory"),
        &dock::keccak_word(b"manufacturing.produce.cmp"),
    );
    let output_idempotency =
        dock::dock_output_idempotency_key(&dock_instance, &output_binding_hash, &target_fact_id);

    // ---- entrance permit digest ----
    let permit_digest = dock::eip712_permit_digest(
        chain_id,
        docking_module,
        permit_domain_version,
        &target_plan_id,
        &target_port_key,
        &service_name_hash,
        &parent_plan_id,
        &service_route_hash,
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
        dock::merkle_proof(&leaves, &service_route_hash).expect("route leaf in root")
    };
    let interface_leaf_proof = {
        let leaves = target_interface["interfaces"]
            .as_array()
            .expect("interfaces")
            .iter()
            .map(|interface| word(&interface["interfaceRoot"]))
            .collect::<Vec<_>>();
        let service_leaf = word(&service_interface["interfaceRoot"]);
        dock::merkle_proof(&leaves, &service_leaf).expect("interface leaf in definition root")
    };
    let input_port_leaf_proof = {
        let leaves = service_interface["inputs"]
            .as_array()
            .expect("interface inputs")
            .iter()
            .map(|port| word(&port["leafHash"]))
            .collect::<Vec<_>>();
        let execute_leaf = service_interface["inputs"]
            .as_array()
            .expect("interface inputs")
            .iter()
            .find(|port| port["port"] == json!("execute"))
            .map(|port| word(&port["leafHash"]))
            .expect("execute leaf");
        dock::merkle_proof(&leaves, &execute_leaf).expect("port leaf in interface inputsRoot")
    };

    let compat = json!({
        "schemaVersion": dock::DOCK_COMPAT_SCHEMA_VERSION,
        "constants": {
            "schemaVersions": {
                "dockInterfaceArtifact": dock::DOCK_INTERFACE_ARTIFACT_SCHEMA_VERSION,
                "dockRoute": dock::DOCK_ROUTE_SCHEMA_VERSION,
                "resolution": dock::DOCK_RESOLUTION_SCHEMA_VERSION
            },
            "domains": {
                "definitionUid": uvp_compiler::DEFINITION_UID_DOMAIN,
                "definitionRef": dock::DOMAIN_DEFINITION_REF,
                "dockInterface": dock::DOMAIN_INTERFACE,
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
                "orderMode": { "new": 0, "existing": 1 },
                "orderModesMask": { "new": 1, "existing": 2 }
            },
            "permitTypeHash": dock::PERMIT_TYPEHASH_SUFFIX,
            "permitDomainVersion": permit_domain_version
        },
        "inputs": {
            "chainId": chain_id,
            "stateMachineAddress": state_machine,
            "dockingModuleAddress": docking_module,
            "cloudDeploymentId": "uvp-cloud-deployment-fixture",
            "cloudSecurityDomain": "uvp-cloud-security-fixture",
            "localOrderId": "order-fixture-001",
            "existingTargetOrderRef": "factory-a/P001",
            "parentPlanIdWord": dock::word_hex(&parent_plan_id)
        },
        "identities": {
            "targetUid": target_uid,
            "parentUid": parent_uid
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
            "interfaceNameIds": {
                "production_service": dock::word_hex(&service_name_hash),
                "production_evidence": dock::word_hex(&evidence_name_hash)
            },
            "dockRoutes": routes.clone(),
            "dockRoutesRoot": parent_plan["dockRoutesRoot"].clone(),
            "dockInterfaceRoot": parent_plan["dockInterfaceRoot"].clone(),
            "evmRuntimeDomain": dock::word_hex(&evm_domain),
            "cloudRuntimeDomain": dock::word_hex(&cloud_domain),
            "localOrderKey": dock::word_hex(&local_order_key),
            "dockInstanceId": dock::word_hex(&dock_instance),
            "linkedOrderId": dock::word_hex(&linked_order),
            "existingDockInstanceId": dock::word_hex(&existing_dock_instance),
            "sourceFactSetHash": dock::word_hex(&source_fact_set),
            "inputPayloadHash": dock::word_hex(&input_payload),
            "inputIdempotencyKey": dock::word_hex(&input_idempotency),
            "outputIdempotencyKey": dock::word_hex(&output_idempotency),
            "permitDigest": dock::word_hex(&permit_digest),
            "routeLeafProof": route_leaf_proof
                .iter()
                .map(dock::word_hex)
                .collect::<Vec<_>>(),
            "interfaceLeafProof": interface_leaf_proof
                .iter()
                .map(dock::word_hex)
                .collect::<Vec<_>>(),
            "inputPortLeafProof": input_port_leaf_proof
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

    // 重写委托 profile fixture：独立子订单语义。profile_fixtures 断言按排序
    // 比较 hook id；生成侧同口径排序，避免编译顺序漂移被误读为语义变化。
    let mut parent_hooks = parent_plan["compiledHooks"]
        .as_array()
        .expect("compiled hooks")
        .iter()
        .map(|hook| hook["hookId"].as_str().expect("hookId").to_string())
        .collect::<Vec<_>>();
    parent_hooks.sort();
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
        "name": "delegation binds resolved dock routes with independent child identity",
        "semanticVersion": "uvp.semantic.v1",
        "target": "hook_plan",
        "portable": true,
        "input": parent,
        "resolutionManifest": manifest,
        "expect": {
            "platform": "cloud",
            "hookIds": parent_hooks,
            "hookDependencyCounts": dependency_counts,
            "dockRouteCount": 2
        }
    });
    std::fs::write(
        fixtures_root().join("zhixu/child_order_source_switch.json"),
        serde_json::to_string_pretty(&child_fixture).expect("serialize fixture"),
    )
    .expect("write child fixture");

    println!("dock fixtures regenerated at {}", fixtures_root().display());
}
