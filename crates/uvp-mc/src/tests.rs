use crate::{mc_check, CheckKind, Manifest, McCheckRequest, Status};
use serde_json::json;

fn demo_definition(with_timer: bool) -> serde_json::Value {
    let release_hook = if with_timer {
        json!("buyer::escrow.held +5s & ~escrow.returned & ~escrow.released")
    } else {
        json!("buyer::escrow.release & ~escrow.returned & ~escrow.released")
    };
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
                            "buyer::~escrow.released",
                            "buyer::~escrow.returned"
                        ]},
                        {"name": "released"},
                        {"name": "refund", "validWhen": [
                            "buyer::escrow.held",
                            "buyer::~escrow.released",
                            "buyer::~escrow.returned"
                        ]},
                        {"name": "returned"}
                    ],
                    "receiveSignals": {
                        "AUTO_RELEASE": release_hook,
                        "RETURN": "buyer::escrow.refund & ~escrow.returned & ~escrow.released"
                    }
                }]
        }
    })
}

fn demo_manifest(with_effects: bool) -> Manifest {
    let effects = if with_effects {
        json!([
            {"fact": "buyer::escrow.released", "requires_any_guards": ["escrow#AUTO_RELEASE"]},
            {"fact": "buyer::escrow.returned", "requires_any_guards": ["escrow#RETURN"]}
        ])
    } else {
        json!([])
    };
    let manifest = json!({
        "schema_version": "uvp.mc.manifest.v1",
        "effects": effects,
        "checks": [
            {
                "id": "terminal-mutex",
                "kind": "bad_state",
                "predicate": "buyer::escrow.released & escrow.returned"
            },
            {
                "id": "terminal-closes-intents",
                "kind": "admission_closed",
                "when": "buyer::escrow.released | escrow.returned",
                "intents": ["buyer::escrow.release", "buyer::escrow.refund"]
            },
            {
                "id": "held-can-progress",
                "kind": "deadlock_free",
                "when": "buyer::escrow.held & ~escrow.released & ~escrow.returned"
            },
            {
                "id": "terminal-reachable",
                "kind": "coreach",
                "predicate": "buyer::escrow.released | escrow.returned"
            },
            {
                "id": "time-settles-held",
                "kind": "time_only_closure",
                "when": "buyer::escrow.held & ~escrow.released & ~escrow.returned",
                "predicate": "buyer::escrow.released | escrow.returned"
            }
        ]
    });
    serde_json::from_value(manifest).expect("demo manifest should deserialize")
}

fn run(definition: serde_json::Value, manifest: Manifest) -> crate::McCheckReport {
    mc_check(McCheckRequest {
        definition,
        manifest,
    })
    .expect("demo check should run")
}

#[test]
fn sound_demo_passes_all_check_kinds() {
    let report = run(demo_definition(true), demo_manifest(true));
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
    assert_eq!(report.checks.len(), 5);
}

#[test]
fn missing_terminal_negative_gate_fails_admission_closure() {
    let mut definition = demo_definition(true);
    let signals = &mut definition["spec"]["stages"][0]["sendSignals"];
    signals[1]["validWhen"] = json!(["buyer::escrow.held"]);
    let report = run(definition, demo_manifest(true));
    let outcome = report
        .checks
        .iter()
        .find(|outcome| outcome.kind == CheckKind::AdmissionClosed)
        .expect("admission_closed outcome");
    assert_eq!(outcome.status, Status::Fail);
    let violation = outcome.violation.as_ref().expect("violation payload");
    assert_eq!(
        violation["check_id"], "terminal-closes-intents",
        "violation must name its check"
    );
    let trace = violation["trace"].as_array().expect("counterexample trace");
    assert!(
        trace.iter().any(|step| step.get("land").is_some()),
        "counterexample must carry a fact landing sequence: {trace:?}"
    );
}

#[test]
fn no_timer_starves_both_liveness_kinds() {
    let report = run(demo_definition(false), demo_manifest(false));
    for kind in [CheckKind::DeadlockFree, CheckKind::TimeOnlyClosure] {
        let outcome = report
            .checks
            .iter()
            .find(|outcome| outcome.kind == kind)
            .expect("liveness outcome");
        assert_eq!(
            outcome.status,
            Status::Fail,
            "{:?} must fail when neither a deadline nor an in-flight effect can move the state",
            kind
        );
    }
    let coreach = report
        .checks
        .iter()
        .find(|outcome| outcome.kind == CheckKind::CoReach)
        .expect("coreach outcome");
    assert_eq!(coreach.status, Status::Pass);
}

#[test]
fn state_cap_is_loud() {
    let mut manifest = demo_manifest(true);
    manifest.max_states = Some(1);
    let error = mc_check(McCheckRequest {
        definition: demo_definition(true),
        manifest,
    })
    .expect_err("cap must fail loudly");
    assert!(
        error.to_string().contains("max_states"),
        "error must name the cap: {error}"
    );
}
