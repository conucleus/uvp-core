use super::*;
use serde_json::json;

#[test]
fn or_combination_keeps_earliest_anchor() {
    let left = EvalValue {
        value: true,
        wait: false,
        cancel: false,
        due_at: None,
        anchor_at: Some(100),
    };
    let right = EvalValue {
        value: true,
        wait: false,
        cancel: false,
        due_at: None,
        anchor_at: Some(5),
    };
    let combined = or_value(left, right);
    assert_eq!(combined.anchor_at, Some(5));
}

#[test]
fn or_ready_winner_keeps_own_anchor_without_waiting_branch() {
    let ready = EvalValue {
        value: true,
        wait: false,
        cancel: false,
        due_at: None,
        anchor_at: Some(1000),
    };
    let waiting = EvalValue {
        value: false,
        wait: true,
        cancel: false,
        due_at: Some(110),
        anchor_at: Some(10),
    };
    let combined = or_value(ready, waiting);
    assert!(combined.value);
    assert!(!combined.wait);
    assert_eq!(combined.anchor_at, Some(1000));
    let combined = or_value(waiting, ready);
    assert!(combined.value);
    assert_eq!(combined.anchor_at, Some(1000));
}

#[test]
fn rejects_unknown_instruction() {
    let instructions = vec![
        json!({"op": "SIGNAL", "signalKey": "0xaa"}),
        json!({"op": "SIGNAL", "signalKey": "0xbb"}),
        json!({"op": "FANIN", "arity": 2}),
    ];
    let error = evaluate_instructions(
        &OracleOrderState::default(),
        &instructions,
        "2026-04-27T00:00:00Z",
    )
    .unwrap_err();
    assert!(error
        .to_string()
        .contains("unsupported chain-mode instruction"));
}

#[test]
fn retired_fan_in_hook_plan_fails_loudly_in_replay() {
    let retired_op = concat!("MER", "GE");
    let events = vec![
        json!({
            "eventName": "PlanRegistered",
            "blockNumber": 1,
            "logIndex": 0,
            "transactionHash": "0x01",
            "plan": {
                "planId": "0x01",
                "zhixuId": "demo",
                "compiledHooks": [{
                    "hookId": "match.exchange#PAIR",
                    "stageId": "match.exchange",
                    "stageIdentifier": "match.exchange",
                    "hookName": "PAIR",
                    "orderTriggerKind": "mint",
                    "emitReady": true,
                    "instructions": [
                        {"op": "SIGNAL", "signalKey": "0x50"},
                        {"op": "SIGNAL", "signalKey": "0x51"},
                        {"op": retired_op, "arity": 2}
                    ]
                }],
                "dependencyIndex": { "0x50": ["match.exchange#PAIR"], "0x51": ["match.exchange#PAIR"] }
            }
        }),
        json!({
            "eventName": "OrderRegistered",
            "blockNumber": 2,
            "logIndex": 0,
            "transactionHash": "0x02",
            "planId": "0x01",
            "zhixuId": "demo",
            "orderId": "order-1",
            "registeredAt": "2026-04-27T00:00:00.000Z"
        }),
        json!({
            "eventName": "SignalSubmitted",
            "blockNumber": 3,
            "logIndex": 0,
            "transactionHash": "0x03",
            "planId": "0x01",
            "zhixuId": "demo",
            "orderId": "order-1",
            "sourceId": "0x30",
            "signalId": "0x40",
            "signalKey": "0x50",
            "senderId": "seller",
            "submittedAt": "2026-04-27T00:00:30.000Z"
        }),
    ];
    let error = replay_chain_events(
        events,
        &ReplayOptions {
            sort: None,
            strict: Some(true),
        },
    )
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("unsupported chain-mode instruction"),
        "unexpected error: {error}"
    );
}

#[test]
fn ready_status_changes_and_duplicates_are_absorbed() {
    let events = vec![
        json!({
            "eventName": "PlanRegistered",
            "blockNumber": 1,
            "logIndex": 0,
            "transactionHash": "0x01",
            "plan": {
                "planId": "0x01",
                "zhixuId": "demo",
                "compiledHooks": [{
                    "hookId": "flow.start#START",
                    "stageId": "flow.start",
                    "stageIdentifier": "flow.start",
                    "hookName": "START",
                    "orderTriggerKind": "mint",
                    "emitReady": true,
                    "instructions": [{"op": "SIGNAL", "signalKey": "0x50"}]
                }],
                "dependencyIndex": { "0x50": ["flow.start#START"] }
            }
        }),
        json!({
            "eventName": "OrderRegistered",
            "blockNumber": 2,
            "logIndex": 0,
            "transactionHash": "0x02",
            "planId": "0x01",
            "zhixuId": "demo",
            "orderId": "order-1",
            "registeredAt": "2026-04-27T00:00:00.000Z"
        }),
        json!({
            "eventName": "SignalSubmitted",
            "blockNumber": 3,
            "logIndex": 0,
            "transactionHash": "0x03",
            "planId": "0x01",
            "zhixuId": "demo",
            "orderId": "order-1",
            "sourceId": "0x30",
            "signalId": "0x40",
            "signalKey": "0x50",
            "senderId": "sender",
            "submittedAt": "2026-04-27T00:00:00.000Z"
        }),
        json!({
            "eventName": "HookStatusChanged",
            "blockNumber": 3,
            "logIndex": 1,
            "transactionHash": "0x03",
            "planId": "0x01",
            "zhixuId": "demo",
            "orderId": "order-1",
            "hookId": "flow.start#START",
            "status": "ready"
        }),
        json!({
            "eventName": "HookReady",
            "blockNumber": 3,
            "logIndex": 2,
            "transactionHash": "0x03",
            "planId": "0x01",
            "zhixuId": "demo",
            "orderId": "order-1",
            "hookId": "flow.start#START",
            "stageIdentifier": "flow.start",
            "hookName": "START"
        }),
        json!({
            "eventName": "HookStatusChanged",
            "blockNumber": 4,
            "logIndex": 0,
            "transactionHash": "0x04",
            "planId": "0x01",
            "zhixuId": "demo",
            "orderId": "order-1",
            "hookId": "flow.start#START",
            "status": "wait",
            "dueAt": "2026-04-27T00:00:05.000Z"
        }),
        json!({
            "eventName": "HookStatusChanged",
            "blockNumber": 5,
            "logIndex": 0,
            "transactionHash": "0x05",
            "planId": "0x01",
            "zhixuId": "demo",
            "orderId": "order-1",
            "hookId": "flow.start#START",
            "status": "wait",
            "dueAt": "2026-04-27T00:00:05.000Z"
        }),
    ];
    let result = replay_chain_events(
        events,
        &ReplayOptions {
            sort: None,
            strict: Some(false),
        },
    )
    .unwrap();
    let expected = result["expected"].as_array().unwrap();
    assert_eq!(expected.len(), 2, "expected: {expected:?}");
    assert_eq!(expected[0]["eventName"], "HookReady");
    assert_eq!(expected[1]["eventName"], "HookStatusChanged");
    assert_eq!(expected[1]["status"], "wait");
}

#[test]
fn ordinary_signals_do_not_advance_order_trigger_hooks() {
    let plan = json!({
        "planId": "0x01",
        "zhixuId": "demo",
        "compiledHooks": [
            {
                "hookId": "birth.one#ENTER",
                "stageId": "birth.one",
                "stageIdentifier": "birth.one",
                "hookName": "ENTER",
                "orderTriggerKind": "mint",
                "emitReady": true,
                "instructions": [{"op": "SIGNAL", "signalKey": "0x50"}]
            },
            {
                "hookId": "birth.two#ENTER",
                "stageId": "birth.two",
                "stageIdentifier": "birth.two",
                "hookName": "ENTER",
                "orderTriggerKind": "mint",
                "emitReady": true,
                "instructions": [{"op": "SIGNAL", "signalKey": "0x51"}]
            }
        ],
        "dependencyIndex": {
            "0x50": ["birth.one#ENTER"],
            "0x51": ["birth.two#ENTER"]
        }
    });
    let events = vec![
        json!({
            "eventName": "PlanRegistered",
            "blockNumber": 1,
            "logIndex": 0,
            "transactionHash": "0x01",
            "plan": plan
        }),
        json!({
            "eventName": "OrderRegistered",
            "blockNumber": 2,
            "logIndex": 0,
            "transactionHash": "0x02",
            "planId": "0x01",
            "zhixuId": "demo",
            "orderId": "order-x",
            "registeredAt": "2026-04-27T00:00:00.000Z"
        }),
        json!({
            "eventName": "SignalSubmitted",
            "blockNumber": 3,
            "logIndex": 0,
            "transactionHash": "0x03",
            "planId": "0x01",
            "zhixuId": "demo",
            "orderId": "order-x",
            "sourceId": "0x30",
            "signalId": "0x40",
            "signalKey": "0x50",
            "senderId": "relayer",
            "submittedAt": "2026-04-27T00:00:00.000Z"
        }),
        json!({
            "eventName": "HookReady",
            "blockNumber": 3,
            "logIndex": 1,
            "transactionHash": "0x03",
            "planId": "0x01",
            "zhixuId": "demo",
            "orderId": "order-x",
            "hookId": "birth.one#ENTER",
            "stageIdentifier": "birth.one",
            "hookName": "ENTER"
        }),
        json!({
            "eventName": "SignalSubmitted",
            "blockNumber": 4,
            "logIndex": 0,
            "transactionHash": "0x04",
            "planId": "0x01",
            "zhixuId": "demo",
            "orderId": "order-x",
            "sourceId": "0x31",
            "signalId": "0x41",
            "signalKey": "0x51",
            "senderId": "relayer",
            "submittedAt": "2026-04-27T00:00:20.000Z"
        }),
    ];
    let result = replay_chain_events(
        events,
        &ReplayOptions {
            sort: None,
            strict: Some(true),
        },
    )
    .unwrap();
    assert_eq!(
        result["mismatches"].as_array().map(Vec::len),
        Some(0),
        "ordinary signals must not advance order-trigger hooks: {}",
        result["mismatches"]
    );
    let order = &result["state"]["orders"]["0x01::order-x"];
    assert_eq!(order["hookStatuses"]["birth.two#ENTER"], json!(null));
    assert_eq!(order["materializedStages"]["birth.two"], json!(null));
    assert_eq!(order["materializedStages"]["birth.one"], true);
}

#[test]
fn wait_reemission_on_due_at_only_change_pairs_cleanly() {
    let plan = json!({
        "planId": "0x01",
        "zhixuId": "demo",
        "compiledHooks": [{
            "hookId": "flow.pay#TIMEOUT",
            "stageId": "flow.pay",
            "stageIdentifier": "flow.pay",
            "hookName": "TIMEOUT",
            "orderTriggerKind": "none",
            "emitReady": true,
            "instructions": [
                {"op": "SIGNAL", "signalKey": "0x50"},
                {"op": "DELAY", "delaySeconds": 30},
                {"op": "SIGNAL", "signalKey": "0x51"},
                {"op": "DELAY", "delaySeconds": 5},
                {"op": "OR", "arity": 2}
            ]
        }],
        "dependencyIndex": {
            "0x50": ["flow.pay#TIMEOUT"],
            "0x51": ["flow.pay#TIMEOUT"]
        }
    });
    let events = vec![
        json!({
            "eventName": "PlanRegistered",
            "blockNumber": 1,
            "logIndex": 0,
            "transactionHash": "0x01",
            "plan": plan
        }),
        json!({
            "eventName": "OrderRegistered",
            "blockNumber": 2,
            "logIndex": 0,
            "transactionHash": "0x02",
            "planId": "0x01",
            "zhixuId": "demo",
            "orderId": "order-w",
            "registeredAt": "2026-04-27T00:00:00.000Z"
        }),
        json!({
            "eventName": "StageMaterialized",
            "blockNumber": 3,
            "logIndex": 0,
            "transactionHash": "0x03",
            "planId": "0x01",
            "orderId": "order-w",
            "stageId": "flow.pay"
        }),
        json!({
            "eventName": "SignalSubmitted",
            "blockNumber": 4,
            "logIndex": 0,
            "transactionHash": "0x04",
            "planId": "0x01",
            "zhixuId": "demo",
            "orderId": "order-w",
            "sourceId": "0x30",
            "signalId": "0x40",
            "signalKey": "0x50",
            "senderId": "executor",
            "submittedAt": "2026-04-27T00:00:00.000Z"
        }),
        json!({
            "eventName": "HookStatusChanged",
            "blockNumber": 4,
            "logIndex": 1,
            "transactionHash": "0x04",
            "planId": "0x01",
            "zhixuId": "demo",
            "orderId": "order-w",
            "hookId": "flow.pay#TIMEOUT",
            "status": "wait",
            "dueAt": "2026-04-27T00:00:30.000Z"
        }),
        json!({
            "eventName": "SignalSubmitted",
            "blockNumber": 5,
            "logIndex": 0,
            "transactionHash": "0x05",
            "planId": "0x01",
            "zhixuId": "demo",
            "orderId": "order-w",
            "sourceId": "0x31",
            "signalId": "0x41",
            "signalKey": "0x51",
            "senderId": "executor",
            "submittedAt": "2026-04-27T00:00:10.000Z"
        }),
        json!({
            "eventName": "HookStatusChanged",
            "blockNumber": 5,
            "logIndex": 1,
            "transactionHash": "0x05",
            "planId": "0x01",
            "zhixuId": "demo",
            "orderId": "order-w",
            "hookId": "flow.pay#TIMEOUT",
            "status": "wait",
            "dueAt": "2026-04-27T00:00:15.000Z"
        }),
    ];
    let result = replay_chain_events(
        events,
        &ReplayOptions {
            sort: None,
            strict: Some(true),
        },
    )
    .unwrap();
    assert_eq!(
        result["mismatches"].as_array().map(Vec::len),
        Some(0),
        "dueAt-only wait re-emissions must pair cleanly: {}",
        result["mismatches"]
    );
    assert_eq!(result["expected"].as_array().map(Vec::len), Some(2));
    assert_eq!(result["observed"].as_array().map(Vec::len), Some(2));
}

#[test]
fn poke_before_due_or_not_waiting_is_skipped() {
    let events = vec![
        json!({
            "eventName": "PlanRegistered",
            "blockNumber": 1,
            "logIndex": 0,
            "transactionHash": "0x01",
            "plan": {
                "planId": "0x01",
                "zhixuId": "demo",
                "compiledHooks": [{
                    "hookId": "flow.pay#TIMEOUT",
                    "stageId": "flow.pay",
                    "stageIdentifier": "flow.pay",
                    "hookName": "TIMEOUT",
                    "orderTriggerKind": "none",
                    "emitReady": true,
                    "instructions": [
                        {"op": "SIGNAL", "signalKey": "0x50"},
                        {"op": "DELAY", "delaySeconds": 10}
                    ]
                }],
                "dependencyIndex": { "0x50": ["flow.pay#TIMEOUT"] }
            }
        }),
        json!({
            "eventName": "OrderRegistered",
            "blockNumber": 2,
            "logIndex": 0,
            "transactionHash": "0x02",
            "planId": "0x01",
            "zhixuId": "demo",
            "orderId": "order-1",
            "registeredAt": "2026-04-27T00:00:00.000Z"
        }),
        json!({
            "eventName": "StageMaterialized",
            "blockNumber": 3,
            "logIndex": 0,
            "transactionHash": "0x03",
            "planId": "0x01",
            "orderId": "order-1",
            "stageId": "flow.pay"
        }),
        json!({
            "eventName": "SignalSubmitted",
            "blockNumber": 4,
            "logIndex": 0,
            "transactionHash": "0x04",
            "planId": "0x01",
            "zhixuId": "demo",
            "orderId": "order-1",
            "sourceId": "0x30",
            "signalId": "0x40",
            "signalKey": "0x50",
            "senderId": "sender",
            "submittedAt": "2026-04-27T00:00:00.000Z"
        }),
        json!({
            "eventName": "HookStatusChanged",
            "blockNumber": 4,
            "logIndex": 1,
            "transactionHash": "0x04",
            "planId": "0x01",
            "zhixuId": "demo",
            "orderId": "order-1",
            "hookId": "flow.pay#TIMEOUT",
            "status": "wait",
            "dueAt": "2026-04-27T00:00:10.000Z"
        }),
        json!({
            "eventName": "TimerPoked",
            "blockNumber": 5,
            "logIndex": 0,
            "transactionHash": "0x05",
            "planId": "0x01",
            "zhixuId": "demo",
            "orderId": "order-1",
            "hookId": "flow.pay#TIMEOUT",
            "pokedAt": "2026-04-27T00:00:05.000Z"
        }),
        json!({
            "eventName": "TimerPoked",
            "blockNumber": 6,
            "logIndex": 0,
            "transactionHash": "0x06",
            "planId": "0x01",
            "zhixuId": "demo",
            "orderId": "order-1",
            "hookId": "flow.pay#TIMEOUT",
            "pokedAt": "2026-04-27T00:00:11.000Z"
        }),
        json!({
            "eventName": "HookStatusChanged",
            "blockNumber": 6,
            "logIndex": 1,
            "transactionHash": "0x06",
            "planId": "0x01",
            "zhixuId": "demo",
            "orderId": "order-1",
            "hookId": "flow.pay#TIMEOUT",
            "status": "ready"
        }),
        json!({
            "eventName": "HookReady",
            "blockNumber": 6,
            "logIndex": 2,
            "transactionHash": "0x06",
            "planId": "0x01",
            "zhixuId": "demo",
            "orderId": "order-1",
            "hookId": "flow.pay#TIMEOUT",
            "stageIdentifier": "flow.pay",
            "hookName": "TIMEOUT"
        }),
    ];
    let result = replay_chain_events(
        events,
        &ReplayOptions {
            sort: None,
            strict: Some(true),
        },
    )
    .unwrap();
    let observed = result["observed"].as_array().unwrap();
    assert_eq!(observed.len(), 2, "observed: {observed:?}");
    assert_eq!(observed[0]["eventName"], "HookStatusChanged");
    assert_eq!(observed[0]["status"], "wait");
    assert_eq!(observed[1]["eventName"], "HookReady");
    assert_eq!(result["mismatches"].as_array().map(Vec::len), Some(0));
}

#[test]
fn rejects_arity_exceeding_stack() {
    let instructions = vec![
        json!({"op": "SIGNAL", "signalKey": "0x50"}),
        json!({"op": "AND", "arity": 2}),
    ];
    let error = evaluate_instructions(
        &OracleOrderState::default(),
        &instructions,
        "2026-04-27T00:00:00Z",
    )
    .unwrap_err();
    assert!(error.to_string().contains("requires 2 operands"));
}

#[test]
fn rejects_leftover_stack_values() {
    let instructions = vec![
        json!({"op": "SIGNAL", "signalKey": "0x50"}),
        json!({"op": "SIGNAL", "signalKey": "0x51"}),
    ];
    let error = evaluate_instructions(
        &OracleOrderState::default(),
        &instructions,
        "2026-04-27T00:00:00Z",
    )
    .unwrap_err();
    assert!(error.to_string().contains("exactly one result value"));
}

#[test]
fn rejects_overflowing_delay_computation() {
    let anchored = EvalValue {
        value: true,
        wait: false,
        cancel: false,
        due_at: None,
        anchor_at: Some(i64::MAX),
    };
    let error = delay_value(anchored, 30 * 24 * 60 * 60, "2026-04-27T00:00:00Z").unwrap_err();
    assert!(error
        .to_string()
        .contains("overflows the replay timestamp range"));
}

#[test]
fn rejects_invalid_submitted_at() {
    let mut order = OracleOrderState::default();
    order.signals.insert(
        "0x50".to_string(),
        json!({"submittedAt": "not-a-timestamp"}),
    );
    let error = signal_value(&order, "0x50").unwrap_err();
    assert!(error.to_string().contains("invalid chain oracle timestamp"));
}

#[test]
fn replays_ready_hook() {
    let events = vec![
        json!({
            "eventName": "PlanRegistered",
            "blockNumber": 1,
            "logIndex": 0,
            "transactionHash": "0x01",
            "plan": {
                "planId": "0x01",
                "zhixuId": "demo",
                "compiledHooks": [{
                    "hookId": "0x10",
                    "stageId": "0x20",
                    "stageIdentifier": "flow.start",
                    "hookName": "START",
                    "orderTriggerKind": "mint",
                    "emitReady": true,
                    "instructions": [{
                        "op": "SIGNAL",
                        "sourceId": "0x30",
                        "signalId": "0x40",
                        "signalKey": "0x50"
                    }]
                }],
                "dependencyIndex": { "0x50": ["0x10"] }
            }
        }),
        json!({
            "eventName": "OrderRegistered",
            "blockNumber": 2,
            "logIndex": 0,
            "transactionHash": "0x02",
            "planId": "0x01",
            "zhixuId": "demo",
            "orderId": "order-1",
            "registeredAt": "2026-04-27T00:00:00.000Z"
        }),
        json!({
            "eventName": "SignalSubmitted",
            "blockNumber": 3,
            "logIndex": 0,
            "transactionHash": "0x03",
            "planId": "0x01",
            "zhixuId": "demo",
            "orderId": "order-1",
            "sourceId": "0x30",
            "signalId": "0x40",
            "signalKey": "0x50",
            "senderId": "sender",
            "submittedAt": "2026-04-27T00:00:00.000Z"
        }),
    ];
    let result = replay_chain_events(
        events,
        &ReplayOptions {
            sort: None,
            strict: Some(false),
        },
    )
    .unwrap();
    assert_eq!(
        result["observed"][0],
        json!({
            "eventName": "HookReady",
            "planId": "0x01",
            "zhixuId": "demo",
            "orderId": "order-1",
            "hookId": "0x10",
            "stageIdentifier": "flow.start",
            "hookName": "START"
        })
    );
}

#[test]
fn emit_ready_is_independent_from_order_materialization() {
    let mut order = OracleOrderState {
        zhixu_id: "demo".to_string(),
        order_id: "order-1".to_string(),
        ..OracleOrderState::default()
    };
    order.signals.insert(
        "0x50".to_string(),
        json!({"submittedAt": "2026-04-27T00:00:00.000Z"}),
    );

    let silent_trigger = json!({
        "hookId": "flow.start#SILENT",
        "stageId": "flow.start",
        "stageIdentifier": "flow.start",
        "hookName": "SILENT",
        "orderTriggerKind": "mint",
        "emitReady": false,
        "instructions": [{"op": "SIGNAL", "signalKey": "0x50"}]
    });
    let trigger_observations =
        evaluate_hook(&mut order, &silent_trigger, "2026-04-27T00:00:00.000Z")
            .expect("silent trigger should evaluate");
    assert!(trigger_observations.is_empty());
    assert!(order.materialized_stages["flow.start"]);
    assert_eq!(order.hook_statuses["flow.start#SILENT"].status, "ready");
    assert!(!order.hook_statuses["flow.start#SILENT"].ready_emitted);

    let ordinary_hook = json!({
        "hookId": "flow.start#OBSERVE",
        "stageId": "flow.start",
        "stageIdentifier": "flow.start",
        "hookName": "OBSERVE",
        "orderTriggerKind": "none",
        "emitReady": true,
        "instructions": [{"op": "SIGNAL", "signalKey": "0x50"}]
    });
    let observations = evaluate_hook(&mut order, &ordinary_hook, "2026-04-27T00:00:00.000Z")
        .expect("materialized ordinary hook should evaluate");
    assert_eq!(observations.len(), 1);
    assert_eq!(observations[0]["eventName"], "HookReady");
    assert_eq!(observations[0]["hookId"], "flow.start#OBSERVE");
}

#[test]
fn stage_materialized_event_backfills_materialization() {
    let events = vec![
        json!({
            "eventName": "PlanRegistered",
            "blockNumber": 1,
            "logIndex": 0,
            "transactionHash": "0x01",
            "plan": {
                "planId": "0x01",
                "zhixuId": "demo",
                "compiledHooks": [{
                    "hookId": "flow.exec#WATCH",
                    "stageId": "flow.exec",
                    "stageIdentifier": "flow.exec",
                    "hookName": "WATCH",
                    "orderTriggerKind": "none",
                    "emitReady": false,
                    "instructions": [{"op": "SIGNAL", "signalKey": "0x50"}]
                }],
                "dependencyIndex": { "0x50": ["flow.exec#WATCH"] }
            }
        }),
        json!({
            "eventName": "OrderRegistered",
            "blockNumber": 2,
            "logIndex": 0,
            "transactionHash": "0x02",
            "planId": "0x01",
            "zhixuId": "demo",
            "orderId": "order-1",
            "registeredAt": "2026-04-27T00:00:00.000Z"
        }),
        json!({
            "eventName": "StageMaterialized",
            "blockNumber": 3,
            "logIndex": 0,
            "transactionHash": "0x03",
            "planId": "0x01",
            "orderId": "order-1",
            "stageId": "flow.exec",
            "triggerHookId": "flow.exec#BIRTH",
            "sourceId": "0x30",
            "signalId": "0x40"
        }),
        json!({
            "eventName": "SignalSubmitted",
            "blockNumber": 4,
            "logIndex": 0,
            "transactionHash": "0x04",
            "planId": "0x01",
            "zhixuId": "demo",
            "orderId": "order-1",
            "sourceId": "0x30",
            "signalId": "0x40",
            "signalKey": "0x50",
            "senderId": "sender",
            "submittedAt": "2026-04-27T00:00:00.000Z"
        }),
    ];
    let result = replay_chain_events(
        events,
        &ReplayOptions {
            sort: None,
            strict: Some(false),
        },
    )
    .unwrap();
    assert_eq!(
        result["state"]["orders"]["0x01::order-1"]["hookStatuses"]["flow.exec#WATCH"]["status"],
        "ready",
        "watcher must evaluate after the chain-emitted StageMaterialized"
    );
}

#[test]
fn hooks_missing_required_v2_fields_are_rejected() {
    let mut order = OracleOrderState {
        zhixu_id: "demo".to_string(),
        order_id: "order-1".to_string(),
        ..OracleOrderState::default()
    };
    order.signals.insert(
        "0x50".to_string(),
        json!({"submittedAt": "2026-04-27T00:00:00.000Z"}),
    );
    let hook_missing_kind = json!({
        "hookId": "flow.start#BARE",
        "stageId": "flow.start",
        "stageIdentifier": "flow.start",
        "hookName": "BARE",
        "emitReady": true,
        "isTrigger": true,
        "instructions": [{"op": "SIGNAL", "signalKey": "0x50"}]
    });
    let error = evaluate_hook(&mut order, &hook_missing_kind, "2026-04-27T00:00:00.000Z")
        .expect_err("hook missing orderTriggerKind must be rejected");
    assert!(
        error.to_string().contains("orderTriggerKind"),
        "unexpected error: {error}"
    );

    let hook_missing_emit_ready = json!({
        "hookId": "flow.start#BARE",
        "stageId": "flow.start",
        "stageIdentifier": "flow.start",
        "hookName": "BARE",
        "orderTriggerKind": "mint",
        "isTrigger": true,
        "instructions": [{"op": "SIGNAL", "signalKey": "0x50"}]
    });
    let error = evaluate_hook(
        &mut order,
        &hook_missing_emit_ready,
        "2026-04-27T00:00:00.000Z",
    )
    .expect_err("hook missing emitReady must be rejected");
    assert!(
        error.to_string().contains("emitReady must be a boolean"),
        "unexpected error: {error}"
    );
}

#[test]
fn replay_scopes_same_order_id_by_plan() {
    let mut events = Vec::new();
    for (index, plan_id) in ["plan-a", "plan-b"].iter().enumerate() {
        let block = (index * 3 + 1) as i64;
        events.push(json!({
            "eventName": "PlanRegistered",
            "blockNumber": block,
            "logIndex": 0,
            "transactionHash": format!("0xplan{index}"),
            "plan": {
                "planId": plan_id,
                "zhixuId": "same-zhixu",
                "compiledHooks": [{
                    "hookId": "flow.start#READY",
                    "stageId": "flow.start",
                    "stageIdentifier": "flow.start",
                    "hookName": "READY",
                    "orderTriggerKind": "mint",
                    "emitReady": true,
                    "instructions": [{"op": "SIGNAL", "signalKey": "0x50"}]
                }],
                "dependencyIndex": {"0x50": ["flow.start#READY"]}
            }
        }));
        events.push(json!({
            "eventName": "OrderRegistered",
            "blockNumber": block + 1,
            "logIndex": 0,
            "transactionHash": format!("0xorder{index}"),
            "planId": plan_id,
            "zhixuId": "same-zhixu",
            "orderId": "reused-order",
            "registeredAt": "2026-04-27T00:00:00.000Z"
        }));
        events.push(json!({
            "eventName": "SignalSubmitted",
            "blockNumber": block + 2,
            "logIndex": 0,
            "transactionHash": format!("0xsignal{index}"),
            "planId": plan_id,
            "zhixuId": "same-zhixu",
            "orderId": "reused-order",
            "sourceId": "0x30",
            "signalId": "0x40",
            "signalKey": "0x50",
            "senderId": format!("sender-{index}"),
            "submittedAt": "2026-04-27T00:00:00.000Z"
        }));
    }

    let result = replay_chain_events(
        events,
        &ReplayOptions {
            sort: Some(true),
            strict: Some(false),
        },
    )
    .expect("plan-scoped order addresses should replay");
    let orders = result["state"]["orders"]
        .as_object()
        .expect("state orders should be an object");
    assert_eq!(orders.len(), 2);
    assert!(orders.contains_key("plan-a::reused-order"));
    assert!(orders.contains_key("plan-b::reused-order"));
    assert_eq!(result["observed"].as_array().unwrap().len(), 2);
}

#[test]
fn replay_options_rejects_unknown_fields() {
    let output = replay_json(r#"{"events": [], "options": {"strick": false}}"#);
    let envelope: Value = serde_json::from_str(&output).expect("envelope");
    assert_eq!(envelope["ok"], json!(false), "{output}");
    assert!(
        envelope["diagnostics"][0]["message"]
            .as_str()
            .unwrap()
            .contains("unknown field"),
        "{output}"
    );
}

fn single_hook_plan(hook_id: &str, instructions: Value) -> Value {
    json!({
        "planId": "0x01",
        "zhixuId": "demo",
        "compiledHooks": [{
            "hookId": hook_id,
            "stageId": "flow.start",
            "stageIdentifier": "flow.start",
            "hookName": "START",
            "orderTriggerKind": "mint",
            "emitReady": true,
            "instructions": instructions,
        }],
        "dependencyIndex": { "0x50": [hook_id] }
    })
}

#[test]
fn wait_due_at_compares_by_instant_not_rendering() {
    let plan = single_hook_plan(
        "flow.start#TIMEOUT",
        json!([
            { "op": "SIGNAL", "signalKey": "0x50" },
            { "op": "DELAY", "delaySeconds": 10 }
        ]),
    );
    let plan = {
        let mut plan = plan;
        plan["compiledHooks"][0]["orderTriggerKind"] = json!("none");
        plan
    };
    let base_events = vec![
        json!({
            "eventName": "PlanRegistered",
            "blockNumber": 1,
            "logIndex": 0,
            "transactionHash": "0x01",
            "plan": plan
        }),
        json!({
            "eventName": "OrderRegistered",
            "blockNumber": 2,
            "logIndex": 0,
            "transactionHash": "0x02",
            "planId": "0x01",
            "zhixuId": "demo",
            "orderId": "order-1",
            "registeredAt": "2026-04-27T00:00:00.000Z"
        }),
        json!({
            "eventName": "StageMaterialized",
            "blockNumber": 3,
            "logIndex": 0,
            "transactionHash": "0x03",
            "planId": "0x01",
            "orderId": "order-1",
            "stageId": "flow.start"
        }),
        json!({
            "eventName": "SignalSubmitted",
            "blockNumber": 4,
            "logIndex": 0,
            "transactionHash": "0x04",
            "planId": "0x01",
            "zhixuId": "demo",
            "orderId": "order-1",
            "sourceId": "0x30",
            "signalId": "0x40",
            "signalKey": "0x50",
            "senderId": "sender",
            "submittedAt": "2026-04-27T00:00:00.000Z"
        }),
    ];
    let mut events = base_events.clone();
    events.push(json!({
        "eventName": "HookStatusChanged",
        "blockNumber": 4,
        "logIndex": 1,
        "transactionHash": "0x04",
        "planId": "0x01",
        "zhixuId": "demo",
        "orderId": "order-1",
        "hookId": "flow.start#TIMEOUT",
        "status": "wait",
        "dueAt": "2026-04-27T00:00:10Z"
    }));
    let result = replay_chain_events(
        events,
        &ReplayOptions {
            sort: None,
            strict: Some(false),
        },
    )
    .unwrap();
    assert_eq!(
        result["mismatches"].as_array().map(Vec::len),
        Some(0),
        "same instant under a different rendering must not mismatch: {}",
        result["mismatches"]
    );

    let mut events = base_events.clone();
    for due_at in ["2026-04-27T00:00:10Z", "2026-04-27T00:00:10.000Z"] {
        events.push(json!({
            "eventName": "HookStatusChanged",
            "blockNumber": 5,
            "logIndex": 0,
            "transactionHash": "0x05",
            "planId": "0x01",
            "zhixuId": "demo",
            "orderId": "order-1",
            "hookId": "flow.start#TIMEOUT",
            "status": "wait",
            "dueAt": due_at
        }));
    }
    let result = replay_chain_events(
        events,
        &ReplayOptions {
            sort: None,
            strict: Some(false),
        },
    )
    .unwrap();
    assert_eq!(result["expected"].as_array().map(Vec::len), Some(1));

    let mut events = base_events;
    events.push(json!({
        "eventName": "HookStatusChanged",
        "blockNumber": 4,
        "logIndex": 1,
        "transactionHash": "0x04",
        "planId": "0x01",
        "zhixuId": "demo",
        "orderId": "order-1",
        "hookId": "flow.start#TIMEOUT",
        "status": "wait",
        "dueAt": "2026-04-27T00:00:11Z"
    }));
    let result = replay_chain_events(
        events,
        &ReplayOptions {
            sort: None,
            strict: Some(false),
        },
    )
    .unwrap();
    let mismatches = result["mismatches"].as_array().unwrap();
    assert_eq!(mismatches.len(), 1, "{mismatches:?}");
    assert_eq!(mismatches[0]["reason"], json!("semantic-mismatch"));
}

#[test]
fn observations_pair_per_hook_key_not_global_index() {
    let plan = single_hook_plan(
        "flow.start#BIRTH",
        json!([{ "op": "SIGNAL", "signalKey": "0x50" }]),
    );
    let mut events = vec![json!({
        "eventName": "PlanRegistered",
        "blockNumber": 1,
        "logIndex": 0,
        "transactionHash": "0x01",
        "plan": plan
    })];
    for (index, order_id) in ["order-1", "order-2"].iter().enumerate() {
        events.push(json!({
            "eventName": "OrderRegistered",
            "blockNumber": 2 + index as i64,
            "logIndex": 0,
            "transactionHash": format!("0x0{index}"),
            "planId": "0x01",
            "zhixuId": "demo",
            "orderId": order_id,
            "registeredAt": "2026-04-27T00:00:00.000Z"
        }));
        events.push(json!({
            "eventName": "SignalSubmitted",
            "blockNumber": 4 + index as i64,
            "logIndex": 0,
            "transactionHash": format!("0x1{index}"),
            "planId": "0x01",
            "zhixuId": "demo",
            "orderId": order_id,
            "sourceId": "0x30",
            "signalId": "0x40",
            "signalKey": "0x50",
            "senderId": "sender",
            "submittedAt": "2026-04-27T00:00:00.000Z"
        }));
    }
    for (index, order_id) in [(0, "order-2"), (1, "order-1")] {
        events.push(json!({
            "eventName": "HookReady",
            "blockNumber": 6 + index as i64,
            "logIndex": 0,
            "transactionHash": format!("0x2{index}"),
            "planId": "0x01",
            "zhixuId": "demo",
            "orderId": order_id,
            "hookId": "flow.start#BIRTH",
            "stageIdentifier": "flow.start",
            "hookName": "START"
        }));
    }
    let result = replay_chain_events(
        events,
        &ReplayOptions {
            sort: None,
            strict: Some(false),
        },
    )
    .unwrap();
    assert_eq!(
        result["mismatches"].as_array().map(Vec::len),
        Some(0),
        "{}",
        result["mismatches"]
    );
    assert_eq!(result["expected"].as_array().map(Vec::len), Some(2));
    assert_eq!(result["observed"].as_array().map(Vec::len), Some(2));
}

#[test]
fn case_distinct_hook_ids_stay_separate() {
    let upper = json!({
        "eventName": "HookReady",
        "planId": "0x01",
        "orderId": "order-1",
        "hookId": "task.Main#GO",
        "stageIdentifier": "task.Main",
        "hookName": "GO"
    });
    let lower = json!({
        "eventName": "HookReady",
        "planId": "0x01",
        "orderId": "order-1",
        "hookId": "task.main#GO",
        "stageIdentifier": "task.main",
        "hookName": "GO"
    });
    assert_ne!(hook_observation_key(&upper), hook_observation_key(&lower));
    assert!(!same_hook_observation(&upper, &lower));
}

#[test]
fn init_status_changes_are_trimmed() {
    let plan = single_hook_plan(
        "flow.start#BIRTH",
        json!([{ "op": "SIGNAL", "signalKey": "0x50" }]),
    );
    let events = vec![
        json!({
            "eventName": "PlanRegistered",
            "blockNumber": 1,
            "logIndex": 0,
            "transactionHash": "0x01",
            "plan": plan
        }),
        json!({
            "eventName": "OrderRegistered",
            "blockNumber": 2,
            "logIndex": 0,
            "transactionHash": "0x02",
            "planId": "0x01",
            "zhixuId": "demo",
            "orderId": "order-1",
            "registeredAt": "2026-04-27T00:00:00.000Z"
        }),
        json!({
            "eventName": "HookStatusChanged",
            "blockNumber": 3,
            "logIndex": 0,
            "transactionHash": "0x03",
            "planId": "0x01",
            "zhixuId": "demo",
            "orderId": "order-1",
            "hookId": "flow.start#BIRTH",
            "status": "init"
        }),
        json!({
            "eventName": "SignalSubmitted",
            "blockNumber": 4,
            "logIndex": 0,
            "transactionHash": "0x04",
            "planId": "0x01",
            "zhixuId": "demo",
            "orderId": "order-1",
            "sourceId": "0x30",
            "signalId": "0x40",
            "signalKey": "0x50",
            "senderId": "sender",
            "submittedAt": "2026-04-27T00:00:00.000Z"
        }),
        json!({
            "eventName": "HookStatusChanged",
            "blockNumber": 4,
            "logIndex": 1,
            "transactionHash": "0x04",
            "planId": "0x01",
            "zhixuId": "demo",
            "orderId": "order-1",
            "hookId": "flow.start#BIRTH",
            "status": "init"
        }),
        json!({
            "eventName": "HookStatusChanged",
            "blockNumber": 4,
            "logIndex": 2,
            "transactionHash": "0x04",
            "planId": "0x01",
            "zhixuId": "demo",
            "orderId": "order-1",
            "hookId": "flow.start#BIRTH",
            "status": "ready"
        }),
        json!({
            "eventName": "HookReady",
            "blockNumber": 4,
            "logIndex": 3,
            "transactionHash": "0x04",
            "planId": "0x01",
            "zhixuId": "demo",
            "orderId": "order-1",
            "hookId": "flow.start#BIRTH",
            "stageIdentifier": "flow.start",
            "hookName": "START"
        }),
    ];
    let result = replay_chain_events(
        events,
        &ReplayOptions {
            sort: None,
            strict: Some(true),
        },
    )
    .unwrap();
    assert_eq!(
        result["mismatches"].as_array().map(Vec::len),
        Some(0),
        "{}",
        result["mismatches"]
    );
    let expected = result["expected"].as_array().unwrap();
    assert_eq!(expected.len(), 1, "init/ready 状态变更被裁剪: {expected:?}");
    assert_eq!(expected[0]["eventName"], json!("HookReady"));
}

#[test]
fn per_key_hook_ids_evaluate_triggers_before_watchers_in_stable_order() {
    let plan = json!({
        "planId": "0x01",
        "zhixuId": "demo",
        "compiledHooks": [
            { "hookId": "w.watch#W1", "stageId": "w.one", "stageIdentifier": "w.one",
              "hookName": "W1", "orderTriggerKind": "none", "emitReady": true,
              "instructions": [{ "op": "SIGNAL", "signalKey": "0x50" }] },
            { "hookId": "b.birth#T", "stageId": "b.birth", "stageIdentifier": "b.birth",
              "hookName": "T", "orderTriggerKind": "mint", "emitReady": true,
              "instructions": [{ "op": "SIGNAL", "signalKey": "0x50" }] },
            { "hookId": "w.watch#W2", "stageId": "w.two", "stageIdentifier": "w.two",
              "hookName": "W2", "orderTriggerKind": "none", "emitReady": true,
              "instructions": [{ "op": "SIGNAL", "signalKey": "0x50" }] }
        ],
        "dependencyIndex": { "0x50": ["w.watch#W1", "b.birth#T", "w.watch#W2"] }
    });
    let events = vec![
        json!({
            "eventName": "PlanRegistered",
            "blockNumber": 1,
            "logIndex": 0,
            "transactionHash": "0x01",
            "plan": plan
        }),
        json!({
            "eventName": "OrderRegistered",
            "blockNumber": 2,
            "logIndex": 0,
            "transactionHash": "0x02",
            "planId": "0x01",
            "zhixuId": "demo",
            "orderId": "order-1",
            "registeredAt": "2026-04-27T00:00:00.000Z"
        }),
        json!({
            "eventName": "SignalSubmitted",
            "blockNumber": 3,
            "logIndex": 0,
            "transactionHash": "0x03",
            "planId": "0x01",
            "zhixuId": "demo",
            "orderId": "order-1",
            "sourceId": "0x30",
            "signalId": "0x40",
            "signalKey": "0x50",
            "senderId": "sender",
            "submittedAt": "2026-04-27T00:00:00.000Z"
        }),
    ];
    let result = replay_chain_events(
        events,
        &ReplayOptions {
            sort: None,
            strict: Some(false),
        },
    )
    .unwrap();
    let observed = result["observed"].as_array().unwrap();
    let order: Vec<&str> = observed
        .iter()
        .map(|item| item["hookId"].as_str().unwrap())
        .collect();
    assert_eq!(
        order,
        vec!["b.birth#T", "w.watch#W1", "w.watch#W2"],
        "trigger-first stable partition over dependencyIndex order: {observed:?}"
    );
}

#[test]
fn order_link_birth_requires_structural_hook_fields() {
    let events = vec![
        json!({
            "eventName": "PlanRegistered",
            "blockNumber": 1,
            "logIndex": 0,
            "transactionHash": "0x01",
            "plan": {
                "planId": "0x01",
                "zhixuId": "demo",
                "compiledHooks": [{
                    "hookId": "linked.entry#BIRTH",
                    "stageIdentifier": "linked.entry",
                    "hookName": "BIRTH",
                    "orderTriggerKind": "mint",
                    "emitReady": true,
                    "instructions": [{"op": "SIGNAL", "signalKey": "0x50"}]
                }],
                "dependencyIndex": { "0x50": ["linked.entry#BIRTH"] }
            }
        }),
        json!({
            "eventName": "OrderRegistered",
            "blockNumber": 2,
            "logIndex": 0,
            "transactionHash": "0x02",
            "planId": "0x01",
            "zhixuId": "demo",
            "orderId": "order-7",
            "registeredAt": "2026-04-27T00:00:00.000Z"
        }),
        json!({
            "eventName": "HookReady",
            "blockNumber": 3,
            "logIndex": 0,
            "transactionHash": "0x03",
            "planId": "0x01",
            "zhixuId": "demo",
            "orderId": "order-7",
            "hookId": "linked.entry#BIRTH",
            "stageIdentifier": "linked.entry",
            "hookName": "BIRTH"
        }),
    ];
    let error = replay_chain_events(
        events.clone(),
        &ReplayOptions {
            sort: None,
            strict: Some(true),
        },
    )
    .unwrap_err();
    assert!(
        error.to_string().contains("stageId must be a string"),
        "missing stageId must fail loudly, got: {error}"
    );

    let mut events = events;
    events[0]["plan"]["compiledHooks"][0]
        .as_object_mut()
        .unwrap()
        .insert("stageId".to_string(), json!("linked.entry"));
    events[0]["plan"]["compiledHooks"][0]
        .as_object_mut()
        .unwrap()
        .remove("stageIdentifier");
    let error = replay_chain_events(
        events,
        &ReplayOptions {
            sort: None,
            strict: Some(true),
        },
    )
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("stageIdentifier must be a string"),
        "missing stageIdentifier must fail loudly, got: {error}"
    );
}

#[test]
fn order_trigger_hook_with_delay_is_a_structural_error() {
    for kind in ["mint", "dock"] {
        let events = vec![json!({
            "eventName": "PlanRegistered",
            "blockNumber": 1,
            "logIndex": 0,
            "transactionHash": "0x01",
            "plan": {
                "planId": "0x01",
                "zhixuId": "demo",
                "compiledHooks": [{
                    "hookId": "flow.start#BIRTH",
                    "stageId": "flow.start",
                    "stageIdentifier": "flow.start",
                    "hookName": "BIRTH",
                    "orderTriggerKind": kind,
                    "emitReady": true,
                    "instructions": [
                        {"op": "SIGNAL", "signalKey": "0x50"},
                        {"op": "DELAY", "delaySeconds": 5}
                    ]
                }],
                "dependencyIndex": { "0x50": ["flow.start#BIRTH"] }
            }
        })];
        let error = replay_chain_events(
            events,
            &ReplayOptions {
                sort: None,
                strict: Some(true),
            },
        )
        .unwrap_err();
        assert!(
            error.to_string().contains("order-trigger hook")
                && error.to_string().contains("must not contain DELAY"),
            "{kind}: {error}"
        );
    }

    let events = vec![json!({
        "eventName": "PlanRegistered",
        "blockNumber": 1,
        "logIndex": 0,
        "transactionHash": "0x01",
        "plan": {
            "planId": "0x01",
            "zhixuId": "demo",
            "compiledHooks": [{
                "hookId": "flow.pay#TIMEOUT",
                "stageId": "flow.pay",
                "stageIdentifier": "flow.pay",
                "hookName": "TIMEOUT",
                "orderTriggerKind": "none",
                "emitReady": true,
                "instructions": [
                    {"op": "SIGNAL", "signalKey": "0x50"},
                    {"op": "DELAY", "delaySeconds": 5}
                ]
            }],
            "dependencyIndex": { "0x50": ["flow.pay#TIMEOUT"] }
        }
    })];
    replay_chain_events(
        events,
        &ReplayOptions {
            sort: None,
            strict: Some(true),
        },
    )
    .expect("watcher hooks may carry DELAY");
}

#[test]
fn epoch_zero_submission_is_a_real_anchor_for_delay() {
    let mut order = OracleOrderState::default();
    order.signals.insert(
        "0x50".to_string(),
        json!({"submittedAt": "1970-01-01T00:00:00.000Z"}),
    );
    let anchored = signal_value(&order, "0x50").expect("signal value");
    assert_eq!(anchored.anchor_at, Some(0));
    let delayed = delay_value(anchored, 10, "1970-01-01T00:00:05.000Z")
        .expect("epoch-0 anchor must be usable by DELAY");
    assert!(delayed.wait);
    assert_eq!(delayed.due_at, Some(10));

    let not_ready = not_value(false_value());
    assert!(not_ready.value);
    assert_eq!(not_ready.anchor_at, None);
    let error = delay_value(not_ready, 10, "2026-04-27T00:00:00Z").unwrap_err();
    assert!(
        error
            .to_string()
            .contains("DELAY requires a positively anchored operand"),
        "{error}"
    );
}

fn order_with_signals(signals: &[(&str, &str)]) -> OracleOrderState {
    let mut order = OracleOrderState::default();
    for (key, submitted_at) in signals {
        order
            .signals
            .insert(key.to_string(), json!({"submittedAt": submitted_at}));
    }
    order
}

#[test]
fn or_winner_carries_its_own_expiry_verbatim() {
    let instructions = vec![
        json!({"op": "SIGNAL", "signalKey": "0x50"}),
        json!({"op": "SIGNAL", "signalKey": "0x51"}),
        json!({"op": "DELAY", "delaySeconds": 600}),
        json!({"op": "NOT"}),
        json!({"op": "AND", "arity": 2}),
        json!({"op": "SIGNAL", "signalKey": "0x52"}),
        json!({"op": "OR", "arity": 2}),
    ];
    let result = evaluate_instructions(
        &order_with_signals(&[
            ("0x50", "2026-04-27T00:00:10.000Z"),
            ("0x51", "2026-04-27T00:00:00.000Z"),
            ("0x52", "2026-04-27T00:00:20.000Z"),
        ]),
        &instructions,
        "2026-04-27T00:00:30.000Z",
    )
    .expect("or picks the earliest-maturity ready branch");
    assert!(result.value && !result.wait && !result.cancel);
    assert_eq!(
        result.anchor_at,
        Some(seconds_from_iso("2026-04-27T00:00:10.000Z").unwrap()),
        "and branch matures first"
    );
    assert_eq!(
        result.due_at,
        Some(seconds_from_iso("2026-04-27T00:10:00.000Z").unwrap()),
        "the winning branch's decaying validity floats up verbatim"
    );
}

#[test]
fn decaying_veto_positions_are_rejected_at_registration() {
    let veto_root = watcher_plan(
        "flow.pay#VETO_ROOT",
        json!([
            { "op": "SIGNAL", "signalKey": "0x50" },
            { "op": "DELAY", "delaySeconds": 5 },
            { "op": "NOT" }
        ]),
    );
    let error = replay_chain_events(
        vec![plan_registered_event(veto_root)],
        &ReplayOptions {
            sort: None,
            strict: Some(true),
        },
    )
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("places a decaying veto at the root"),
        "{error}"
    );

    let veto_under_or = watcher_plan(
        "flow.pay#VETO_OR",
        json!([
            { "op": "SIGNAL", "signalKey": "0x50" },
            { "op": "DELAY", "delaySeconds": 5 },
            { "op": "NOT" },
            { "op": "SIGNAL", "signalKey": "0x51" },
            { "op": "OR", "arity": 2 }
        ]),
    );
    let error = replay_chain_events(
        vec![plan_registered_event(veto_under_or)],
        &ReplayOptions {
            sort: None,
            strict: Some(true),
        },
    )
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("places a decaying veto under an OR branch"),
        "{error}"
    );

    let veto_in_delay = watcher_plan(
        "flow.pay#VETO_DELAY",
        json!([
            { "op": "SIGNAL", "signalKey": "0x50" },
            { "op": "SIGNAL", "signalKey": "0x51" },
            { "op": "DELAY", "delaySeconds": 5 },
            { "op": "NOT" },
            { "op": "AND", "arity": 2 },
            { "op": "DELAY", "delaySeconds": 10 }
        ]),
    );
    let error = replay_chain_events(
        vec![plan_registered_event(veto_in_delay)],
        &ReplayOptions {
            sort: None,
            strict: Some(true),
        },
    )
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("applies DELAY to an operand containing a decaying veto"),
        "{error}"
    );

    let double_negation = watcher_plan(
        "flow.pay#VETO_NOT",
        json!([
            { "op": "SIGNAL", "signalKey": "0x50" },
            { "op": "DELAY", "delaySeconds": 5 },
            { "op": "NOT" },
            { "op": "NOT" }
        ]),
    );
    let error = replay_chain_events(
        vec![plan_registered_event(double_negation)],
        &ReplayOptions {
            sort: None,
            strict: Some(true),
        },
    )
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("applies NOT to a non-bare-SIGNAL operand"),
        "{error}"
    );

    let mut legal = watcher_plan(
        "flow.pay#VETO",
        json!([
            { "op": "SIGNAL", "signalKey": "0x50" },
            { "op": "SIGNAL", "signalKey": "0x51" },
            { "op": "DELAY", "delaySeconds": 5 },
            { "op": "NOT" },
            { "op": "AND", "arity": 2 }
        ]),
    );
    legal["dependencyIndex"] = json!({ "0x50": ["flow.pay#VETO"], "0x51": ["flow.pay#VETO"] });
    replay_chain_events(
        vec![plan_registered_event(legal)],
        &ReplayOptions {
            sort: None,
            strict: Some(true),
        },
    )
    .expect("a veto as a direct conjunction operand registers");
}

#[test]
fn non_positive_delay_seconds_is_rejected_at_replay_decode() {
    let anchored = EvalValue {
        value: true,
        wait: false,
        cancel: false,
        due_at: None,
        anchor_at: Some(1000),
    };
    for delay_seconds in [0i64, -3] {
        let error = delay_value(anchored, delay_seconds, "2026-04-27T00:00:00Z").unwrap_err();
        assert!(
            error
                .to_string()
                .contains("DELAY delaySeconds must be positive"),
            "delaySeconds={delay_seconds}: {error}"
        );
    }
    let instructions = vec![
        json!({"op": "SIGNAL", "signalKey": "0x50"}),
        json!({"op": "DELAY", "delaySeconds": 0}),
    ];
    let error = evaluate_instructions(
        &OracleOrderState::default(),
        &instructions,
        "2026-04-27T00:00:00Z",
    )
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("DELAY delaySeconds must be positive"),
        "{error}"
    );
}

#[test]
fn epoch_zero_due_is_persisted_and_poke_eligible() {
    let events = vec![
        json!({
            "eventName": "PlanRegistered",
            "blockNumber": 1,
            "logIndex": 0,
            "transactionHash": "0x01",
            "plan": {
                "planId": "0x01",
                "zhixuId": "demo",
                "compiledHooks": [{
                    "hookId": "flow.start#WAIT",
                    "stageId": "flow.start",
                    "stageIdentifier": "flow.start",
                    "hookName": "WAIT",
                    "orderTriggerKind": "none",
                    "emitReady": true,
                    "instructions": [
                        {"op": "SIGNAL", "signalKey": "0x50"},
                        {"op": "DELAY", "delaySeconds": 5}
                    ]
                }],
                "dependencyIndex": { "0x50": ["flow.start#WAIT"] }
            }
        }),
        json!({
            "eventName": "OrderRegistered",
            "blockNumber": 2,
            "logIndex": 0,
            "transactionHash": "0x02",
            "planId": "0x01",
            "zhixuId": "demo",
            "orderId": "order-1",
            "registeredAt": "1969-12-31T23:59:50.000Z"
        }),
        json!({
            "eventName": "SignalSubmitted",
            "blockNumber": 3,
            "logIndex": 0,
            "transactionHash": "0x03",
            "planId": "0x01",
            "zhixuId": "demo",
            "orderId": "order-1",
            "sourceId": "0x30",
            "signalId": "0x40",
            "signalKey": "0x50",
            "senderId": "seller",
            "submittedAt": "1969-12-31T23:59:55.000Z"
        }),
        json!({
            "eventName": "HookStatusChanged",
            "blockNumber": 3,
            "logIndex": 1,
            "transactionHash": "0x03",
            "planId": "0x01",
            "zhixuId": "demo",
            "orderId": "order-1",
            "hookId": "flow.start#WAIT",
            "status": "wait",
            "dueAt": "1970-01-01T00:00:00.000Z"
        }),
        json!({
            "eventName": "TimerPoked",
            "blockNumber": 4,
            "logIndex": 0,
            "transactionHash": "0x04",
            "planId": "0x01",
            "zhixuId": "demo",
            "orderId": "order-1",
            "hookId": "flow.start#WAIT",
            "pokedAt": "1970-01-01T00:00:00.000Z"
        }),
        json!({
            "eventName": "HookReady",
            "blockNumber": 4,
            "logIndex": 1,
            "transactionHash": "0x04",
            "planId": "0x01",
            "zhixuId": "demo",
            "orderId": "order-1",
            "hookId": "flow.start#WAIT",
            "stageIdentifier": "flow.start",
            "hookName": "WAIT"
        }),
    ];
    let result = replay_chain_events(
        events,
        &ReplayOptions {
            sort: None,
            strict: Some(true),
        },
    )
    .unwrap();
    assert_eq!(
        result["mismatches"].as_array().map(Vec::len),
        Some(0),
        "epoch-0 due must replay without mismatch: {}",
        result["mismatches"]
    );
    let observed = result["observed"].as_array().unwrap();
    let wait = observed
        .iter()
        .find(|item| item["status"] == "wait")
        .expect("wait observation with the epoch-0 dueAt");
    assert_eq!(wait["dueAt"], "1970-01-01T00:00:00.000Z");
    assert!(
        observed.iter().any(|item| item["eventName"] == "HookReady"),
        "poke at epoch 0 must make the hook ready: {observed:?}"
    );
    let order = &result["state"]["orders"]["0x01::order-1"];
    assert_eq!(order["hookStatuses"]["flow.start#WAIT"]["status"], "ready");
}

fn watcher_plan(hook_id: &str, instructions: Value) -> Value {
    json!({
        "planId": "0x01",
        "zhixuId": "demo",
        "compiledHooks": [{
            "hookId": hook_id,
            "stageId": "flow.pay",
            "stageIdentifier": "flow.pay",
            "hookName": "TIMEOUT",
            "orderTriggerKind": "none",
            "emitReady": true,
            "instructions": instructions,
        }],
        "dependencyIndex": { "0x50": [hook_id] }
    })
}

fn plan_registered_event(plan: Value) -> Value {
    json!({
        "eventName": "PlanRegistered",
        "blockNumber": 1,
        "logIndex": 0,
        "transactionHash": "0x01",
        "plan": plan
    })
}

#[test]
fn delay_cap_30d_is_enforced_at_registration() {
    let over = watcher_plan(
        "flow.pay#TIMEOUT",
        json!([
            { "op": "SIGNAL", "signalKey": "0x50" },
            { "op": "DELAY", "delaySeconds": 2592001 }
        ]),
    );
    let error = replay_chain_events(
        vec![plan_registered_event(over)],
        &ReplayOptions {
            sort: None,
            strict: Some(true),
        },
    )
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("exceeds the maximum allowed delay of 2592000s (30d)"),
        "{error}"
    );

    let at_limit = watcher_plan(
        "flow.pay#TIMEOUT",
        json!([
            { "op": "SIGNAL", "signalKey": "0x50" },
            { "op": "DELAY", "delaySeconds": 2592000 }
        ]),
    );
    replay_chain_events(
        vec![plan_registered_event(at_limit)],
        &ReplayOptions {
            sort: None,
            strict: Some(true),
        },
    )
    .expect("a 30d delay sits exactly at the contract cap and must register");
}

#[test]
fn root_without_positive_anchor_is_rejected_at_registration() {
    let plan = watcher_plan(
        "flow.pay#GUARD",
        json!([
            { "op": "SIGNAL", "signalKey": "0x50" },
            { "op": "NOT" }
        ]),
    );
    let error = replay_chain_events(
        vec![plan_registered_event(plan)],
        &ReplayOptions {
            sort: None,
            strict: Some(true),
        },
    )
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("no positive signal anchor at the root"),
        "{error}"
    );
}

#[test]
fn instruction_depth_cap_120_is_enforced_at_registration() {
    let nested_and_plan = |levels: usize| {
        let mut instructions = vec![json!({ "op": "SIGNAL", "signalKey": "0x50" })];
        for _ in 0..levels {
            instructions.push(json!({ "op": "SIGNAL", "signalKey": "0x50" }));
            instructions.push(json!({ "op": "AND", "arity": 2 }));
        }
        watcher_plan("flow.pay#DEEP", Value::Array(instructions))
    };
    let error = replay_chain_events(
        vec![plan_registered_event(nested_and_plan(121))],
        &ReplayOptions {
            sort: None,
            strict: Some(true),
        },
    )
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("instruction nesting exceeds the maximum depth of 120"),
        "{error}"
    );
    replay_chain_events(
        vec![plan_registered_event(nested_and_plan(120))],
        &ReplayOptions {
            sort: None,
            strict: Some(true),
        },
    )
    .expect("nesting depth exactly 120 sits at the cap and must register");
}

#[test]
fn dependency_index_key_mismatch_is_rejected_at_registration() {
    let mut wrong_key = watcher_plan(
        "flow.pay#TIMEOUT",
        json!([{ "op": "SIGNAL", "signalKey": "0x50" }]),
    );
    wrong_key["dependencyIndex"] = json!({ "0x99": ["flow.pay#TIMEOUT"] });
    let error = replay_chain_events(
        vec![plan_registered_event(wrong_key)],
        &ReplayOptions {
            sort: None,
            strict: Some(true),
        },
    )
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("dependencyIndex maps key 0x99 to hook flow.pay#TIMEOUT but the key is not a SIGNAL atom"),
        "{error}"
    );
    assert!(
        error.to_string().contains("HookDependencyKeyMismatch"),
        "{error}"
    );

    let mut unindexed = watcher_plan(
        "flow.pay#TIMEOUT",
        json!([{ "op": "SIGNAL", "signalKey": "0x50" }]),
    );
    unindexed["dependencyIndex"] = json!({});
    let error = replay_chain_events(
        vec![plan_registered_event(unindexed)],
        &ReplayOptions {
            sort: None,
            strict: Some(true),
        },
    )
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("references SIGNAL key 0x50 that dependencyIndex does not map back to it"),
        "{error}"
    );
    assert!(
        error.to_string().contains("HookDependencyKeyMismatch"),
        "{error}"
    );

    let mut missing_index = watcher_plan(
        "flow.pay#TIMEOUT",
        json!([{ "op": "SIGNAL", "signalKey": "0x50" }]),
    );
    missing_index
        .as_object_mut()
        .expect("plan object")
        .remove("dependencyIndex");
    let error = replay_chain_events(
        vec![plan_registered_event(missing_index)],
        &ReplayOptions {
            sort: None,
            strict: Some(true),
        },
    )
    .unwrap_err();
    assert!(
        error.to_string().contains("missing dependencyIndex"),
        "{error}"
    );

    replay_chain_events(
        vec![plan_registered_event(watcher_plan(
            "flow.pay#TIMEOUT",
            json!([{ "op": "SIGNAL", "signalKey": "0x50" }]),
        ))],
        &ReplayOptions {
            sort: None,
            strict: Some(true),
        },
    )
    .expect("an index that mirrors the SIGNAL atoms exactly must register");
}

#[test]
fn silent_order_trigger_is_rejected_at_registration() {
    let mut silent = single_hook_plan(
        "flow.start#TRIGGER",
        json!([{ "op": "SIGNAL", "signalKey": "0x50" }]),
    );
    silent["compiledHooks"][0]["emitReady"] = json!(false);
    let error = replay_chain_events(
        vec![plan_registered_event(silent)],
        &ReplayOptions {
            sort: None,
            strict: Some(true),
        },
    )
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("order-trigger hook flow.start#TRIGGER must carry emitReady=true"),
        "{error}"
    );
    assert!(
        error.to_string().contains("SilentOrderTriggerHook"),
        "{error}"
    );

    let mut silent_watcher = watcher_plan(
        "flow.pay#WATCH",
        json!([{ "op": "SIGNAL", "signalKey": "0x50" }]),
    );
    silent_watcher["compiledHooks"][0]["emitReady"] = json!(false);
    replay_chain_events(
        vec![plan_registered_event(silent_watcher)],
        &ReplayOptions {
            sort: None,
            strict: Some(true),
        },
    )
    .expect("a silent non-trigger watcher is a legal shape and must register");
}

#[test]
fn duplicate_order_registered_is_absorbed_without_resetting_state() {
    let plan = single_hook_plan(
        "flow.start#BIRTH",
        json!([{ "op": "SIGNAL", "signalKey": "0x50" }]),
    );
    let mut events = vec![
        plan_registered_event(plan),
        json!({
            "eventName": "OrderRegistered",
            "blockNumber": 2,
            "logIndex": 0,
            "transactionHash": "0x02",
            "planId": "0x01",
            "zhixuId": "demo",
            "orderId": "order-1",
            "registeredAt": "2026-04-27T00:00:00.000Z"
        }),
        json!({
            "eventName": "SignalSubmitted",
            "blockNumber": 3,
            "logIndex": 0,
            "transactionHash": "0x03",
            "planId": "0x01",
            "zhixuId": "demo",
            "orderId": "order-1",
            "sourceId": "0x30",
            "signalId": "0x40",
            "signalKey": "0x50",
            "senderId": "sender",
            "submittedAt": "2026-04-27T00:00:00.000Z"
        }),
        json!({
            "eventName": "HookReady",
            "blockNumber": 3,
            "logIndex": 1,
            "transactionHash": "0x03",
            "planId": "0x01",
            "zhixuId": "demo",
            "orderId": "order-1",
            "hookId": "flow.start#BIRTH",
            "stageIdentifier": "flow.start",
            "hookName": "START"
        }),
    ];
    events.push(json!({
        "eventName": "OrderRegistered",
        "blockNumber": 4,
        "logIndex": 0,
        "transactionHash": "0x04",
        "planId": "0x01",
        "zhixuId": "demo",
        "orderId": "order-1",
        "registeredAt": "2026-04-27T00:00:00.000Z"
    }));
    let result = replay_chain_events(
        events,
        &ReplayOptions {
            sort: None,
            strict: Some(true),
        },
    )
    .expect("a re-emitted OrderRegistered must be absorbed");
    assert_eq!(result["mismatches"].as_array().map(Vec::len), Some(0));
    let order = &result["state"]["orders"]["0x01::order-1"];
    assert_eq!(
        order["signals"].as_object().map(serde_json::Map::len),
        Some(1),
        "absorbed re-registration must keep accumulated signals: {order}"
    );
    assert_eq!(order["hookStatuses"]["flow.start#BIRTH"]["status"], "ready");

    let plan = single_hook_plan(
        "flow.start#BIRTH",
        json!([{ "op": "SIGNAL", "signalKey": "0x50" }]),
    );
    let events = vec![
        plan_registered_event(plan),
        json!({
            "eventName": "OrderRegistered",
            "blockNumber": 2,
            "logIndex": 0,
            "transactionHash": "0x02",
            "planId": "0x01",
            "zhixuId": "demo",
            "orderId": "order-1",
            "registeredAt": "2026-04-27T00:00:00.000Z"
        }),
        json!({
            "eventName": "OrderRegistered",
            "blockNumber": 3,
            "logIndex": 0,
            "transactionHash": "0x03",
            "planId": "0x01",
            "zhixuId": "impostor",
            "orderId": "order-1",
            "registeredAt": "2026-04-27T00:00:01.000Z"
        }),
    ];
    let error = replay_chain_events(
        events,
        &ReplayOptions {
            sort: None,
            strict: Some(true),
        },
    )
    .unwrap_err();
    assert!(
        error.to_string().contains("carries a different zhixuId"),
        "{error}"
    );
}

#[test]
fn non_integer_block_number_fails_loudly_at_sorting() {
    let plan = single_hook_plan(
        "flow.start#BIRTH",
        json!([{ "op": "SIGNAL", "signalKey": "0x50" }]),
    );
    for bad_block in [json!("later"), json!(1.5), Value::Null] {
        let mut event = plan_registered_event(plan.clone());
        event["blockNumber"] = bad_block;
        let error = replay_chain_events(
            vec![event],
            &ReplayOptions {
                sort: None,
                strict: Some(true),
            },
        )
        .unwrap_err();
        assert!(
            error.to_string().contains("blockNumber must be an integer")
                && error
                    .to_string()
                    .contains("refuses to fold a non-integer to 0"),
            "{error}"
        );
    }
    let mut event = plan_registered_event(plan);
    event.as_object_mut().unwrap().remove("blockNumber");
    let error = replay_chain_events(
        vec![event],
        &ReplayOptions {
            sort: None,
            strict: Some(true),
        },
    )
    .unwrap_err();
    assert!(
        error.to_string().contains("blockNumber must be an integer"),
        "{error}"
    );
    let plan = single_hook_plan(
        "flow.start#BIRTH",
        json!([{ "op": "SIGNAL", "signalKey": "0x50" }]),
    );
    let mut event = plan_registered_event(plan);
    event["blockNumber"] = json!("later");
    replay_chain_events(
        vec![event],
        &ReplayOptions {
            sort: Some(false),
            strict: Some(true),
        },
    )
    .expect("unsorted replays take the caller-asserted order and skip the sort-key gate");
}

#[test]
fn unrenderable_due_at_fails_loudly_instead_of_folding_to_permanent_wait() {
    let error = render_due_at(8_210_866_176_000).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("refusing to fold to an undated permanent wait"),
        "{error}"
    );
    assert_eq!(render_due_at(0).unwrap(), "1970-01-01T00:00:00.000Z");
}

#[test]
fn plan_without_instruction_track_fails_loudly_instead_of_hollow_pass() {
    let rust_shaped_plan = json!({
        "planId": "0x01",
        "zhixuId": "demo",
        "compiledHooks": [{
            "hookId": "flow.pay#OBSERVE",
            "stageIdentifier": "flow.pay",
            "hookName": "OBSERVE",
            "orderTriggerKind": "none",
            "emitReady": true,
            "dependencies": [{ "source": "buyer", "signalName": "flow.pay.ack" }]
        }],
        "dependencyIndex": { "buyer::flow.pay.ack": ["flow.pay#OBSERVE"] }
    });
    let events = vec![
        plan_registered_event(rust_shaped_plan),
        json!({
            "eventName": "OrderRegistered",
            "blockNumber": 2,
            "logIndex": 0,
            "transactionHash": "0x02",
            "planId": "0x01",
            "zhixuId": "demo",
            "orderId": "order-1",
            "registeredAt": "2026-04-27T00:00:00.000Z"
        }),
        json!({
            "eventName": "SignalSubmitted",
            "blockNumber": 3,
            "logIndex": 0,
            "transactionHash": "0x03",
            "planId": "0x01",
            "zhixuId": "demo",
            "orderId": "order-1",
            "sourceId": "0x30",
            "signalId": "0x40",
            "signalKey": "0x50",
            "senderId": "sender",
            "submittedAt": "2026-04-27T00:00:00.000Z"
        }),
    ];
    let error = replay_chain_events(
        events,
        &ReplayOptions {
            sort: None,
            strict: Some(true),
        },
    )
    .unwrap_err();
    let message = error.to_string();
    assert!(
        message.contains("flow.pay#OBSERVE")
            && message.contains("missing instructions")
            && message.contains("cannot be replayed"),
        "{message}"
    );

    let empty_track = json!({
        "planId": "0x01",
        "zhixuId": "demo",
        "compiledHooks": [{
            "hookId": "flow.pay#OBSERVE",
            "stageId": "flow.pay",
            "stageIdentifier": "flow.pay",
            "hookName": "OBSERVE",
            "orderTriggerKind": "none",
            "emitReady": true,
            "instructions": []
        }],
        "dependencyIndex": { "0x50": ["flow.pay#OBSERVE"] }
    });
    let error = replay_chain_events(
        vec![plan_registered_event(empty_track)],
        &ReplayOptions {
            sort: None,
            strict: Some(true),
        },
    )
    .unwrap_err();
    assert!(
        error.to_string().contains("carries no instructions"),
        "{error}"
    );
}

fn admission_plan(admissions: Value) -> Value {
    let mut plan = watcher_plan(
        "flow.pay#OBSERVE",
        json!([
            { "op": "SIGNAL", "signalKey": "0x50" }
        ]),
    );
    plan["admissions"] = admissions;
    plan
}

fn admission_entry(label: &str, instructions: Value) -> Value {
    json!({
        "admissionId": label,
        "stageIdentifier": "flow.pay",
        "signalName": label,
        "instructions": instructions,
    })
}

#[test]
fn admission_decaying_veto_positions_register_under_the_filter_gate() {
    let admissions = json!([
        admission_entry(
            "root_veto",
            json!([
                { "op": "SIGNAL", "signalKey": "0x51" },
                { "op": "DELAY", "delaySeconds": 5 },
                { "op": "NOT" }
            ])
        ),
        admission_entry(
            "or_veto",
            json!([
                { "op": "SIGNAL", "signalKey": "0x50" },
                { "op": "SIGNAL", "signalKey": "0x51" },
                { "op": "DELAY", "delaySeconds": 5 },
                { "op": "NOT" },
                { "op": "OR", "arity": 2 }
            ])
        ),
        admission_entry(
            "delay_operand_veto",
            json!([
                { "op": "SIGNAL", "signalKey": "0x50" },
                { "op": "SIGNAL", "signalKey": "0x51" },
                { "op": "DELAY", "delaySeconds": 5 },
                { "op": "NOT" },
                { "op": "AND", "arity": 2 },
                { "op": "DELAY", "delaySeconds": 10 }
            ])
        ),
        admission_entry(
            "no_positive_anchor",
            json!([
                { "op": "SIGNAL", "signalKey": "0x50" },
                { "op": "NOT" }
            ])
        )
    ]);
    replay_chain_events(
        vec![plan_registered_event(admission_plan(admissions))],
        &ReplayOptions {
            sort: None,
            strict: Some(true),
        },
    )
    .expect("filter-gate admission shapes register");
}

#[test]
fn admission_not_vocabulary_and_delay_bounds_are_enforced_at_registration() {
    let composite_not = admission_plan(json!([admission_entry(
        "cmp",
        json!([
            { "op": "SIGNAL", "signalKey": "0x50" },
            { "op": "SIGNAL", "signalKey": "0x51" },
            { "op": "AND", "arity": 2 },
            { "op": "NOT" }
        ])
    )]));
    let error = replay_chain_events(
        vec![plan_registered_event(composite_not)],
        &ReplayOptions {
            sort: None,
            strict: Some(true),
        },
    )
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("admission cmp applies NOT to a non-bare-SIGNAL operand"),
        "{error}"
    );

    for (label, seconds) in [("zero", 0i64), ("over_30d", 2592001)] {
        let over = admission_plan(json!([admission_entry(
            label,
            json!([
                { "op": "SIGNAL", "signalKey": "0x50" },
                { "op": "DELAY", "delaySeconds": seconds }
            ])
        )]));
        let error = replay_chain_events(
            vec![plan_registered_event(over)],
            &ReplayOptions {
                sort: None,
                strict: Some(true),
            },
        )
        .unwrap_err();
        assert!(
            error.to_string().contains("DELAY")
                && (error.to_string().contains("must be positive")
                    || error
                        .to_string()
                        .contains("exceeds the maximum allowed delay")),
            "{label}: {error}"
        );
    }
}

#[test]
fn duplicate_hook_id_in_plan_is_a_structural_error() {
    let events = vec![json!({
        "eventName": "PlanRegistered",
        "blockNumber": 1,
        "logIndex": 0,
        "transactionHash": "0x01",
        "plan": {
            "planId": "0x01",
            "zhixuId": "demo",
            "compiledHooks": [
                {
                    "hookId": "match.exchange#PAIR",
                    "stageId": "match.exchange",
                    "stageIdentifier": "match.exchange",
                    "hookName": "PAIR",
                    "orderTriggerKind": "mint",
                    "emitReady": true,
                    "instructions": [{"op": "SIGNAL", "signalKey": "0x50"}]
                },
                {
                    "hookId": "match.exchange#PAIR",
                    "stageId": "match.exchange",
                    "stageIdentifier": "match.exchange",
                    "hookName": "PAIR",
                    "orderTriggerKind": "none",
                    "emitReady": false,
                    "instructions": [{"op": "SIGNAL", "signalKey": "0x50"}]
                }
            ],
            "dependencyIndex": { "0x50": ["match.exchange#PAIR"] }
        }
    })];
    let error = replay_chain_events(
        events,
        &ReplayOptions {
            sort: None,
            strict: Some(true),
        },
    )
    .unwrap_err();
    assert!(error.to_string().contains("duplicate hookId"), "{error}");
}

#[test]
fn duplicate_order_triggered_with_conflicting_tx_is_loud() {
    let base_events = |trigger_tx: &str| {
        vec![
            json!({
                "eventName": "PlanRegistered",
                "blockNumber": 1,
                "logIndex": 0,
                "transactionHash": "0x01",
                "plan": {
                    "planId": "0x01",
                    "zhixuId": "demo",
                    "compiledHooks": [{
                        "hookId": "linked.entry#BIRTH",
                        "stageId": "linked.entry",
                        "stageIdentifier": "linked.entry",
                        "hookName": "BIRTH",
                        "orderTriggerKind": "mint",
                        "emitReady": true,
                        "instructions": [{"op": "SIGNAL", "signalKey": "0x50"}]
                    }],
                    "dependencyIndex": { "0x50": ["linked.entry#BIRTH"] }
                }
            }),
            json!({
                "eventName": "OrderRegistered",
                "blockNumber": 2,
                "logIndex": 0,
                "transactionHash": "0x02",
                "planId": "0x01",
                "zhixuId": "demo",
                "orderId": "order-7",
                "registeredAt": "2026-04-27T00:00:00.000Z"
            }),
            json!({
                "eventName": "OrderTriggered",
                "blockNumber": 3,
                "logIndex": 0,
                "transactionHash": trigger_tx,
                "planId": "0x01",
                "orderId": "order-7"
            }),
        ]
    };
    replay_chain_events(
        {
            let mut events = base_events("0x03");
            let replayed = events[2].clone();
            events.push(replayed);
            events
        },
        &ReplayOptions {
            sort: None,
            strict: Some(true),
        },
    )
    .expect("same-tx duplicate OrderTriggered must be absorbed");
    let error = replay_chain_events(
        {
            let mut events = base_events("0x03");
            let mut conflicting = events[2].clone();
            conflicting["transactionHash"] = json!("0x09");
            conflicting["blockNumber"] = json!(4);
            conflicting["logIndex"] = json!(0);
            events.push(conflicting);
            events
        },
        &ReplayOptions {
            sort: None,
            strict: Some(true),
        },
    )
    .unwrap_err();
    assert!(
        error.to_string().contains("duplicate OrderTriggered"),
        "{error}"
    );
}
