//! uvp-constraints.v1 一致性 harness（Rust 线）：约束注册表
//! `uvp-protocol/protocol/uvp-constraints.v1.json` 是跨语言接受面规则
//! （zhixu / hook-dsl / dock / onchain-plan）的单一出处。

use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

const CONSTRAINTS_ENV_VAR: &str = "UVP_CONSTRAINTS_PATH";
const PINNED_VERSION: &str = "uvp.constraints.v1";

const CONSTRAINTS_RELATIVE_PATH: &str = "uvp-protocol/protocol/uvp-constraints.v1.json";

fn default_constraints_path() -> std::path::PathBuf {
    let mut root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    loop {
        let candidate = root.join(CONSTRAINTS_RELATIVE_PATH);
        if candidate.exists() {
            return candidate;
        }
        if !root.pop() {
            break;
        }
    }
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(CONSTRAINTS_RELATIVE_PATH)
}

fn load_constraints_table() -> (String, Value, std::path::PathBuf) {
    let path = std::env::var(CONSTRAINTS_ENV_VAR)
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| default_constraints_path());
    let raw = std::fs::read_to_string(&path).unwrap_or_else(|err| {
        panic!(
            "[uvp-constraints] 读不到跨语言约束注册表（硬失败，不 skip）：{}\n\
             - 设置 {CONSTRAINTS_ENV_VAR}=<uvp-constraints.v1.json 绝对路径> 覆盖；\n\
             - 或确认 uvp-protocol 仓 protocol/uvp-constraints.v1.json 存在。\n\
             原始错误：{err}",
            path.display()
        )
    });
    let table: Value = serde_json::from_str(&raw)
        .unwrap_or_else(|err| panic!("[uvp-constraints] 注册表不是合法 JSON：{err}"));
    (raw, table, path)
}

fn load_constraints_meta(registry_path: &std::path::Path) -> Value {
    let meta_path = registry_path
        .parent()
        .expect("registry path has a parent")
        .join("uvp-constraints.v1.meta.json");
    let raw = std::fs::read_to_string(&meta_path).unwrap_or_else(|err| {
        panic!(
            "[uvp-constraints] 读不到注册表 sha 声明文件（硬失败，不 skip）：{}\n\
             - 注册表内容 sha 只在 uvp-protocol 仓 protocol/uvp-constraints.v1.meta.json 声明。\n\
             原始错误：{err}",
            meta_path.display()
        )
    });
    let meta: Value = serde_json::from_str(&raw)
        .unwrap_or_else(|err| panic!("[uvp-constraints] sha 声明文件不是合法 JSON：{err}"));
    let algorithm = meta
        .get("algorithm")
        .and_then(Value::as_str)
        .expect("sha 声明文件携带 algorithm");
    assert_eq!(
        algorithm, "sha256",
        "sha 声明文件 algorithm={algorithm}，本 harness 只实现 sha256"
    );
    meta
}

fn envelope_message(output: &str) -> (bool, String) {
    let envelope: Value =
        serde_json::from_str(output).expect("uvp compiler returns a JSON envelope");
    let ok = envelope
        .get("ok")
        .and_then(Value::as_bool)
        .expect("envelope carries ok flag");
    let message = envelope
        .get("diagnostics")
        .and_then(Value::as_array)
        .map(|diagnostics| {
            diagnostics
                .iter()
                .filter_map(|d| d.get("message").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("; ")
        })
        .unwrap_or_default();
    (ok, message)
}

fn probe_compile(definition: Value) -> (bool, String) {
    let request = json!({ "target": "parse", "definition": definition });
    envelope_message(&uvp_compiler::compile_json(&request.to_string()))
}

fn probe_link(definition: Value, dock_targets: Value) -> (bool, String) {
    let request = json!({
        "target": "hook_plan",
        "definition": definition,
        "dockTargets": dock_targets,
    });
    envelope_message(&uvp_compiler::compile_json(&request.to_string()))
}

fn probe_hook(profile: &str, hook_name: &str, hook: &str) -> (bool, String) {
    let request = json!({ "profile": profile, "hookName": hook_name, "hook": hook });
    envelope_message(&uvp_hook_dsl::parse_hook_json(&request.to_string()))
}

fn assert_satisfy(outcome: (bool, String), rule: &str) {
    assert!(
        outcome.0,
        "[{rule}] 满足样例被 validator 拒绝：{}",
        outcome.1
    );
}

fn assert_violate(outcome: (bool, String), anchor: &str, rule: &str) {
    assert!(
        !outcome.0,
        "[{rule}] 违反样例被 validator 放行：本 rule 的错误锚点「{anchor}」再未触发"
    );
    assert!(
        outcome.1.contains(anchor),
        "[{rule}] 违反样例的错误文案缺锚点「{anchor}」：{}",
        outcome.1
    );
}

fn base_definition() -> Value {
    json!({
            "apiVersion": "uvp/v0",
            "kind": "Zhixu",
            "metadata": {
                "name": "constraints_probe"
            },
            "spec": {
                "platform": { "type": "cloud" },
                "nucleation": { "id": "constraints-core" },
                "taskPatterns": [
                    { "name": "main", "stages": [
                        {
                            "name": "work",
                            "source": "buyer",
                            "receiveSignals": { "START": "buyer::main.work.cmp" },
                            "sendSignals": [
    { "name": "str" },
    { "name": "cmp" }
    ],
                            "executor": { "supplierType": "organization", "supplierID": "buyer-app" }
                        }
                    ]}
                ]
            }
        })
}

fn stage_mut(definition: &mut Value) -> &mut Value {
    definition
        .pointer_mut("/spec/taskPatterns/0/stages/0")
        .expect("base definition has one stage")
}

fn target_interface_definition() -> Value {
    json!({
            "apiVersion": "uvp/v0",
            "kind": "Zhixu",
            "metadata": { "name": "constraints_target" },
            "spec": {
                "platform": { "type": "cloud" },
                "nucleation": { "id": "target-core" },
                "dockInterface": {
                    "production_service": {
                        "orderModes": ["new"],
                        "inputs": {
                            "execute": { "hook": "main.work#DOCK_ENTER" }
                        },
                        "outputs": {
                            "done": { "signal": "buyer::main.work.cmp" }
                        }
                    }
                },
                "taskPatterns": [
                    { "name": "main", "stages": [
                        {
                            "name": "work",
                            "source": "buyer",
                            "receiveSignals": {
                                "DOCK_ENTER": "buyer::main.work.enter",
                                "SELF": "buyer::main.work.seed"
                            },
                            "sendSignals": [
    { "name": "str" },
    { "name": "cmp" },
    { "name": "seed" }
    ],
                            "executor": { "supplierType": "organization", "supplierID": "target-org" }
                        }
                    ]}
                ]
            }
        })
}

const TARGET_UID: &str = "zx-0123456789abcdef0123456789abcdef";

fn target_dock_targets() -> Value {
    json!([{
        "uid": TARGET_UID,
        "definition": target_interface_definition(),
    }])
}

fn dock_definition() -> Value {
    dock_definition_with("new")
}

fn dock_definition_with(mode: &str) -> Value {
    let mut definition = base_definition();
    let stage = stage_mut(&mut definition);
    stage["receiveSignals"] = json!({ "START": "buyer::main.work.cmp" });
    stage["sendSignals"] = json!([{ "name": "str" }, { "name": "cmp" }]);
    stage["executor"] = json!({
        "supplierType": "zhixu",
        "zhixuExecutorConfig": {
            "target": { "zhixu": TARGET_UID },
            "interface": "production_service",
            "order": { "mode": mode },
            "inputMap": { "START": "execute" },
            "signalMap": { "str": "done" }
        }
    });
    definition
}

fn dock_config_mut(definition: &mut Value) -> &mut Value {
    definition
        .pointer_mut("/spec/taskPatterns/0/stages/0/executor/zhixuExecutorConfig")
        .expect("dock definition has a zhixuExecutorConfig")
}

fn signal_map_mut(definition: &mut Value) -> &mut Value {
    definition
        .pointer_mut("/spec/taskPatterns/0/stages/0/executor/zhixuExecutorConfig/signalMap")
        .expect("dock definition has a signalMap")
}

fn oversize_ascii(length: usize, byte: u8) -> String {
    std::iter::repeat_n(byte as char, length).collect()
}

type Probe = (fn() -> (bool, String), fn() -> (bool, String), &'static str);

fn rust_probes() -> Vec<(String, Probe)> {
    let mut probes: Vec<(String, Probe)> = Vec::new();

    probes.push((
        "zhixu-api-version-closed-enum".into(),
        (
            || probe_compile(base_definition()),
            || {
                probe_compile({
                    let mut d = base_definition();
                    d["apiVersion"] = json!("uvp/v1");
                    d
                })
            },
            "apiVersion must be uvp/v0",
        ),
    ));
    probes.push((
        "zhixu-kind-closed-enum".into(),
        (
            || probe_compile(base_definition()),
            || {
                probe_compile({
                    let mut d = base_definition();
                    d["kind"] = json!("NotZhixu");
                    d
                })
            },
            "kind must be Zhixu",
        ),
    ));
    probes.push((
        "metadata-name-required".into(),
        (
            || probe_compile(base_definition()),
            || {
                probe_compile({
                    let mut d = base_definition();
                    d["metadata"]["name"] = json!("   ");
                    d
                })
            },
            "metadata.name must be non-empty",
        ),
    ));
    probes.push((
        "metadata-name-max-length".into(),
        (
            || probe_compile(base_definition()),
            || {
                probe_compile({
                    let mut d = base_definition();
                    d["metadata"]["name"] = json!(oversize_ascii(101, b'n'));
                    d
                })
            },
            "exceeds 100 bytes",
        ),
    ));
    probes.push((
        "metadata-uid-not-an-input".into(),
        (
            || probe_compile(base_definition()),
            || {
                probe_compile({
                    let mut d = base_definition();
                    d["metadata"]["uid"] = json!("zx-constraints-probe");
                    d
                })
            },
            "unknown field `uid`",
        ),
    ));
    probes.push((
        "metadata-name-slug-shape".into(),
        (
            || probe_compile(base_definition()),
            || {
                probe_compile({
                    let mut d = base_definition();
                    d["metadata"]["name"] = json!("Constraints_Probe");
                    d
                })
            },
            "must match ^[a-z][a-z0-9_-]{0,99}$",
        ),
    ));
    probes.push((
        "stage-identifier-max-length".into(),
        (
            || probe_compile(base_definition()),
            || {
                probe_compile({
                    let mut d = base_definition();
                    stage_mut(&mut d)["name"] = json!(oversize_ascii(100, b's'));
                    d
                })
            },
            "exceeds 100 bytes (global_stage.stage_identifier)",
        ),
    ));
    probes.push((
        "stage-source-max-length".into(),
        (
            || probe_compile(base_definition()),
            || {
                probe_compile({
                    let mut d = base_definition();
                    stage_mut(&mut d)["source"] = json!(oversize_ascii(37, b'b'));
                    d
                })
            },
            "exceeds 36 bytes",
        ),
    ));
    probes.push((
        "stage-source-required".into(),
        (
            || probe_compile(base_definition()),
            || {
                probe_compile({
                    let mut d = base_definition();
                    stage_mut(&mut d)["source"] = json!("  ");
                    d
                })
            },
            "source must be non-empty",
        ),
    ));
    probes.push((
        "stage-source-charset".into(),
        (
            || probe_compile(base_definition()),
            || {
                probe_compile({
                    let mut d = base_definition();
                    stage_mut(&mut d)["source"] = json!("buy er");
                    d
                })
            },
            "must be a plain identifier (ASCII letters, digits, '_' or '-')",
        ),
    ));
    probes.push((
        "task-pattern-name-charset".into(),
        (
            || probe_compile(base_definition()),
            || {
                probe_compile({
                    let mut d = base_definition();
                    d["spec"]["taskPatterns"][0]["name"] = json!("1main");
                    d
                })
            },
            "must start with an ASCII letter and contain only ASCII letters, digits, '_' or '-'",
        ),
    ));
    probes.push((
        "stage-name-charset".into(),
        (
            || probe_compile(base_definition()),
            || {
                probe_compile({
                    let mut d = base_definition();
                    stage_mut(&mut d)["name"] = json!("work shop");
                    d
                })
            },
            "must start with an ASCII letter and contain only ASCII letters, digits, '_' or '-'",
        ),
    ));
    probes.push((
        "executor-supplier-type-closed-enum".into(),
        (
            || probe_compile(base_definition()),
            || {
                probe_compile({
                    let mut d = base_definition();
                    stage_mut(&mut d)["executor"]["supplierType"] = json!("org");
                    d
                })
            },
            "supplierType must be one of",
        ),
    ));
    probes.push((
        "file-resource-type-closed-enum".into(),
        (
            || {
                probe_compile({
                    let mut d = base_definition();
                    stage_mut(&mut d)["fileResources"] = json!({
                        "contract_template": { "fileType": "local", "localFile": { "path": "./t.md" } }
                    });
                    d
                })
            },
            || {
                probe_compile({
                    let mut d = base_definition();
                    stage_mut(&mut d)["fileResources"] =
                        json!({ "contract_template": { "fileType": "locale" } });
                    d
                })
            },
            "fileType must be one of",
        ),
    ));
    probes.push((
        "send-signal-combined-max-length".into(),
        (
            || {
                let canonical = probe_compile({
                    let mut d = base_definition();
                    stage_mut(&mut d)["sendSignals"] =
                        json!([{ "name": "cmp" }, { "name": "main.work.".to_string() + &"s".repeat(90) }]);
                    d
                });
                assert_satisfy(canonical, "send-signal-combined-max-length(canonical)");
                probe_compile(base_definition())
            },
            || {
                let canonical_over = probe_compile({
                    let mut d = base_definition();
                    stage_mut(&mut d)["sendSignals"] =
                        json!([{ "name": "cmp" }, { "name": "main.work.".to_string() + &"s".repeat(91) }]);
                    d
                });
                assert_violate(
                    canonical_over,
                    "exceeds 100 bytes combined (individual_record.signal_name)",
                    "send-signal-combined-max-length(canonical)",
                );
                probe_compile({
                    let mut d = base_definition();
                    let stage = stage_mut(&mut d);
                    stage["name"] = json!(oversize_ascii(98, b's'));
                    stage["sendSignals"] = json!([{ "name": "s12345" }]);
                    d
                })
            },
            "exceeds 100 bytes combined (individual_record.signal_name)",
        ),
    ));

    probes.push((
        "receive-signals-key-max-length".into(),
        (
            || probe_hook("evm_strict", "S", "buyer::task.main.cmp"),
            || {
                probe_hook(
                    "evm_strict",
                    &oversize_ascii(37, b'H'),
                    "buyer::task.main.cmp",
                )
            },
            "hook_name must be 1-36 characters",
        ),
    ));
    probes.push((
        "hook-source-class-max-length".into(),
        (
            || probe_hook("evm_strict", "HOOK", "buyer::task.main.cmp"),
            || {
                probe_hook(
                    "evm_strict",
                    "HOOK",
                    &format!("{}::task.main.cmp", oversize_ascii(37, b'b')),
                )
            },
            "hook source class exceeds the maximum length of 36",
        ),
    ));
    probes.push((
        "hook-source-class-charset".into(),
        (
            || probe_hook("evm_strict", "HOOK", "buyer::task.main.cmp"),
            || probe_hook("evm_strict", "HOOK", "buy er::task.main.cmp"),
            "hook source must be a plain identifier",
        ),
    ));
    probes.push((
        "subscription-target-source-max-length".into(),
        (
            || {
                probe_hook(
                    "cloud_compat",
                    "SUB",
                    "::ANCHOR(@seller::trade.listing.cmp)",
                )
            },
            || {
                probe_hook(
                    "cloud_compat",
                    "SUB",
                    &format!("::ANCHOR(@{}::task.main.cmp)", oversize_ascii(37, b's')),
                )
            },
            "subscription source exceeds the maximum length of 36",
        ),
    ));
    probes.push((
        "subscription-target-signal-max-length".into(),
        (
            || {
                probe_hook(
                    "cloud_compat",
                    "SUB",
                    "::ANCHOR(@seller::trade.listing.cmp)",
                )
            },
            || {
                probe_hook(
                    "cloud_compat",
                    "SUB",
                    &format!("::ANCHOR(@seller::task.main.{})", oversize_ascii(101, b'a')),
                )
            },
            "subscription target signal exceeds the maximum length of 100",
        ),
    ));
    probes.push((
        "hook-delay-seconds-range".into(),
        (
            || probe_hook("evm_strict", "TIMEOUT", "buyer::(task.pay.cmp +2592000s)"),
            || probe_hook("evm_strict", "TIMEOUT", "buyer::(task.pay.cmp +2592001s)"),
            "exceeds the maximum allowed delay of 2592000s",
        ),
    ));

    probes.push((
        "dock-order-mode-closed-enum".into(),
        (
            || probe_compile(dock_definition()),
            || {
                probe_compile({
                    let mut d = dock_definition();
                    dock_config_mut(&mut d)["order"]["mode"] = json!("reused");
                    d
                })
            },
            "must be \"new\" or \"existing\"",
        ),
    ));
    probes.push((
        "dock-target-name-slug".into(),
        (
            || probe_compile(dock_definition()),
            || {
                probe_compile({
                    let mut d = dock_definition();
                    dock_config_mut(&mut d)["target"]["zhixu"] = json!("");
                    d
                })
            },
            "D003",
        ),
    ));
    probes.push((
        "dock-port-name-pattern".into(),
        (
            || probe_compile(dock_definition()),
            || {
                probe_compile({
                    let mut d = dock_definition();
                    signal_map_mut(&mut d)["str"] = json!("Out-Port");
                    d
                })
            },
            "value must be a port name matching ^[a-z][a-z0-9_]{0,31}$",
        ),
    ));
    probes.push((
        "dock-signalmap-key-max-length".into(),
        (
            || probe_compile(dock_definition()),
            || {
                probe_compile({
                    let mut d = dock_definition();
                    signal_map_mut(&mut d)[oversize_ascii(27, b'a')] = json!("done");
                    d
                })
            },
            "D006",
        ),
    ));
    probes.push((
        "dock-signalmap-key-forbidden-separator".into(),
        (
            || probe_compile(dock_definition()),
            || {
                probe_compile({
                    let mut d = dock_definition();
                    signal_map_mut(&mut d)["bad.key"] = json!("done");
                    d
                })
            },
            "D006",
        ),
    ));
    probes.push((
        "dock-signalmap-key-combined-max-length".into(),
        (
            || probe_compile(dock_definition()),
            || {
                probe_compile({
                    let mut d = dock_definition();
                    let long_stage = oversize_ascii(95, b's');
                    let stage = stage_mut(&mut d);
                    stage["name"] = json!(long_stage);
                    stage["sendSignals"] = json!([]);
                    stage["receiveSignals"] =
                        json!({ "START": format!("buyer::main.{long_stage}.cmp") });
                    d.pointer_mut(
                        "/spec/taskPatterns/0/stages/0/executor/zhixuExecutorConfig/signalMap",
                    )
                    .expect("signalMap path")["s12345"] = json!("done");
                    d
                })
            },
            "exceeds 100 (individual_record.signal_name)",
        ),
    ));
    probes.push((
        "dock-at-least-one-mapping".into(),
        (
            || probe_compile(dock_definition()),
            || {
                probe_compile({
                    let mut d = dock_definition();
                    let config = dock_config_mut(&mut d).as_object_mut().unwrap();
                    config.remove("inputMap");
                    config.remove("signalMap");
                    d
                })
            },
            "at least one of inputMap/signalMap must bind a port",
        ),
    ));
    probes.push((
        "dock-new-mode-single-input-binding".into(),
        (
            || probe_compile(dock_definition()),
            || {
                probe_compile({
                    let mut d = dock_definition();
                    let stage = stage_mut(&mut d);
                    stage["receiveSignals"]["ALSO"] = json!("buyer::main.work.cmp");
                    dock_config_mut(&mut d)["inputMap"]["ALSO"] = json!("execute");
                    d
                })
            },
            "exactly one inputMap binding",
        ),
    ));

    probes.push((
        "dock-interface-name-pattern".into(),
        (
            || probe_compile(target_interface_definition()),
            || {
                probe_compile({
                    let mut d = target_interface_definition();
                    let dock = d["spec"]["dockInterface"].as_object_mut().unwrap();
                    let spec = dock.remove("production_service").unwrap();
                    dock.insert("ProductionService".to_string(), spec);
                    d
                })
            },
            "interface name must match ^[a-z][a-z0-9_]{0,31}$",
        ),
    ));
    probes.push((
        "dock-interface-order-modes".into(),
        (
            || probe_compile(target_interface_definition()),
            || {
                probe_compile({
                    let mut d = target_interface_definition();
                    d["spec"]["dockInterface"]["production_service"]["orderModes"] = json!([]);
                    d
                })
            },
            "orderModes must be a non-empty subset of {new, existing}",
        ),
    ));

    probes.push((
        "dock-order-mode-allowed-by-interface".into(),
        (
            || probe_link(dock_definition_with("new"), target_dock_targets()),
            || probe_link(dock_definition_with("existing"), target_dock_targets()),
            "allows orderModes",
        ),
    ));

    probes.push((
        "dock-birth-hook-single-atom-syntax".into(),
        (
            || probe_compile(target_interface_definition()),
            || {
                probe_compile({
                    let mut d = target_interface_definition();
                    d["spec"]["taskPatterns"][0]["stages"][0]["receiveSignals"]["DOCK_ENTER"] =
                        json!("buyer::main.work.enter & buyer::main.work.enter");
                    d
                })
            },
            "D013",
        ),
    ));

    probes.push((
        "interface-ports-max-count".into(),
        (
            || probe_compile(target_interface_definition()),
            || {
                probe_compile({
                    let mut d = target_interface_definition();
                    let inputs: Map<String, Value> = (0..65)
                        .map(|index| {
                            (
                                format!("p{index:02}"),
                                json!({ "hook": "main.work#DOCK_ENTER" }),
                            )
                        })
                        .collect();
                    d["spec"]["dockInterface"]["production_service"]["inputs"] =
                        Value::Object(inputs);
                    d
                })
            },
            "interface exposes 66 ports (65 inputs + 1 outputs), limit is 64",
        ),
    ));
    probes.push((
        "manifest-definitions-max-count".into(),
        (
            || probe_link(dock_definition(), target_dock_targets()),
            || {
                let mut targets = target_dock_targets()
                    .as_array()
                    .expect("dock targets carry an entry array")
                    .clone();
                for index in 0..256 {
                    targets.push(json!({ "uid": format!("zx-{index:032x}") }));
                }
                probe_link(dock_definition(), Value::Array(targets))
            },
            "dockTargets carries 257 definitions, limit is 256",
        ),
    ));

    probes.push((
        "birth-channel-key-union-uniqueness".into(),
        (
            || probe_compile(mint_union_definition(false)),
            || probe_compile(mint_union_definition(true)),
            "一事一单：同一事实至多铸一单",
        ),
    ));

    probes.push((
        "task-patterns-max-count".into(),
        (
            || probe_compile(base_definition()),
            || probe_compile(mechanical_definition(65, 1, 1)),
            "task patterns, limit is 64",
        ),
    ));
    probes.push((
        "stage-entries-max-count".into(),
        (
            || probe_compile(base_definition()),
            || probe_compile(mechanical_definition(1, 257, 1)),
            "stages across taskPatterns, limit is 256",
        ),
    ));
    probes.push((
        "compile-hooks-max-count".into(),
        (
            || probe_compile(base_definition()),
            || probe_compile(mechanical_definition(1, 256, 3)),
            "receiveSignals channels (compiled hooks) across taskPatterns, limit is 512",
        ),
    ));

    probes
}

fn mint_union_definition(same_birth_fact: bool) -> Value {
    let cellar_birth = if same_birth_fact {
        "::ANCHOR(@producer::dispatch.main.smart_contract)".to_string()
    } else {
        "::ANCHOR(@distributor::depot.ship.manifest)".to_string()
    };
    json!({
            "apiVersion": "uvp/v0",
            "kind": "Zhixu",
            "metadata": { "name": "mint_union_probe" },
            "spec": {
                "platform": { "type": "cloud" },
                "nucleation": { "id": "probe-core" },
                "taskPatterns": [
                    { "name": "dispatch", "stages": [{
                        "name": "main",
                        "source": "producer",
                        "receiveSignals": { "PUBLISH": "producer::dispatch.main.seed" },
                        "sendSignals": [
    { "name": "smart_contract" },
    { "name": "seed" }
    ],
                        "executor": { "supplierType": "organization", "supplierID": "dispatch-main" }
                    }]},
                    { "name": "depot", "stages": [{
                        "name": "ship",
                        "source": "distributor",
                        "receiveSignals": { "PUBLISH": "distributor::depot.ship.seed" },
                        "sendSignals": [
    { "name": "manifest" },
    { "name": "seed" }
    ],
                        "executor": { "supplierType": "organization", "supplierID": "depot-ship" }
                    }]},
                    { "name": "orchard", "stages": [{
                        "name": "retail",
                        "source": "buyer",
                        "receiveSignals": { "SPAWN": "::ANCHOR(@producer::dispatch.main.smart_contract)" },
                        "sendSignals": [{ "name": "ack" }],
                        "mint": "per-fact",
                        "executor": { "supplierType": "organization", "supplierID": "orchard-retail" }
                    }]},
                    { "name": "cellar", "stages": [{
                        "name": "store",
                        "source": "cellar",
                        "receiveSignals": { "SPAWN": cellar_birth },
                        "sendSignals": [{ "name": "shelve" }],
                        "mint": "per-fact",
                        "executor": { "supplierType": "organization", "supplierID": "cellar-store" }
                    }]},
                ]
            }
        })
}

fn mechanical_definition(tasks: usize, stages: usize, hooks: usize) -> Value {
    let signals = ["cmp", "str", "ack"];
    let hooks = hooks.max(1).min(signals.len());
    let mut patterns = Vec::new();
    for task_index in 0..tasks {
        let mut stage_values = Vec::new();
        for stage_index in 0..stages {
            let task = format!("t{task_index}");
            let stage = format!("s{stage_index}");
            let mut receive_signals = Map::new();
            let mut send_signals = Vec::new();
            for (hook_index, signal) in signals.iter().take(hooks).enumerate() {
                receive_signals.insert(
                    format!("H{hook_index}"),
                    Value::String(format!("buyer::{task}.{stage}.{signal}")),
                );
                send_signals.push(json!({ "name": signal }));
            }
            stage_values.push(json!({
                "name": stage,
                "source": "buyer",
                "receiveSignals": receive_signals,
                "sendSignals": send_signals,
                "executor": {
                    "supplierType": "organization",
                    "supplierID": format!("e{task_index}x{stage_index}")
                }
            }));
        }
        patterns.push(json!({ "name": format!("t{task_index}"), "stages": stage_values }));
    }
    json!({
        "apiVersion": "uvp/v0",
        "kind": "Zhixu",
        "metadata": { "name": "count_probe" },
        "spec": {
            "platform": { "type": "cloud" },
            "nucleation": { "id": "probe-core" },
            "taskPatterns": patterns,
        }
    })
}

#[test]
fn constraints_registry_is_pinned() {
    let (raw, table, registry_path) = load_constraints_table();
    let meta = load_constraints_meta(&registry_path);
    assert_eq!(
        table
            .get("version")
            .and_then(Value::as_str)
            .expect("registry carries version"),
        PINNED_VERSION,
        "约束注册表 version 漂移：三线 harness 必须同声报警"
    );
    let digest = Sha256::digest(raw.as_bytes());
    let hex: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    let declared = meta
        .get("contentSha256")
        .and_then(Value::as_str)
        .expect("sha 声明文件携带 contentSha256");
    assert_eq!(
        hex, declared,
        "约束注册表内容与 meta 声明的 sha256 不一致：请逐条核对规则后，\
         在 uvp-protocol 仓同一提交里更新 protocol/uvp-constraints.v1.meta.json 的 contentSha256"
    );
}

#[test]
fn every_rust_rule_has_a_probe() {
    let (_, table, _) = load_constraints_table();
    let rules = table
        .get("rules")
        .and_then(Value::as_array)
        .expect("registry carries rules array");
    let probes = rust_probes();
    let mut rust_rules = Vec::new();
    for rule in rules {
        let id = rule
            .get("id")
            .and_then(Value::as_str)
            .expect("rule carries id");
        let applies = rule
            .get("applies")
            .and_then(Value::as_array)
            .expect("rule carries applies");
        if applies.iter().any(|line| line == "rust") {
            rust_rules.push(id);
        }
    }
    assert!(
        !rust_rules.is_empty(),
        "注册表中没有任何 rust 线规则：要么表被改坏，要么 harness 选择器失效"
    );
    for id in &rust_rules {
        assert!(
            probes.iter().any(|(probe_id, _)| probe_id == id),
            "注册表 rule {id} 标注 applies 含 rust，但本 harness 没有注册探针；请补 probe，不要放行静默漏测"
        );
    }
    for (probe_id, _) in &probes {
        assert!(
            rust_rules.iter().any(|id| id == probe_id),
            "harness 注册了探针 {probe_id}，但注册表中它不再适用于 rust 线；请同步删除"
        );
    }
}

#[test]
fn constraints_registry_probes_rust_line() {
    for (rule, (satisfy, violate, anchor)) in rust_probes() {
        let outcome = satisfy();
        assert_satisfy(outcome, &rule);
        let outcome = violate();
        assert_violate(outcome, anchor, &rule);
    }
}
