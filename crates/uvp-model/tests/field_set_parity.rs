//! 定义结构体字段闭集的跨语言钉死（Rust 线）：本测试与 uvp-protocol 仓
//! `packages/compiler` 的 zhixu-loader KNOWN_FIELDS 测试、miniprogram 仓
//! `uvp-utils/api/core/v0` 的 json tag 反射测试对同一份
//! `fixtures/closed-sets/fields.v1.json` 做逐元素相等比对——全字段实例经
//! serde 序列化取键集，Option/skip_if 空集合字段全部填实，任一侧增删字段
//! 都会让至少一侧的测试报警，不存在单侧静默改结构的路径。

use serde_json::{json, Value};
use std::collections::BTreeSet;
use uvp_model::{
    DockInputPortSource, DockInterfaceSpec, DockOutputPortSource, Nucleation, ObjectMeta,
    ZhixuDefinition, ZhixuExecutor, ZhixuPlatform, ZhixuSendSignal, ZhixuSpec, ZhixuStage,
};

const FIELDS: &str = include_str!("../../../fixtures/closed-sets/fields.v1.json");

fn keys_of(value: &impl serde::Serialize) -> BTreeSet<String> {
    let serialized = serde_json::to_value(value).expect("full-field instance should serialize");
    let Value::Object(record) = serialized else {
        panic!("full-field instance should serialize to an object");
    };
    record.keys().cloned().collect()
}

fn pinned_group(pinned: &Value, group: &str) -> BTreeSet<String> {
    pinned[group]
        .as_array()
        .unwrap_or_else(|| panic!("field-set corpus group {group} should be an array"))
        .iter()
        .map(|item| item.as_str().unwrap_or_default().to_string())
        .collect()
}

#[test]
fn struct_field_sets_match_the_pinned_corpus() {
    let pinned: Value = serde_json::from_str(FIELDS).expect("field-set corpus should decode");
    assert_eq!(pinned["schemaVersion"], "uvp.fieldSets.v1");

    let root = ZhixuDefinition {
        api_version: "uvp/v0".to_string(),
        kind: "Zhixu".to_string(),
        metadata: ObjectMeta {
            name: "demo".to_string(),
            labels: [("k".to_string(), "v".to_string())].into_iter().collect(),
            annotations: [("a".to_string(), "b".to_string())].into_iter().collect(),
        },
        spec: ZhixuSpec {
            platform: ZhixuPlatform {
                platform_type: "cloud".to_string(),
                provider: Some("eth".to_string()),
                network: Some("sepolia".to_string()),
                version: Some("v1".to_string()),
                params: [("p".to_string(), "q".to_string())].into_iter().collect(),
            },
            nucleation: Nucleation {
                id: "core".to_string(),
                params: [("p".to_string(), "q".to_string())].into_iter().collect(),
            },
            stages: vec![ZhixuStage {
                name: "main".to_string(),
                source: "buyer".to_string(),
                mint: Some("per-fact".to_string()),
                executor: Some(ZhixuExecutor {
                    supplier_type: "organization".to_string(),
                    supplier_id: Some("org".to_string()),
                    zhixu_executor_config: Some(json!({"target": {}})),
                    selectable_resource: Some(json!({"file": {}})),
                }),
                selected_stages: vec!["main".to_string()],
                send_signals: vec![ZhixuSendSignal {
                    name: "cmp".to_string(),
                    valid_when: Some(vec!["buyer::main.cmp".to_string()]),
                }],
                receive_signals: [("START".to_string(), "buyer::main.seed".to_string())]
                    .into_iter()
                    .collect(),
                file_resources: [("doc".to_string(), json!({"type": "local"}))]
                    .into_iter()
                    .collect(),
            }],
            dock_interface: [(
                "production_service".to_string(),
                DockInterfaceSpec {
                    order_modes: vec!["new".to_string()],
                    inputs: [(
                        "execute".to_string(),
                        DockInputPortSource {
                            hook: "intake#EXECUTE".to_string(),
                        },
                    )]
                    .into_iter()
                    .collect(),
                    outputs: [(
                        "started".to_string(),
                        DockOutputPortSource {
                            signal: "factory::intake.str".to_string(),
                        },
                    )]
                    .into_iter()
                    .collect(),
                },
            )]
            .into_iter()
            .collect(),
        },
    };

    let cases: [(&str, BTreeSet<String>); 11] = [
        ("root", keys_of(&root)),
        ("metadata", keys_of(&root.metadata)),
        ("spec", keys_of(&root.spec)),
        ("platform", keys_of(&root.spec.platform)),
        ("nucleation", keys_of(&root.spec.nucleation)),
        ("stage", keys_of(&root.spec.stages[0])),
        (
            "sendSignalEntry",
            keys_of(&root.spec.stages[0].send_signals[0]),
        ),
        (
            "executor",
            keys_of(root.spec.stages[0].executor.as_ref().unwrap()),
        ),
        (
            "dockInterfaceEntry",
            keys_of(&root.spec.dock_interface["production_service"]),
        ),
        (
            "dockInputPort",
            keys_of(&root.spec.dock_interface["production_service"].inputs["execute"]),
        ),
        (
            "dockOutputPort",
            keys_of(&root.spec.dock_interface["production_service"].outputs["started"]),
        ),
    ];
    for (group, actual) in cases {
        assert_eq!(
            actual,
            pinned_group(&pinned, group),
            "struct {group} 字段闭集与钉死语料分叉：改结构必须同改语料与 TS/Go 镜像"
        );
    }
}
