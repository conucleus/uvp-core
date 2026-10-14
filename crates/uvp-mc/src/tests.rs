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

fn demo_checks() -> Vec<Check> {
    vec![
        check("claim-mutex", CheckKind::BadState, "buyer::escrow.release & escrow.refund"),
        check_closed("terminal-closes-intents", "buyer::escrow.released | escrow.returned"),
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
    assert_eq!(report.checks.len(), 2);
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

// 互模拟引理钉：不被任何延时位点读取纪元的事实，其纪元对一切求值不可
// 见——规范化（折算为 0）前后 enabled_actions 与截止推进完全一致。
#[test]
fn irrelevant_epoch_is_invisible_to_evaluation() {
    let definition = json!({
        "apiVersion": "uvp/v0",
        "kind": "Zhixu",
        "metadata": {"name": "mc_demo_epoch"},
        "spec": {
            "platform": {"type": "cloud"},
            "nucleation": {"id": "core_org"},
            "stages": [{
                "name": "lane",
                "source": "buyer",
                "executor": {"supplierType": "organization", "supplierID": "supplier_demo"},
                "sendSignals": [
                    {"name": "held"},
                    {"name": "approved"},
                    {"name": "claim", "validWhen": [
                        "buyer::lane.approved",
                        "buyer::~lane.held",
                        "buyer::~(lane.approved +10s)"
                    ]},
                    {"name": "done"}
                ],
                "receiveSignals": {"H": "buyer::lane.approved"}
            }]
        }
    });
    let vocab = crate::vocab::Vocabulary::build(&definition, &[]).expect("vocab builds");
    let held = vocab
        .fact_index
        .get("buyer::lane.held")
        .copied()
        .expect("held interned");
    let approved = vocab
        .fact_index
        .get("buyer::lane.approved")
        .copied()
        .expect("approved interned");
    assert!(
        !vocab.delay_sites.is_empty(),
        "demo must carry a delay site on approved"
    );

    let universe = vec![true; vocab.facts.len()];
    let mut with_early_noise = crate::model::McState::initial(&vocab);
    let mut with_late_noise = with_early_noise.clone();
    with_early_noise.fact_epochs[held] = Some(0);
    with_early_noise.fact_epochs[approved] = Some(0);
    with_late_noise.fact_epochs[held] = Some(7);
    with_late_noise.fact_epochs[approved] = Some(0);
    with_late_noise.now = 0;

    let times_a = with_early_noise.signal_times(&vocab);
    let times_b = with_late_noise.signal_times(&vocab);
    assert_eq!(
        crate::model::enabled_actions(&vocab, &universe, &with_early_noise, &times_a).unwrap(),
        crate::model::enabled_actions(&vocab, &universe, &with_late_noise, &times_b).unwrap(),
        "held 的纪元无位点读取，落地时刻差异不得改变任何可行动作"
    );
    assert_eq!(
        crate::model::next_deadline_epoch(&vocab, &times_a, with_early_noise.now).unwrap(),
        crate::model::next_deadline_epoch(&vocab, &times_b, with_late_noise.now).unwrap(),
    );
}

// 时间行为钉：截止推进真实发生在探索里。claim 只能在 held 后 10s 窗口内
// 落地；落地后时间推进过窗，「claim 已落 ∧ 窗已过」即坏状态，反例必须含
// advance 步。同一形态也钉住阻断项判定：窗口负位（~(held+10s)）不得被当
// 成落地位阻断项，否则 held 落地即剪光见证子树，本检查会假绿。
#[test]
fn window_closure_advances_time_and_closes_gates() {
    let definition = json!({
        "apiVersion": "uvp/v0",
        "kind": "Zhixu",
        "metadata": {"name": "mc_demo_window"},
        "spec": {
            "platform": {"type": "cloud"},
            "nucleation": {"id": "core_org"},
            "stages": [{
                "name": "lane",
                "source": "buyer",
                "executor": {"supplierType": "organization", "supplierID": "supplier_demo"},
                "sendSignals": [
                    {"name": "held"},
                    {"name": "claim", "validWhen": [
                        "buyer::lane.held",
                        "buyer::~(lane.held +10s)"
                    ]}
                ],
                "receiveSignals": {"H": "buyer::lane.held"}
            }]
        }
    });
    let checks = vec![check(
        "claim-outlives-window",
        CheckKind::BadState,
        "buyer::(lane.claim & (lane.held +10s))",
    )];
    let report = run(definition, checks);
    let outcome = report
        .checks
        .iter()
        .find(|outcome| outcome.id == "claim-outlives-window")
        .expect("window outcome");
    assert_eq!(
        outcome.status,
        Status::Fail,
        "claim 落地后推进过窗即坏状态，必须红：{:?}",
        outcome.violation
    );
    let violation = outcome.violation.as_ref().expect("violation payload");
    let trace = violation["trace"].as_array().expect("trace");
    assert!(
        trace.iter().any(|step| step.get("advance_to_epoch").is_some()),
        "反例必须含时间推进步：{trace:?}"
    );
}
