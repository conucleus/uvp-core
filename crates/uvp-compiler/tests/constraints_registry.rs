//! uvp-constraints.v1 一致性 harness（Rust 线）。
//!
//! 约束注册表 `uvp-protocol/protocol/uvp-constraints.v1.json` 是跨语言接受面
//! 规则（zhixu / hook-dsl / dock / onchain-plan）的单一出处。本 harness：
//!   1. 钉住注册表 version + sha256 —— 任何一处改表，三线（TS/Rust/Go）测试同声报警；
//!   2. 对 applies 含 "rust" 的每条 rule 生成边界探针（满足/违反各一）打真 validator
//!      （uvp_compiler::compile_json / uvp_hook_dsl::parse_hook_json），断言真实错误文案锚点；
//!   3. rust 线没有探针的新 rule 会让本文件硬失败（防静默漏测）。
//!
//! 读不到注册表时硬失败并给出路径/环境变量指引，绝不 skip。
//! 路径解析：优先环境变量 `UVP_CONSTRAINTS_PATH`；默认相对 crate 目录的
//! `../../../uvp-protocol/protocol/uvp-constraints.v1.json`（uvp-core 与
//! uvp-protocol 同父目录的检出布局）。

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

const CONSTRAINTS_ENV_VAR: &str = "UVP_CONSTRAINTS_PATH";
const PINNED_VERSION: &str = "uvp.constraints.v1";
/// sha256(uvp-constraints.v1.json)。改表必须三线同步更新：
///   uvp-protocol packages/compiler/test/constraints-registry.test.ts
///   uvp-core      crates/uvp-compiler/tests/constraints_registry.rs
///   miniprogram   pkg/compiler/validator/constraints_registry_test.go
const PINNED_SHA256: &str = "3b0a947f84547abcf6433939ca9a1ce53d9b6f1a47dbf6c599df2a4d0bc4b8bd";

fn default_constraints_path() -> std::path::PathBuf {
    // 测试进程 cwd = crates/uvp-compiler。
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../uvp-protocol/protocol/uvp-constraints.v1.json")
}

fn load_constraints_table() -> (String, Value) {
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
    (raw, table)
}

// ---------------------------------------------------------------------------
// 探针基元
// ---------------------------------------------------------------------------

/// compile_json / parse_hook_json 共用的错误 envelope 形状。
fn envelope_message(output: &str) -> (bool, String) {
    let envelope: Value = serde_json::from_str(output).expect("uvp compiler returns a JSON envelope");
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

/// 定义级探针：target=parse（允许未解析 route 的本地编译）。
fn probe_compile(definition: Value) -> (bool, String) {
    let request = json!({ "target": "parse", "definition": definition });
    envelope_message(&uvp_compiler::compile_json(&request.to_string()))
}

/// hook 级探针：profile = evm_strict / cloud_compat。
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

// ---------------------------------------------------------------------------
// 定义基底
// ---------------------------------------------------------------------------

/// 无 zhixu 委托的最小合法定义（定义级探针基底）。
fn base_definition() -> Value {
    json!({
        "apiVersion": "uvp/v0",
        "kind": "Zhixu",
        "metadata": {
            "name": "constraints_probe",
            "uid": "zx-constraints-probe",
            "annotations": { "version": "1.0.0" }
        },
        "spec": {
            "platform": { "type": "cloud" },
            "nucleation": { "id": "constraints-core" },
            "taskPatterns": [
                { "name": "main", "stages": [
                    {
                        "name": "work",
                        "source": "buyer",
                        "sendSignals": ["str"],
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

/// 带 uvp.dock.v1 委托 executor 的定义（dock D001-D007 探针基底）。
fn dock_definition() -> Value {
    let mut definition = base_definition();
    let stage = stage_mut(&mut definition);
    stage["receiveSignals"] = json!({ "START": "buyer::main.work.cmp" });
    stage["sendSignals"] = json!(["str", "cmp"]);
    stage["executor"] = json!({
        "supplierType": "zhixu",
        "zhixuExecutorConfig": {
            "schemaVersion": "uvp.dock.v1",
            "target": { "zhixu": "zx-target", "version": "1.0.0" },
            "order": { "idPolicy": "derived-v1" },
            "inputMap": { "START": "entrance" },
            "signalMap": { "str": "out_str", "cmp": "out_cmp" }
        }
    });
    definition
}

fn signal_map_mut(definition: &mut Value) -> &mut Value {
    definition
        .pointer_mut(
            "/spec/taskPatterns/0/stages/0/executor/zhixuExecutorConfig/signalMap",
        )
        .expect("dock definition has a signalMap")
}

fn oversize_ascii(length: usize, byte: u8) -> String {
    std::iter::repeat_n(byte as char, length).collect()
}

// ---------------------------------------------------------------------------
// rust 线探针注册表：rule id -> (satisfy, violate, 锚点)
// ---------------------------------------------------------------------------

type Probe = (fn() -> (bool, String), fn() -> (bool, String), &'static str);

fn rust_probes() -> Vec<(String, Probe)> {
    let mut probes: Vec<(String, Probe)> = Vec::new();

    // --- zhixu 定义级 ---
    probes.push((
        "zhixu-api-version-closed-enum".into(),
        (
            || probe_compile(base_definition()),
            || probe_compile({
                let mut d = base_definition();
                d["apiVersion"] = json!("uvp/v1");
                d
            }),
            "apiVersion must be uvp/v0",
        ),
    ));
    probes.push((
        "zhixu-kind-closed-enum".into(),
        (
            || probe_compile(base_definition()),
            || probe_compile({
                let mut d = base_definition();
                d["kind"] = json!("NotZhixu");
                d
            }),
            "kind must be Zhixu",
        ),
    ));
    probes.push((
        "metadata-name-required".into(),
        (
            || probe_compile(base_definition()),
            || probe_compile({
                let mut d = base_definition();
                d["metadata"]["name"] = json!("   ");
                d
            }),
            "metadata.name must be non-empty",
        ),
    ));
    probes.push((
        "metadata-name-max-length".into(),
        (
            || probe_compile(base_definition()),
            || probe_compile({
                let mut d = base_definition();
                d["metadata"]["name"] = json!(oversize_ascii(101, b'n'));
                d
            }),
            "exceeds 100 bytes (global_zhixu.name)",
        ),
    ));
    probes.push((
        "metadata-uid-max-length".into(),
        (
            || probe_compile(base_definition()),
            || probe_compile({
                let mut d = base_definition();
                d["metadata"]["uid"] = json!(oversize_ascii(65, b'u'));
                d
            }),
            "exceeds 64 bytes (global_zhixu.uid)",
        ),
    ));
    probes.push((
        "stage-identifier-max-length".into(),
        (
            || probe_compile(base_definition()),
            || probe_compile({
                let mut d = base_definition();
                stage_mut(&mut d)["name"] = json!(oversize_ascii(100, b's'));
                d
            }),
            "exceeds 100 bytes (global_stage.stage_identifier)",
        ),
    ));
    probes.push((
        "stage-source-max-length".into(),
        (
            || probe_compile(base_definition()),
            || probe_compile({
                let mut d = base_definition();
                stage_mut(&mut d)["source"] = json!(oversize_ascii(37, b'b'));
                d
            }),
            "exceeds 36 bytes",
        ),
    ));
    probes.push((
        "stage-source-required".into(),
        (
            || probe_compile(base_definition()),
            || probe_compile({
                let mut d = base_definition();
                stage_mut(&mut d)["source"] = json!("  ");
                d
            }),
            "source must be non-empty",
        ),
    ));
    probes.push((
        "stage-source-charset".into(),
        (
            || probe_compile(base_definition()),
            || probe_compile({
                let mut d = base_definition();
                stage_mut(&mut d)["source"] = json!("buy er");
                d
            }),
            "must be a plain identifier (ASCII letters, digits, '_' or '-')",
        ),
    ));
    probes.push((
        "send-signal-combined-max-length".into(),
        (
            || probe_compile(base_definition()),
            || probe_compile({
                let mut d = base_definition();
                let stage = stage_mut(&mut d);
                stage["name"] = json!(oversize_ascii(98, b's'));
                stage["sendSignals"] = json!(["s12345"]);
                d
            }),
            "exceeds 100 bytes combined (individual_record.signal_name)",
        ),
    ));

    // --- hook-dsl 级 ---
    probes.push((
        "receive-signals-key-max-length".into(),
        (
            || probe_hook("evm_strict", "S", "buyer::task.main.cmp"),
            || probe_hook("evm_strict", &oversize_ascii(37, b'H'), "buyer::task.main.cmp"),
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
            || probe_hook("cloud_compat", "SUB", "::ANCHOR(@seller::trade.listing.cmp)"),
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
            || probe_hook("cloud_compat", "SUB", "::ANCHOR(@seller::trade.listing.cmp)"),
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

    // --- dock 级 ---
    probes.push((
        "dock-schema-version-closed-enum".into(),
        (
            || probe_compile(dock_definition()),
            || probe_compile({
                let mut d = dock_definition();
                *d
                    .pointer_mut(
                        "/spec/taskPatterns/0/stages/0/executor/zhixuExecutorConfig/schemaVersion",
                    )
                    .expect("schemaVersion path") = json!("uvp.dock.v2");
                d
            }),
            "D002",
        ),
    ));
    probes.push((
        "dock-order-id-policy-closed-enum".into(),
        (
            || probe_compile(dock_definition()),
            || probe_compile({
                let mut d = dock_definition();
                *d
                    .pointer_mut(
                        "/spec/taskPatterns/0/stages/0/executor/zhixuExecutorConfig/order/idPolicy",
                    )
                    .expect("idPolicy path") = json!("sequential-v1");
                d
            }),
            "D004",
        ),
    ));
    probes.push((
        "dock-target-version-exact".into(),
        (
            || probe_compile(dock_definition()),
            || probe_compile({
                let mut d = dock_definition();
                *d
                    .pointer_mut(
                        "/spec/taskPatterns/0/stages/0/executor/zhixuExecutorConfig/target/version",
                    )
                    .expect("version path") = json!("latest");
                d
            }),
            "D003",
        ),
    ));
    probes.push((
        "dock-port-name-pattern".into(),
        (
            || probe_compile(dock_definition()),
            || probe_compile({
                let mut d = dock_definition();
                signal_map_mut(&mut d)["str"] = json!("Out-Port");
                d
            }),
            "value must be a port name matching ^[a-z][a-z0-9_]{0,31}$",
        ),
    ));
    probes.push((
        "dock-signalmap-key-max-length".into(),
        (
            || probe_compile(dock_definition()),
            || probe_compile({
                let mut d = dock_definition();
                signal_map_mut(&mut d)[oversize_ascii(27, b'a')] = json!("out_x");
                d
            }),
            "D006",
        ),
    ));
    probes.push((
        "dock-signalmap-key-forbidden-separator".into(),
        (
            || probe_compile(dock_definition()),
            || probe_compile({
                let mut d = dock_definition();
                signal_map_mut(&mut d)["bad.key"] = json!("out_x");
                d
            }),
            "D006",
        ),
    ));
    probes.push((
        "dock-signalmap-key-combined-max-length".into(),
        (
            || probe_compile(dock_definition()),
            || probe_compile({
                let mut d = dock_definition();
                let long_stage = oversize_ascii(95, b's');
                let stage = stage_mut(&mut d);
                // identifier = "main." + 95 = 100（恰好合规），组合列宽由
                // signalMap 键突破；sendSignals 置空避免 shape 层组合错误
                // 抢先中断，让 D006 组合检查成为首个 dock 错误。
                stage["name"] = json!(long_stage);
                stage["sendSignals"] = json!([]);
                stage["receiveSignals"] =
                    json!({ "START": format!("buyer::main.{long_stage}.cmp") });
                d.pointer_mut(
                    "/spec/taskPatterns/0/stages/0/executor/zhixuExecutorConfig/signalMap",
                )
                .expect("signalMap path")["s12345"] = json!("out_x");
                d
            }),
            "exceeds 100 (individual_record.signal_name)",
        ),
    ));

    probes
}

// ---------------------------------------------------------------------------
// 钉测试
// ---------------------------------------------------------------------------

#[test]
fn constraints_registry_is_pinned() {
    let (raw, table) = load_constraints_table();
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
    assert_eq!(
        hex, PINNED_SHA256,
        "约束注册表内容被修改：请逐条核对规则后同步更新三线 harness 的 sha256 钉\
         （uvp-protocol packages/compiler、uvp-core crates/uvp-compiler、Go pkg/compiler/validator）"
    );
}

#[test]
fn every_rust_rule_has_a_probe() {
    let (_, table) = load_constraints_table();
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
