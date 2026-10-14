use serde_json::json;

use crate::{mc_check, Check, CheckKind, McCheckRequest, Status};

fn demo_definition() -> serde_json::Value {
    json!({
        "apiVersion": "uvp/v0",
        "kind": "Zhixu",
        "metadata": {"name": "mc_demo_settle"},
        "spec": {
            "platform": {"type": "cloud"},
            "nucleation": {"id": "core_org"},
            "stages": [{
                "name": "escrow",
                "source": "buyer",
                "executor": {"supplierType": "organization", "supplierID": "supplier_demo"},
                "sendSignals": [
                    {"name": "held"},
                    {"name": "release", "validWhen": [
                        "buyer::escrow.held",
                        "buyer::~escrow.refund",
                        "buyer::~escrow.released",
                        "buyer::~escrow.returned"
                    ]},
                    {"name": "released"},
                    {"name": "refund", "validWhen": [
                        "buyer::escrow.held",
                        "buyer::~escrow.release",
                        "buyer::~escrow.released",
                        "buyer::~escrow.returned"
                    ]},
                    {"name": "returned"}
                ],
                "receiveSignals": {
                    "AUTO_RELEASE": "buyer::escrow.held +5s & ~escrow.returned & ~escrow.released"
                }
            }]
        }
    })
}

fn check_when(id: &str, kind: CheckKind, when: &str) -> Check {
    Check {
        id: id.to_string(),
        kind,
        description: None,
        when: Some(when.to_string()),
        predicate: None,
        intents: Vec::new(),
    }
}

fn demo_checks() -> Vec<Check> {
    vec![
        check("claim-mutex", CheckKind::BadState, "buyer::escrow.release & escrow.refund"),
        check_closed("terminal-closes-intents", "buyer::escrow.released | escrow.returned"),
        check_when("held-can-progress", CheckKind::DeadlockFree, "buyer::escrow.held & ~escrow.released & ~escrow.returned & ~escrow.release & ~escrow.refund"),
        check("terminal-reachable", CheckKind::CoReach, "buyer::escrow.released | escrow.returned"),
    ]
}

fn check(id: &str, kind: CheckKind, predicate: &str) -> Check {
    Check {
        id: id.to_string(),
        kind,
        description: None,
        when: None,
        predicate: Some(predicate.to_string()),
        intents: Vec::new(),
    }
}

fn check_closed(id: &str, when: &str) -> Check {
    Check {
        id: id.to_string(),
        kind: CheckKind::AdmissionClosed,
        description: None,
        when: Some(when.to_string()),
        predicate: None,
        intents: vec![
            "buyer::escrow.release".to_string(),
            "buyer::escrow.refund".to_string(),
        ],
    }
}

fn run(definition: serde_json::Value, checks: Vec<Check>) -> crate::McCheckReport {
    mc_check(McCheckRequest {
        definition,
        checks,
        max_states: None,
    })
    .expect("demo check should run")
}

#[test]
fn sound_demo_passes_all_check_kinds() {
    let report = run(demo_definition(), demo_checks());
    assert!(
        report.passed,
        "sound demo must pass; lint={:?} checks={:?}",
        report.lint,
        report
            .checks
            .iter()
            .map(|outcome| (outcome.id.clone(), outcome.violation.clone()))
            .collect::<Vec<_>>()
    );
    assert_eq!(report.checks.len(), 4);
}

#[test]
fn one_sided_negative_gate_fails_claim_mutex() {
    let mut definition = demo_definition();
    let signals = &mut definition["spec"]["stages"][0]["sendSignals"];
    // release 丢掉 ~refund：单边负门只挡自己先落，refund 先落仍可共存。
    signals[1]["validWhen"] = json!(["buyer::escrow.held", "buyer::~escrow.released", "buyer::~escrow.returned"]);
    let report = run(definition, demo_checks());
    let outcome = report
        .checks
        .iter()
        .find(|outcome| outcome.id == "claim-mutex")
        .expect("claim-mutex outcome");
    assert_eq!(outcome.status, Status::Fail);
    let violation = outcome.violation.as_ref().expect("violation payload");
    assert_eq!(violation["check_id"], "claim-mutex");
    let trace = violation["trace"].as_array().expect("counterexample trace");
    let lands: Vec<&str> = trace
        .iter()
        .filter_map(|step| step.get("land").and_then(|value| value.as_str()))
        .collect();
    assert!(
        lands.contains(&"buyer::escrow.refund") && lands.contains(&"buyer::escrow.release"),
        "counterexample must land both claims: {lands:?}"
    );
}

#[test]
fn bare_run_flags_dead_intent_and_reports_coexistence() {
    // dead: stuck 要求 held 与 ~held 同时成立，任何可达状态都不适格。
    let definition = json!({
        "apiVersion": "uvp/v0",
        "kind": "Zhixu",
        "metadata": {"name": "mc_demo_bare"},
        "spec": {
            "platform": {"type": "cloud"},
            "nucleation": {"id": "core_org"},
            "stages": [{
                "name": "lane",
                "source": "buyer",
                "executor": {"supplierType": "organization", "supplierID": "supplier_demo"},
                "sendSignals": [
                    {"name": "held"},
                    {"name": "stuck", "validWhen": ["buyer::lane.held", "buyer::~lane.held"]},
                    {"name": "free_go", "validWhen": ["buyer::lane.held"]},
                    {"name": "independent", "validWhen": ["buyer::~lane.held"]},
                    {"name": "done"}
                ],
                "receiveSignals": {"H": "buyer::lane.held"}
            }]
        }
    });
    let report = run(definition, Vec::new());
    assert!(!report.passed, "dead intent must fail the bare run");
    let dead = report
        .checks
        .iter()
        .find(|outcome| outcome.id == "dead-intent:buyer::lane.stuck")
        .expect("dead-intent outcome for stuck");
    assert_eq!(dead.status, Status::Fail);
    assert!(
        report
            .checks
            .iter()
            .all(|outcome| outcome.id == "dead-intent:buyer::lane.stuck" || outcome.status == Status::Pass),
        "healthy intents must pass: {:?}",
        report.checks
    );
    let pairs = report.intent_pairs.as_ref().expect("coexistence matrix");
    let lookup = |a: &str, b: &str| {
        pairs
            .iter()
            .find(|pair| {
                (pair.a == a && pair.b == b) || (pair.a == b && pair.b == a)
            })
            .unwrap_or_else(|| panic!("pair {a}/{b} missing"))
            .coexist
    };
    assert!(
        lookup("buyer::lane.free_go", "buyer::lane.independent"),
        "no cross-negative pair must coexist"
    );
    // demo 的互负门对：共存矩阵必须报 mutex。
    let demo = run(demo_definition(), Vec::new());
    let demo_pairs = demo.intent_pairs.as_ref().expect("demo matrix");
    let pair = demo_pairs
        .iter()
        .find(|pair| {
            (pair.a == "buyer::escrow.release" && pair.b == "buyer::escrow.refund")
                || (pair.a == "buyer::escrow.refund" && pair.b == "buyer::escrow.release")
        })
        .expect("release/refund pair");
    assert!(!pair.coexist, "mutual negative gates must read as mutex");
}

#[test]
fn typo_atom_is_rejected_loudly() {
    let mut checks = demo_checks();
    checks.push(check("typo", CheckKind::BadState, "buyer::escrow.ghost & escrow.held"));
    let report = run(demo_definition(), checks);
    assert!(!report.passed);
    assert!(!report.lint["ok"].as_bool().expect("lint ok flag"));
    let findings = serde_json::to_string(&report.lint).expect("lint json");
    assert!(
        findings.contains("escrow.ghost"),
        "lint must name the undeclared atom: {findings}"
    );
}

#[test]
fn state_cap_is_loud() {
    let error = mc_check(McCheckRequest {
        definition: demo_definition(),
        checks: demo_checks(),
        max_states: Some(1),
    })
    .expect_err("cap must fail loudly");
    assert!(
        error.to_string().contains("max_states"),
        "error must name the cap: {error}"
    );
}
