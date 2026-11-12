//! uvp-constraints.v1 一致性 harness（Rust 线）：约束注册表自宿主于本仓
//! `protocol/uvp-constraints.v1.json`，是跨语言接受面规则
//! （zhixu / hook-dsl / dock / onchain-plan）的单一出处；链侧 TS harness
//! 对同一份表独立见证。

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

const CONSTRAINTS_ENV_VAR: &str = "UVP_CONSTRAINTS_PATH";
const PINNED_VERSION: &str = "uvp.constraints.v1";

const CONSTRAINTS_RELATIVE_PATH: &str = "protocol/uvp-constraints.v1.json";

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
             - 或确认本仓 protocol/uvp-constraints.v1.json 存在。\n\
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
             - 注册表内容 sha 只在与注册表同目录的 uvp-core/protocol/uvp-constraints.v1.meta.json 声明（两份 uvp-core 副本同批更新）。\n\
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
    let request = json!({ "target": "analyze", "definition": definition });
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
                "stages": [{
                            "name": "work",
                            "source": "buyer",
                            "receiveSignals": { "START": "buyer::work.cmp" },
                            "sendSignals": [
    { "name": "str" },
    { "name": "cmp" }
    ],
                            "executor": { "supplierType": "organization", "supplierID": "buyer-app" }
                        }]
            }
        })
}

fn stage_mut(definition: &mut Value) -> &mut Value {
    definition
        .pointer_mut("/spec/stages/0")
        .expect("base definition has one stage")
}

fn oversize_ascii(length: usize, byte: u8) -> String {
    std::iter::repeat_n(byte as char, length).collect()
}

type Probe = (fn() -> (bool, String), fn() -> (bool, String), &'static str);

fn rust_probes() -> Vec<(String, Probe)> {
    let mut probes: Vec<(String, Probe)> = Vec::new();

    probes.push((
        "file-resource-type-closed-enum".into(),
        (
            || probe_compile(base_definition()),
            || {
                probe_compile({
                    let mut d = base_definition();
                    d["spec"]["stages"][0]["fileResources"] = json!({
                        "rule_file": { "fileType": "s3", "localFile": { "path": "/tmp/x" } }
                    });
                    d
                })
            },
            "fileType must be one of",
        ),
    ));
    probes.push((
        "stage-entries-max-count".into(),
        (
            || probe_compile(base_definition()),
            || {
                probe_compile({
                    let mut d = base_definition();
                    let stages: Vec<Value> = (0..257)
                        .map(|i| {
                            json!({ "name": format!("s{i}"), "source": "buyer", "executor":
                                { "supplierType": "organization", "supplierID": format!("org{i}") } })
                        })
                        .collect();
                    d["spec"]["stages"] = json!(stages);
                    d
                })
            },
            "spec.stages contains 257 stages, limit is 256",
        ),
    ));
    probes.push((
        "compile-hooks-max-count".into(),
        (
            || probe_compile(base_definition()),
            || {
                probe_compile({
                    let mut d = base_definition();
                    let stages: Vec<Value> = (0..513)
                        .map(|i| {
                            json!({ "name": format!("s{i}"), "source": "buyer",
                                "sendSignals": [{ "name": "str" }],
                                "receiveSignals": { "GO": format!("buyer::s{i}.str") },
                                "executor": { "supplierType": "organization", "supplierID": format!("org{i}") } })
                        })
                        .collect();
                    d["spec"]["stages"] = json!(stages);
                    d
                })
            },
            "receiveSignals channels (compiled hooks) across spec.stages, limit is 512",
        ),
    ));
    probes.push((
        "executor-supplier-id-max-length".into(),
        (
            || probe_compile(base_definition()),
            || {
                probe_compile({
                    let mut d = base_definition();
                    d["spec"]["stages"][0]["executor"]["supplierID"] = json!("a".repeat(37));
                    d
                })
            },
            "exceeds 36 bytes",
        ),
    ));
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
                    stage_mut(&mut d)["name"] = json!(oversize_ascii(101, b's'));
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
        "send-signal-combined-max-length".into(),
        (
            || {
                let canonical = probe_compile({
                    let mut d = base_definition();
                    stage_mut(&mut d)["sendSignals"] =
                        json!([{ "name": "cmp" }, { "name": "work.".to_string() + &"s".repeat(95) }]);
                    d
                });
                assert_satisfy(canonical, "send-signal-combined-max-length(canonical)");
                probe_compile(base_definition())
            },
            || {
                let canonical_over = probe_compile({
                    let mut d = base_definition();
                    stage_mut(&mut d)["sendSignals"] =
                        json!([{ "name": "cmp" }, { "name": "work.".to_string() + &"s".repeat(96) }]);
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
            || probe_hook("evm_strict", "S", "buyer::main.cmp"),
            || probe_hook("evm_strict", &oversize_ascii(37, b'H'), "buyer::main.cmp"),
            "hook_name must be 1-36 characters",
        ),
    ));
    probes.push((
        "hook-source-class-max-length".into(),
        (
            || probe_hook("evm_strict", "HOOK", "buyer::main.cmp"),
            || {
                probe_hook(
                    "evm_strict",
                    "HOOK",
                    &format!("{}::main.cmp", oversize_ascii(37, b'b')),
                )
            },
            "hook source class exceeds the maximum length of 36",
        ),
    ));
    probes.push((
        "hook-source-class-charset".into(),
        (
            || probe_hook("evm_strict", "HOOK", "buyer::main.cmp"),
            || probe_hook("evm_strict", "HOOK", "buy er::main.cmp"),
            "hook source must be a plain identifier",
        ),
    ));
    probes.push((
        "hook-delay-seconds-range".into(),
        (
            || probe_hook("evm_strict", "TIMEOUT", "buyer::(pay.cmp +2592000s)"),
            || probe_hook("evm_strict", "TIMEOUT", "buyer::(pay.cmp +2592001s)"),
            "exceeds the maximum allowed delay of 2592000s",
        ),
    ));

    probes.push((
        "stage-trigger-static-plain-executor".into(),
        (
            || {
                probe_compile({
                    let mut d = base_definition();
                    stage_mut(&mut d)["sendSignals"][1]["trigger"] = json!(true);
                    d
                })
            },
            || {
                probe_compile({
                    let mut d = base_definition();
                    stage_mut(&mut d)["sendSignals"][1]["trigger"] = json!(true);
                    stage_mut(&mut d)["executor"] = json!({ "supplierType": "organization" });
                    d
                })
            },
            "trigger requires its own static individual/organization executor with supplierID",
        ),
    ));

    probes
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
         在与注册表同目录的 uvp-core/protocol/uvp-constraints.v1.meta.json 更新 contentSha256（两份 uvp-core 副本同批更新）"
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
