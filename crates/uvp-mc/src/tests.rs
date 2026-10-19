use serde_json::json;
use uvp_hook_dsl::EvalState;

use crate::explore::{explore_universe, StopKind};
use crate::model::{self, McState};
use crate::vocab::Vocabulary;
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
        check(
            "claim-mutex",
            CheckKind::BadState,
            "buyer::escrow.release & escrow.refund",
        ),
        check_closed(
            "terminal-closes-intents",
            "buyer::escrow.released | escrow.returned",
        ),
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
    signals[1]["validWhen"] = json!([
        "buyer::escrow.held",
        "buyer::~escrow.released",
        "buyer::~escrow.returned"
    ]);
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
fn or_predicate_witness_via_the_other_arm_is_not_pruned_away() {
    // A5 回归钉：谓词 (arm_a | arm_b) 的见证可以只经 arm_b 臂。arm_a 的
    // 正门 never 是永死意图（base 与 ~base 同时要求，任何状态都不适
    // 格），arm_a 自身永不可落，其阻断项 lock 落地即把它永久关死；
    // arm_b 的唯一正门恰是 lock。修复前（positive_signal_atoms 下降
    // Or）must 含 arm_a：凡 lock 在案而 arm_a 缺席的后继（arm_a 永不可
    // 落，即一切含 lock 的状态）都命中剪枝整枝丢弃——而 arm_b 的唯一
    // 适格路径必经含 lock 的状态，见证被误剪、检查误 PASS。修复后 Or
    // 不下降、must 为空、剪枝关闭：lock 落地、arm_b 随之落地、谓词
    // ready，必报 Violation。
    let definition = json!({
        "apiVersion": "uvp/v0",
        "kind": "Zhixu",
        "metadata": {"name": "mc_or_prune_regression"},
        "spec": {
            "platform": {"type": "cloud"},
            "nucleation": {"id": "core_org"},
            "stages": [{
                "name": "lane",
                "source": "buyer",
                "executor": {"supplierType": "organization", "supplierID": "supplier_demo"},
                "sendSignals": [
                    {"name": "base"},
                    {"name": "lock"},
                    {"name": "never", "validWhen": ["buyer::lane.base", "buyer::~lane.base"]},
                    {"name": "arm_a", "validWhen": ["buyer::lane.never", "buyer::~lane.lock"]},
                    {"name": "arm_b", "validWhen": ["buyer::lane.lock"]}
                ],
                "receiveSignals": {"H": "buyer::lane.lock"}
            }]
        }
    });
    let report = run(
        definition,
        vec![check(
            "or-any-arm-witness",
            CheckKind::BadState,
            "buyer::lane.arm_a | lane.arm_b",
        )],
    );
    let outcome = report
        .checks
        .iter()
        .find(|outcome| outcome.id == "or-any-arm-witness")
        .expect("or-any-arm-witness outcome");
    assert_eq!(
        outcome.status,
        Status::Fail,
        "witness via arm_b must stay reachable; witness-must pruning must not descend Or arms"
    );
    let violation = outcome.violation.as_ref().expect("violation payload");
    let trace = violation["trace"].as_array().expect("counterexample trace");
    let lands: Vec<&str> = trace
        .iter()
        .filter_map(|step| step.get("land").and_then(|value| value.as_str()))
        .collect();
    assert!(
        lands.contains(&"buyer::lane.arm_b"),
        "counterexample must reach the witness through arm_b: {lands:?}"
    );
}

#[test]
fn conjunctive_predicate_pruning_keeps_its_reachability_verdict() {
    // A5 正例：纯合取谓词的 must 剪枝语义不变。veto_p/veto_q 各带一枚
    // 独立负门阻断项（must 非空、剪枝照常启用），但阻断项不落地时两者
    // 双双适格、见证 {veto_p, veto_q} 可达——剪枝必须对这条路径保持沉
    // 默，检查必报 Violation（合取臂原子是真必经项，收进 must 不误剪）。
    let definition = json!({
        "apiVersion": "uvp/v0",
        "kind": "Zhixu",
        "metadata": {"name": "mc_and_prune_regression"},
        "spec": {
            "platform": {"type": "cloud"},
            "nucleation": {"id": "core_org"},
            "stages": [{
                "name": "lane",
                "source": "buyer",
                "executor": {"supplierType": "organization", "supplierID": "supplier_demo"},
                "sendSignals": [
                    {"name": "veto_zp"},
                    {"name": "veto_zq"},
                    {"name": "veto_p", "validWhen": ["buyer::~lane.veto_zp"]},
                    {"name": "veto_q", "validWhen": ["buyer::~lane.veto_zq"]}
                ],
                "receiveSignals": {"H": "buyer::lane.veto_p"}
            }]
        }
    });
    let report = run(
        definition,
        vec![check(
            "conj-both-arms-witness",
            CheckKind::BadState,
            "buyer::lane.veto_p & lane.veto_q",
        )],
    );
    let outcome = report
        .checks
        .iter()
        .find(|outcome| outcome.id == "conj-both-arms-witness")
        .expect("conj-both-arms-witness outcome");
    assert_eq!(
        outcome.status,
        Status::Fail,
        "conjunctive witness must stay reachable with must-based pruning enabled"
    );
    let violation = outcome.violation.as_ref().expect("violation payload");
    let trace = violation["trace"].as_array().expect("counterexample trace");
    let lands: Vec<&str> = trace
        .iter()
        .filter_map(|step| step.get("land").and_then(|value| value.as_str()))
        .collect();
    assert!(
        lands.contains(&"buyer::lane.veto_p") && lands.contains(&"buyer::lane.veto_q"),
        "counterexample must land both conjunctive arms: {lands:?}"
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
            .all(|outcome| outcome.id == "dead-intent:buyer::lane.stuck"
                || outcome.status == Status::Pass),
        "healthy intents must pass: {:?}",
        report.checks
    );
    let pairs = report.intent_pairs.as_ref().expect("coexistence matrix");
    let lookup = |a: &str, b: &str| {
        pairs
            .iter()
            .find(|pair| (pair.a == a && pair.b == b) || (pair.a == b && pair.b == a))
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
    checks.push(check(
        "typo",
        CheckKind::BadState,
        "buyer::escrow.ghost & escrow.held",
    ));
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
        trace
            .iter()
            .any(|step| step.get("advance_to_epoch").is_some()),
        "反例必须含时间推进步：{trace:?}"
    );
}

struct XorShift(u64);

impl XorShift {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    fn flag(&mut self) -> bool {
        self.next() & 1 == 1
    }
}

fn gen_expr(rng: &mut XorShift, atoms: &[String], depth: usize) -> String {
    if depth == 0 || atoms.is_empty() || rng.flag() {
        let atom = &atoms[rng.below(atoms.len())];
        return match rng.below(4) {
            0 => atom.clone(),
            1 => format!("~{atom}"),
            2 => format!("{atom} + {}s", 1 + rng.below(2)),
            _ => format!("~({atom} + {}s)", 1 + rng.below(2)),
        };
    }
    let left = gen_expr(rng, atoms, depth - 1);
    let right = gen_expr(rng, atoms, depth - 1);
    if rng.flag() {
        format!("({left} & {right})")
    } else {
        format!("({left} | {right})")
    }
}

fn random_case(rng: &mut XorShift) -> (serde_json::Value, Vec<Check>) {
    let names: Vec<String> = (0..3 + rng.below(3)).map(|i| format!("lane.s{i}")).collect();
    let mut send_signals = Vec::new();
    let mut gated: Vec<String> = Vec::new();
    for (index, name) in names.iter().enumerate() {
        if index == 0 || rng.flag() {
            send_signals.push(json!({"name": name}));
            continue;
        }
        let others: Vec<String> = names
            .iter()
            .enumerate()
            .filter(|(j, _)| *j != index)
            .map(|(_, n)| n.clone())
            .collect();
        let items: Vec<String> = (0..1 + rng.below(2))
            .map(|_| format!("buyer::{}", gen_expr(rng, &others, 2)))
            .collect();
        send_signals.push(json!({"name": name, "validWhen": items}));
        gated.push(name.clone());
    }
    let definition = json!({
        "apiVersion": "uvp/v0",
        "kind": "Zhixu",
        "metadata": {"name": "mc_diff_case"},
        "spec": {
            "platform": {"type": "cloud"},
            "nucleation": {"id": "core_org"},
            "stages": [{
                "name": "lane",
                "source": "buyer",
                "executor": {"supplierType": "organization", "supplierID": "supplier_demo"},
                "sendSignals": send_signals,
                "receiveSignals": {"H": format!("buyer::{}", names[0])}
            }]
        }
    });
    let mut checks = vec![check(
        "diff-bad-state",
        CheckKind::BadState,
        &format!("buyer::{}", gen_expr(rng, &names, 2)),
    )];
    if !gated.is_empty() && rng.flag() {
        checks.push(Check {
            id: "diff-admission-closed".to_string(),
            kind: CheckKind::AdmissionClosed,
            description: None,
            when: Some(format!("buyer::{}", gen_expr(rng, &names, 2))),
            predicate: None,
            intents: vec![format!("buyer::{}", gated[rng.below(gated.len())])],
        });
    }
    (definition, checks)
}

fn predicate_ready(vocab: &Vocabulary, raw: Option<&str>, state: &McState) -> bool {
    let Some(raw) = raw else { return true };
    let decoded = vocab
        .predicates
        .get(raw)
        .expect("predicate parsed during vocabulary build");
    model::eval_state(decoded, &state.signal_times(vocab), state.now).expect("eval succeeds")
        == EvalState::Ready
}

fn exhaustive_status(vocab: &Vocabulary, source: &Check) -> Status {
    let universe: Vec<usize> = (0..vocab.facts.len()).collect();
    match source.kind {
        CheckKind::BadState => {
            let predicate = source.predicate.as_deref().expect("validated");
            let when = source.when.as_deref();
            let graph = explore_universe(
                vocab,
                &universe,
                crate::DEFAULT_MAX_STATES,
                Some(&|state: &McState| {
                    Ok((predicate_ready(vocab, when, state)
                        && predicate_ready(vocab, Some(predicate), state))
                        .then_some(StopKind::Violation))
                }),
                None,
            )
            .expect("oracle stays inside the state cap");
            if graph.stopped.is_some() {
                Status::Fail
            } else {
                Status::Pass
            }
        }
        CheckKind::AdmissionClosed => {
            let when = source.when.as_deref().expect("validated");
            for name in &source.intents {
                let fact = vocab
                    .fact_index
                    .get(name)
                    .copied()
                    .expect("lint interned the intent");
                let &intent = vocab
                    .intent_fact
                    .get(&fact)
                    .expect("lint verified the admission");
                let graph = explore_universe(
                    vocab,
                    &universe,
                    crate::DEFAULT_MAX_STATES,
                    Some(&|state: &McState| {
                        if state.fact_epochs[fact].is_some()
                            || !predicate_ready(vocab, Some(when), state)
                        {
                            return Ok(None);
                        }
                        let admissible = model::intent_admissible(
                            vocab,
                            intent,
                            &state.signal_times(vocab),
                            state.now,
                        )
                        .expect("eval succeeds");
                        Ok(admissible.then_some(StopKind::Violation))
                    }),
                    None,
                )
                .expect("oracle stays inside the state cap");
                if graph.stopped.is_some() {
                    return Status::Fail;
                }
            }
            Status::Pass
        }
    }
}

// 差分钉：剪枝（落地阻断项整枝剪除）与闭包宇宙收窄只许换速度、不许换
// 结论——剪枝版结论与全事实宇宙、无剪枝的穷举版不一致即红（假 PASS 的
// oracle 撒谎面）。
#[test]
fn pruned_verdicts_match_exhaustive_exploration() {
    for seed in 1..=200u64 {
        let mut rng = XorShift(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
        let (definition, checks) = random_case(&mut rng);
        let vocab = Vocabulary::build(&definition, &checks).expect("generated case builds");
        let report = mc_check(McCheckRequest {
            definition: definition.clone(),
            checks: checks.clone(),
            max_states: None,
        })
        .expect("generated case runs");
        assert!(
            report.lint["ok"].as_bool().expect("lint ok flag"),
            "seed {seed}: generator emitted an unlintable case: {:?}",
            report.lint
        );
        for outcome in &report.checks {
            let source = checks
                .iter()
                .find(|probe| probe.id == outcome.id)
                .expect("outcome maps back to a check");
            let oracle = exhaustive_status(&vocab, source);
            assert_eq!(
                outcome.status,
                oracle,
                "seed {seed} check {}: pruned says {:?}, exhaustive says {:?}; definition={definition:?} checks={checks:?}",
                outcome.id, outcome.status, oracle
            );
        }
    }
}

// 回归钉：Or 臂内的否定（aux | ~x）与正负同位（gate & (aux | ~gate)）都
// 不是落地阻断项——修复前被当成阻断项整枝剪掉，admission_closed 与
// bad_state 在这三处全部假 PASS。两形态现必须与穷举结论一致且判 Fail。
#[test]
fn or_arm_negation_and_same_site_polarity_match_exhaustive() {
    let shape_a = json!({
        "apiVersion": "uvp/v0",
        "kind": "Zhixu",
        "metadata": {"name": "mc_shape_or_arm_neg"},
        "spec": {
            "platform": {"type": "cloud"},
            "nucleation": {"id": "core_org"},
            "stages": [{
                "name": "lane",
                "source": "buyer",
                "executor": {"supplierType": "organization", "supplierID": "supplier_demo"},
                "sendSignals": [
                    {"name": "x"},
                    {"name": "aux"},
                    {"name": "out", "validWhen": ["buyer::(lane.aux | ~lane.x)"]}
                ],
                "receiveSignals": {"H": "buyer::lane.x"}
            }]
        }
    });
    let shape_b = json!({
        "apiVersion": "uvp/v0",
        "kind": "Zhixu",
        "metadata": {"name": "mc_shape_same_site_polarity"},
        "spec": {
            "platform": {"type": "cloud"},
            "nucleation": {"id": "core_org"},
            "stages": [{
                "name": "lane",
                "source": "buyer",
                "executor": {"supplierType": "organization", "supplierID": "supplier_demo"},
                "sendSignals": [
                    {"name": "gate"},
                    {"name": "aux"},
                    {"name": "out", "validWhen": ["buyer::(lane.gate & (lane.aux | ~lane.gate))"]}
                ],
                "receiveSignals": {"H": "buyer::lane.gate"}
            }]
        }
    });
    let cases: Vec<(serde_json::Value, Vec<Check>)> = vec![
        (
            shape_a,
            vec![
                Check {
                    id: "a-closed".to_string(),
                    kind: CheckKind::AdmissionClosed,
                    description: None,
                    when: Some("buyer::lane.x".to_string()),
                    predicate: None,
                    intents: vec!["buyer::lane.out".to_string()],
                },
                check("a-bad", CheckKind::BadState, "buyer::lane.out"),
            ],
        ),
        (
            shape_b,
            vec![
                Check {
                    id: "b-closed".to_string(),
                    kind: CheckKind::AdmissionClosed,
                    description: None,
                    when: Some("buyer::lane.aux".to_string()),
                    predicate: None,
                    intents: vec!["buyer::lane.out".to_string()],
                },
                check("b-bad", CheckKind::BadState, "buyer::lane.out"),
            ],
        ),
    ];
    for (definition, checks) in cases {
        let vocab = Vocabulary::build(&definition, &checks).expect("shape builds");
        let report = mc_check(McCheckRequest {
            definition: definition.clone(),
            checks: checks.clone(),
            max_states: None,
        })
        .expect("shape runs");
        assert!(report.lint["ok"].as_bool().expect("lint ok flag"));
        for outcome in &report.checks {
            let source = checks
                .iter()
                .find(|probe| probe.id == outcome.id)
                .expect("outcome maps back to a check");
            assert_eq!(
                outcome.status,
                exhaustive_status(&vocab, source),
                "check {} must agree with exhaustive exploration",
                outcome.id
            );
            assert_eq!(
                outcome.status,
                Status::Fail,
                "check {} witnessed through the once-pruned arm must stay red",
                outcome.id
            );
        }
    }
}

#[test]
fn multi_source_admission_header_is_a_typed_error_not_a_panic() {
    let definition = json!({
        "apiVersion": "uvp/v0",
        "kind": "Zhixu",
        "metadata": {"name": "mc_multi_source_reject"},
        "spec": {
            "platform": {"type": "cloud"},
            "nucleation": {"id": "core_org"},
            "stages": [
                {
                    "name": "marks",
                    "source": "seller",
                    "executor": {"supplierType": "organization", "supplierID": "supplier_demo"},
                    "sendSignals": [{"name": "mark"}],
                    "receiveSignals": {"H": "seller::marks.mark"}
                },
                {
                    "name": "lane",
                    "source": "buyer",
                    "executor": {"supplierType": "organization", "supplierID": "supplier_demo"},
                    "sendSignals": [{"name": "out", "validWhen": ["seller::marks.mark"]}],
                    "receiveSignals": {"H": "buyer::lane.out"}
                }
            ]
        }
    });
    let error = mc_check(McCheckRequest {
        definition,
        checks: Vec::new(),
        max_states: None,
    })
    .expect_err("multi-source admission must be rejected, not panicked on");
    let message = error.to_string();
    assert!(
        message.contains("multi-source") && message.contains("lane.out"),
        "error must name the unsupported shape: {message}"
    );
}
