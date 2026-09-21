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
    // ready×wait 混合时，等待分支的陈旧锚点不得参与归约——就绪胜者
    // 自带计时器（对齐 hook-dsl Expr::Or 与合约 _orValue）：归约结果
    // 的 due 取就绪分支自身的计时，不取双分支 due 的较早者。
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
    // 未知指令一律 unsupported：求值器只认冻结指令集（SIGNAL/NOT/AND/OR/DELAY）。
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
fn retired_fan_in_instruction_is_rejected_as_unknown() {
    // 指令集收敛：撮合扇入指令无官方生产者，仅手工 plan 可触达，
    // 不在指令集内。求值器没有专属分支——携带该指令的 plan 与任意
    // 未知指令同口径，在 unsupported 错误上响亮失败。指令字面拆写拼接，
    // 使仓内对该词的全文检索保持零命中。
    let retired_op = concat!("MER", "GE");
    let instructions = vec![
        json!({"op": "SIGNAL", "signalKey": "0x50"}),
        json!({"op": "SIGNAL", "signalKey": "0x51"}),
        json!({"op": retired_op, "arity": 2}),
    ];
    let error = evaluate_instructions(
        &OracleOrderState::default(),
        &instructions,
        "2026-04-27T00:01:00Z",
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
fn retired_fan_in_hook_plan_fails_loudly_in_replay() {
    // 负向 golden：手工 plan 携带指令集外的扇入指令时，回放整体以错误收场
    // （envelope ok:false），不产出"部分观察 + mismatch"的软化报告——
    // 与合约 commitPlan 注册边界的响亮拒绝同口径。
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
    // 真实事件流：合约对 →Ready 先发 HookStatusChanged(ready) 再发
    // HookReady；逐字重复的 wait 状态变更被吸收——两者都不得产生
    // missing-observed 假阳性。
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
    // ready 状态变更被裁剪、重复 wait 被吸收：expected 只剩 HookReady 与
    // 首条 wait。
    let expected = result["expected"].as_array().unwrap();
    assert_eq!(expected.len(), 2, "expected: {expected:?}");
    assert_eq!(expected[0]["eventName"], "HookReady");
    assert_eq!(expected[1]["eventName"], "HookStatusChanged");
    assert_eq!(expected[1]["status"], "wait");
}

#[test]
fn order_link_birth_is_derived_from_hook_ready() {
    // order-link 出生（triggerOrderFromSignalFromModule）：链上不
    // _recordSignal 但 emit HookReady。oracle 从 HookReady 推导出生
    // （runtime ready + 阶段物化），并接受链上断言的观察。
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
            "transactionHash": "0x03",
            "planId": "0x01",
            "orderId": "order-7"
        }),
        json!({
            "eventName": "HookReady",
            "blockNumber": 3,
            "logIndex": 1,
            "transactionHash": "0x03",
            "planId": "0x01",
            "zhixuId": "demo",
            "orderId": "order-7",
            "hookId": "linked.entry#BIRTH",
            "stageIdentifier": "linked.entry",
            "hookName": "BIRTH"
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
        "order-link birth must replay without mismatch"
    );
    let order = &result["state"]["orders"]["0x01::order-7"];
    assert_eq!(
        order["hookStatuses"]["linked.entry#BIRTH"]["status"],
        "ready"
    );
    assert_eq!(
        order["hookStatuses"]["linked.entry#BIRTH"]["readyEmitted"],
        true
    );
    assert_eq!(order["materializedStages"]["linked.entry"], true);
}

#[test]
fn dock_hook_ready_without_signal_stays_a_mismatch() {
    // dock 出生事实恒先落本订单（createDockedOrderFromModule 内
    // _recordSignal → SignalSubmitted）：没有信号先行的链上 HookReady
    // 是事实缺失的异常，oracle 不做断言推导，mismatch 保持暴露。
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
                    "hookId": "target.entry#DOCK_ENTER",
                    "stageId": "target.entry",
                    "stageIdentifier": "target.entry",
                    "hookName": "DOCK_ENTER",
                    "orderTriggerKind": "dock",
                    "emitReady": true,
                    "instructions": [{"op": "SIGNAL", "signalKey": "0x50"}]
                }],
                "dependencyIndex": { "0x50": ["target.entry#DOCK_ENTER"] }
            }
        }),
        json!({
            "eventName": "OrderRegistered",
            "blockNumber": 2,
            "logIndex": 0,
            "transactionHash": "0x02",
            "planId": "0x01",
            "zhixuId": "demo",
            "orderId": "order-9",
            "registeredAt": "2026-04-27T00:00:00.000Z"
        }),
        json!({
            "eventName": "HookReady",
            "blockNumber": 3,
            "logIndex": 0,
            "transactionHash": "0x03",
            "planId": "0x01",
            "zhixuId": "demo",
            "orderId": "order-9",
            "hookId": "target.entry#DOCK_ENTER",
            "stageIdentifier": "target.entry",
            "hookName": "DOCK_ENTER"
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
    let mismatches = result["mismatches"].as_array().unwrap();
    assert_eq!(mismatches.len(), 1, "{mismatches:?}");
    assert_eq!(mismatches[0]["reason"], "missing-observed");
    // 断言不被接受：订单状态不被无信号的链上 HookReady 污染。
    let order = &result["state"]["orders"]["0x01::order-9"];
    assert!(order["hookStatuses"].as_object().unwrap().is_empty());
    assert!(order["materializedStages"].as_object().unwrap().is_empty());
}

#[test]
fn dock_birth_with_recorded_fact_replays_clean() {
    // 合法 dock 出生流：出生事实（SignalSubmitted）先行，求值路径推导
    // HookReady，链上 HookReady 与之一一配对——mint-only 推导门不影响。
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
                    "hookId": "target.entry#DOCK_ENTER",
                    "stageId": "target.entry",
                    "stageIdentifier": "target.entry",
                    "hookName": "DOCK_ENTER",
                    "orderTriggerKind": "dock",
                    "emitReady": true,
                    "instructions": [{"op": "SIGNAL", "signalKey": "0x50"}]
                }],
                "dependencyIndex": { "0x50": ["target.entry#DOCK_ENTER"] }
            }
        }),
        json!({
            "eventName": "OrderRegistered",
            "blockNumber": 2,
            "logIndex": 0,
            "transactionHash": "0x02",
            "planId": "0x01",
            "zhixuId": "demo",
            "orderId": "order-9",
            "registeredAt": "2026-04-27T00:00:00.000Z"
        }),
        json!({
            "eventName": "SignalSubmitted",
            "blockNumber": 3,
            "logIndex": 0,
            "transactionHash": "0x03",
            "planId": "0x01",
            "zhixuId": "demo",
            "orderId": "order-9",
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
            "orderId": "order-9",
            "hookId": "target.entry#DOCK_ENTER",
            "stageIdentifier": "target.entry",
            "hookName": "DOCK_ENTER"
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
        "dock birth with a recorded fact must replay without mismatch"
    );
    let order = &result["state"]["orders"]["0x01::order-9"];
    assert_eq!(
        order["hookStatuses"]["target.entry#DOCK_ENTER"]["readyEmitted"],
        true
    );
    assert_eq!(order["materializedStages"]["target.entry"], true);
}

#[test]
fn ordinary_signals_do_not_advance_order_trigger_hooks() {
    // 镜像合约 _evaluateAffectedHooks 的 evaluateOrderTriggerHooks 出生
    // 求值范围：order-trigger（mint/dock）hook 只在出生事务内求值。订单
    // 由事实 K1 出生（首条信号 = 出生通道，H1 照常 Ready）；随后普通提
    // 交另一出生线事实 K2——链上已不再对 H2 求值，oracle 若仍推 H2
    // Ready 会产出凭空 observed（阶段也被错误物化）。
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
        // 出生事务：首条信号是出生事实，H1 求值照常（出生通道）。
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
        // 普通事务：K2 是另一条出生线的事实。链上跳过 H2，不发任何
        // 观察——oracle 也不得推 H2。
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
fn order_link_born_orders_take_ordinary_signals_only() {
    // order-link 出生不 _recordSignal：OrderTriggered 事务内没有信号，
    // 订单后续收到的第一条信号已是普通信号——不是出生通道，不得推进
    // 其它 mint hook。出生断言本身仍由 mint-only 推导门（HookReady 反
    // 推 H1）承载。
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
            "orderId": "order-y",
            "registeredAt": "2026-04-27T00:00:00.000Z"
        }),
        // order-link 出生：OrderTriggered + HookReady 同事务、无信号。
        json!({
            "eventName": "OrderTriggered",
            "blockNumber": 3,
            "logIndex": 0,
            "transactionHash": "0x03",
            "planId": "0x01",
            "orderId": "order-y",
            "triggerStageId": "birth.one",
            "triggerHookId": "birth.one#ENTER",
            "sourceId": "0x30",
            "signalId": "0x40",
            "submitter": "relayer"
        }),
        json!({
            "eventName": "HookReady",
            "blockNumber": 3,
            "logIndex": 1,
            "transactionHash": "0x03",
            "planId": "0x01",
            "zhixuId": "demo",
            "orderId": "order-y",
            "hookId": "birth.one#ENTER",
            "stageIdentifier": "birth.one",
            "hookName": "ENTER"
        }),
        // 订单的首条信号（K2）在 OrderTriggered 事务之外到达：普通信
        // 号，H2 不被推进；链上也不发观察。
        json!({
            "eventName": "SignalSubmitted",
            "blockNumber": 4,
            "logIndex": 0,
            "transactionHash": "0x04",
            "planId": "0x01",
            "zhixuId": "demo",
            "orderId": "order-y",
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
        "order-link born orders take ordinary signals only: {}",
        result["mismatches"]
    );
    let order = &result["state"]["orders"]["0x01::order-y"];
    assert_eq!(order["hookStatuses"]["birth.two#ENTER"], json!(null));
    assert_eq!(order["materializedStages"]["birth.two"], json!(null));
}

#[test]
fn wait_reemission_on_due_at_only_change_pairs_cleanly() {
    // 合约 _evaluateHook 对 wait→wait 仅 dueAt 变化也重复发
    // HookStatusChanged（previousDueAt != nextDueAt 即发）：OR 最早到期
    // 分支后到会把 due 前移。expected 吸收门只吸收"同 status 同 dueAt"
    // 的重复，observed 在 dueAt 变化时照常重发——两条 wait 观察按到达
    // 序配对，不得误报 semantic-mismatch。
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
        // 第二条事实在 00:00:10 到达：OR 最早到期前移到 00:00:15——
        // 仅 dueAt 变化，合约重复发 wait 观察。
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
    // 对齐合约 pokeTimer：非 wait / 未到期的 poke 直接跳过，不产生
    // unexpected-observed 假阳性；到期后重评照常发生。
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
    // 首次观察是 wait（信号到达），未到期 poke 不产生任何观察，到期
    // poke 产生 HookReady——共两条。
    assert_eq!(observed.len(), 2, "observed: {observed:?}");
    assert_eq!(observed[0]["eventName"], "HookStatusChanged");
    assert_eq!(observed[0]["status"], "wait");
    assert_eq!(observed[1]["eventName"], "HookReady");
    assert_eq!(result["mismatches"].as_array().map(Vec::len), Some(0));
}

#[test]
fn timer_poked_without_poked_at_fails_loudly() {
    // pokedAt 是 TimerPoked 的求值时钟：缺失/非串是结构性毒输入，整场
    // 回放响亮失败——跳过（或按未到期静默滤除）会把"事件存在但时钟
    // 不可知"吞成空洞。TS 镜像层按同口径抛错，此测试是两侧对齐的
    // 权威侧钉子。
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
                    // watcher 形态：order-trigger hook 禁 DELAY，出生钩
                    // 携 DELAY 会在注册门先炸，到不了 pokedAt 口径。
                    "orderTriggerKind": "none",
                    "emitReady": true,
                    "instructions": [
                        {"op": "SIGNAL", "signalKey": "0x50"},
                        {"op": "DELAY", "delaySeconds": 5}
                    ],
                }],
                "dependencyIndex": {"0x50": ["flow.pay#TIMEOUT"]}
            },
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
            "eventName": "TimerPoked",
            "blockNumber": 4,
            "logIndex": 0,
            "transactionHash": "0x04",
            "planId": "0x01",
            "zhixuId": "demo",
            "orderId": "order-1",
            "hookId": "flow.pay#TIMEOUT",
            "dueAt": "2026-04-27T00:00:05.000Z"
        }),
    ];
    let error = replay_chain_events(events, &ReplayOptions::default()).unwrap_err();
    assert!(
        error.to_string().contains("pokedAt must be a string"),
        "unexpected error: {error}"
    );
}

#[test]
fn rejects_not_without_operand() {
    let instructions = vec![json!({"op": "NOT"})];
    let error = evaluate_instructions(
        &OracleOrderState::default(),
        &instructions,
        "2026-04-27T00:00:00Z",
    )
    .unwrap_err();
    assert!(error.to_string().contains("NOT requires one operand"));
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
fn emit_ready_hook_materializes_stage_before_materialization() {
    // EMIT_READY hook 是 executor dispatch 边——阶段未物化时
    // 仍求值，Ready 时物化自身阶段并发 HookReady（普通 executor 阶段的
    // 标准形态：全部 receive hook emit-ready）。"未物化即跳过"只适用
    // 于 trigger hook。
    let mut order = OracleOrderState {
        zhixu_id: "demo".to_string(),
        order_id: "order-1".to_string(),
        ..OracleOrderState::default()
    };
    order.signals.insert(
        "0x50".to_string(),
        json!({"submittedAt": "2026-04-27T00:00:00.000Z"}),
    );

    let emit_ready_hook = json!({
        "hookId": "flow.execute#READY",
        "stageId": "flow.execute",
        "stageIdentifier": "flow.execute",
        "hookName": "READY",
        "orderTriggerKind": "none",
        "emitReady": true,
        "instructions": [{"op": "SIGNAL", "signalKey": "0x50"}]
    });
    assert!(
        !order.materialized_stages.contains_key("flow.execute"),
        "precondition: stage not yet materialized"
    );
    let observations = evaluate_hook(&mut order, &emit_ready_hook, "2026-04-27T00:00:00.000Z")
        .expect("emit-ready hook must evaluate before stage materialization");
    assert_eq!(observations.len(), 1);
    assert_eq!(observations[0]["eventName"], "HookReady");
    assert_eq!(observations[0]["hookId"], "flow.execute#READY");
    assert!(
        order.materialized_stages["flow.execute"],
        "emit-ready readiness must materialize its own stage"
    );

    // 阶段物化后，同阶段的纯 flags=0 watcher 获得求值资格。
    let watcher = json!({
        "hookId": "flow.execute#WATCH",
        "stageId": "flow.execute",
        "stageIdentifier": "flow.execute",
        "hookName": "WATCH",
        "orderTriggerKind": "none",
        "emitReady": false,
        "instructions": [{"op": "SIGNAL", "signalKey": "0x50"}]
    });
    let watcher_observations = evaluate_hook(&mut order, &watcher, "2026-04-27T00:00:00.000Z")
        .expect("watcher must evaluate once its stage materialized");
    assert!(
        watcher_observations.is_empty(),
        "flags=0 watcher emits nothing"
    );
    assert_eq!(order.hook_statuses["flow.execute#WATCH"].status, "ready");
}

#[test]
fn flags_zero_watcher_on_unmaterialized_stage_is_skipped() {
    // 防御纵深：编译器已拒绝该形态（不可物化阶段不得挂 receive hook）；
    // oracle 对漏网产物按合约 A3 修复口径跳过（不 revert、不观察）。
    let mut order = OracleOrderState {
        zhixu_id: "demo".to_string(),
        order_id: "order-1".to_string(),
        ..OracleOrderState::default()
    };
    order.signals.insert(
        "0x50".to_string(),
        json!({"submittedAt": "2026-04-27T00:00:00.000Z"}),
    );
    let watcher = json!({
        "hookId": "flow.ghost#WATCH",
        "stageId": "flow.ghost",
        "stageIdentifier": "flow.ghost",
        "hookName": "WATCH",
        "orderTriggerKind": "none",
        "emitReady": false,
        "instructions": [{"op": "SIGNAL", "signalKey": "0x50"}]
    });
    let observations = evaluate_hook(&mut order, &watcher, "2026-04-27T00:00:00.000Z")
        .expect("unmaterialized flags=0 watcher must be skipped, not an error");
    assert!(observations.is_empty());
    assert!(!order.hook_statuses.contains_key("flow.ghost#WATCH"));
}

#[test]
fn stage_materialized_event_backfills_materialization() {
    // StageMaterialized 事件被消费：链上物化事实回填 oracle 状态，后续
    // 依赖该阶段的 watcher 求值据此放行。
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
    // fail-closed：缺失 v2 必需字段（orderTriggerKind / emitReady）即
    // 结构性错误，不做隐式回退。
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
    // options 与外层信封同口径拒绝未知字段：拼错的键不得被静默吞成
    // 缺省语义。
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

/// 单 mint trigger hook 的最小 plan（按需复用的探针基底）。
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
    // dueAt 归一化：链上观察携带无毫秒渲染（…:10Z），oracle 产出毫秒
    // 渲染（…:10.000Z）——同一时刻不得误报 semantic-mismatch；时刻
    // 不同（…:11Z）必须照常 mismatch。
    let plan = single_hook_plan(
        "flow.start#TIMEOUT",
        json!([
            { "op": "SIGNAL", "signalKey": "0x50" },
            { "op": "DELAY", "delaySeconds": 10 }
        ]),
    );
    // mint trigger 禁 DELAY（合约编码门）——把探针改成 none/emitReady
    // 的 watcher 形态，避免把测试载体做成不可注册的 plan。
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

    // 同一时刻的重复 wait 观察（不同渲染）被吸收：expected 只留一条。
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

    // 时刻不同：照常 semantic-mismatch。
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
    // 配对契约：expected/observed 按 (planId, orderId, hookId) 分桶配对。
    // 两订单的 HookReady 到达序与 oracle 推导序相反（链上 order-2 的
    // 就绪先落块）——全局下标配对会误报 2 条 semantic-mismatch，
    // 分桶配对 0 条。
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
    // 链上 HookReady 与 oracle 推导序相反：order-2 的先到。
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
    // 编译器身份大小写敏感（Main/main 两个 stage 合法共存），回放侧
    // 分桶与逐字段比较同口径字节精确：仅大小写不同的 hookId 是两个
    // 独立实体，不得折叠进同一桶或互相配对。
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
    // 合约不产出 HookStatusChanged(status=init)（Init 是隐含初值，
    // 无观察语义）：携带该状态的输入事件被裁剪，不产生 expected、
    // 不参与比对——原生入口可直接喂，无需适配层预裁。
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
fn delay_without_positive_anchor_fails_loudly() {
    // 手工 plan 的 DELAY 叠无锚就绪（NOT(缺席信号) 的 value=true、
    // anchor_at=0）：求值期复验锚点不变量并响亮失败，不产出 1970 due
    // 的假 wait 观察（对齐合约 _validateHook 注册门的 hasPosAnchor）。
    let instructions = vec![
        json!({ "op": "SIGNAL", "signalKey": "0x50" }),
        json!({ "op": "NOT" }),
        json!({ "op": "DELAY", "delaySeconds": 10 }),
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
            .contains("DELAY requires a positively anchored operand"),
        "unexpected error: {error}"
    );
}

#[test]
fn per_key_hook_ids_evaluate_triggers_before_watchers_in_stable_order() {
    // 与合约 _evaluateAffectedHooks 的两遍扫描等值：同一事实键的
    // hookIds 先按 index 序扫 order-trigger，再按 index 序扫普通
    // watcher——stable 分区（W1, T, W2 → T, W1, W2），不是稳定排序
    // 之外的任意重排。
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
    // mint 出生推导对 stageId/stageIdentifier/hookName 缺失响亮失败
    // （与 evaluate_hook 对 stageId 的 ? 门口径一致）：unwrap_or_default
    // 会把缺失吞成空串并物化 "" 键——畸形 plan 的链上断言被静默接受。
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
                    // stageId 缺失：其余结构字段在场，推导门必须报错。
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

    // stageIdentifier 缺失同口径。
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
    // 合约注册门镜像：order-trigger（mint/dock）hook 携 DELAY 在
    // commitPlan 即 revert InvalidInstruction（出生事实与订单创建同笔
    // 交易，Delay 必得 Wait，出生路径永久 InvalidTriggerHook）——链上
    // 不可注册的 plan 形态在回放输入里即结构性错误，不产软化 mismatch。
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

    // 基线：同指令集在 watcher（none）上合法——门只镜像 order-trigger
    // 的合约约束，不扩大到非 trigger hook。
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
    // 显式 Option 锚点：epoch 0（1970-01-01T00:00:00Z）提交的事实是
    // 真实锚点，DELAY 不再误报"结构性错误"，照常产出 wait 观察；
    // 无锚伪就绪（NOT(缺席信号)）依旧被拒。
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

    // 无锚伪就绪 + DELAY：结构性错误照旧（哨兵区分不改变该拒绝面）。
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

#[test]
fn non_positive_delay_seconds_is_rejected_at_replay_decode() {
    // 回放解码镜像合约注册门（delaySeconds == 0 revert
    // InvalidInstruction）与在线入口（uvp-hook-dsl 解码层 positive 门）：
    // 手工 plan 的 0 值延时恒等、负值回拨锚点，都是确定性非法输入。
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
    // 指令流入口同口径：SIGNAL + DELAY(0) 在 evaluate_instructions
    // 响亮失败，不产出"恒等延时"的观察。
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
    // due_at 的显式存在性口径：epoch 0 的等待期限必须照常渲染并让
    // poke 资格闸放行——旧 0 哨兵把 due 折成 None，等待行永久脱离
    // poke 认领。锚点取 1969-12-31T23:59:55Z + 5s 延时 → due 恰为
    // epoch 0。
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
    // wait 观察携带渲染后的 epoch-0 dueAt（不是缺席）。
    let observed = result["observed"].as_array().unwrap();
    let wait = observed
        .iter()
        .find(|item| item["status"] == "wait")
        .expect("wait observation with the epoch-0 dueAt");
    assert_eq!(wait["dueAt"], "1970-01-01T00:00:00.000Z");
    // epoch 0 的 poke 合资格：重评后就绪。
    assert!(
        observed.iter().any(|item| item["eventName"] == "HookReady"),
        "poke at epoch 0 must make the hook ready: {observed:?}"
    );
    let order = &result["state"]["orders"]["0x01::order-1"];
    assert_eq!(order["hookStatuses"]["flow.start#WAIT"]["status"], "ready");
}

// ------------------------------------------------------------------
// 链上注册门镜像（30d 上限 / 根正锚 / NOT 裸操作数 / 深度 120）、
// 重复 OrderRegistered 吸收、非整数 blockNumber、dueAt 渲染响亮失败。
// ------------------------------------------------------------------

/// 单 watcher 钩子的最小 plan（注册门探针基底：orderTriggerKind=none）。
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
    // 合约 MAX_HOOK_DELAY_SECONDS = 30 days = 2592000s：超限在 commitPlan
    // 即 revert HookDelayTooLong——"合约不可能的 plan"在回放注册门响亮
    // 失败，不产出远期 wait 的软化观察。
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

    // 边界值 2592000 恰在限内：注册放行。
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
    // 纯否定条件（~A）：value=true 时 anchorAt 无源，合约注册边界按
    // hasPosAnchor[0] 拒绝——镜像同口径。
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
fn not_on_composite_operand_is_rejected_at_registration() {
    // ~(A&B)：NOT 的操作数必须是裸 SIGNAL——组合否定的取消/锚点语义与
    // 编译器产物形态分叉，合约 _validateHook 注册边界拒绝。
    let plan = watcher_plan(
        "flow.pay#COMPOSITE_NOT",
        json!([
            { "op": "SIGNAL", "signalKey": "0x50" },
            { "op": "SIGNAL", "signalKey": "0x51" },
            { "op": "AND", "arity": 2 },
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
            .contains("applies NOT to a non-bare-SIGNAL operand"),
        "{error}"
    );

    // 对照：NOT 直接作用于裸 SIGNAL 合法（A&~B 的负依赖形态）。指令集
    // 携带两个 SIGNAL 原子，dependencyIndex 须逐点镜像（真实编译产物按
    // 全部原子建索引）。
    let mut plan = watcher_plan(
        "flow.pay#GUARD",
        json!([
            { "op": "SIGNAL", "signalKey": "0x50" },
            { "op": "SIGNAL", "signalKey": "0x51" },
            { "op": "NOT" },
            { "op": "AND", "arity": 2 }
        ]),
    );
    plan["dependencyIndex"] = json!({ "0x50": ["flow.pay#GUARD"], "0x51": ["flow.pay#GUARD"] });
    replay_chain_events(
        vec![plan_registered_event(plan)],
        &ReplayOptions {
            sort: None,
            strict: Some(true),
        },
    )
    .expect("NOT over a bare SIGNAL is the compiler-produced negative-dependency shape");
}

#[test]
fn instruction_depth_cap_120_is_enforced_at_registration() {
    // 深度闸镜像 MAX_PARSE_DEPTH=120 / Go MaxASTDepth=120：逐槽计数
    // （SIGNAL=0，组合=操作数最大深度+1）。121 层嵌套超限拒绝，120 层
    // 恰在限内。
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
    // 绕过形态：instructions 全合法（SIGNAL 原子可求值、根含正锚），但
    // dependencyIndex 用不匹配的键挂该 hook。oracle 的求值范围由
    // dependencyIndex 反查决定——指令轨与索引错位时事件流回放产出零观察，
    // observed/mismatches 全 0 仍 ok:true（空洞假 PASS）。镜像合约
    // HookDependencyKeyMismatch 门，注册期响亮失败。
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

    // 反向错位：SIGNAL 原子不在索引内——该事实到达永不触发求值，hook
    // 永久 Init 且零告警，同样必须响亮失败。
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

    // dependencyIndex 整体缺失：求值范围反查恒为空，一切信号零观察——
    // "合约不可能的 plan"（注册必然写入索引），按结构错误拒绝。
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

    // 对照：指令原子键与索引逐点一致的 plan 照常注册（watcher_plan 基底
    // 即该形态）。
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
    // 镜像合约 SilentOrderTriggerHook 门：order-trigger hook 必须携带
    // emitReady。沉默 trigger 物化阶段但不发 HookReady——该形态会让
    // oracle 的 expected 观察与链上事件流系统性分叉，注册边界拒绝，
    // 不留到求值期。
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

    // 对照：非 trigger 的沉默 watcher（emitReady=false）合法——物化门由
    // 阶段物化状态承担，不发 HookReady 是其正常形态。
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
    // 合约对重复注册 revert OrderAlreadyRegistered：订单在链上恰注册一次，
    // 事件流中的重复 OrderRegistered 是投影重放——吸收并保留已积累状态，
    // 不得清空重放（清空会把已验证的出生事实吞成空洞）。
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
    // 重发的 OrderRegistered（同键同身份）落在事实之后。
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

    // 同键不同 zhixu：身份矛盾的事件流，响亮失败。
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
    // 排序键非整数（字符串/浮点/缺失）不得折 0 静默重排：排序前响亮
    // 报错，保住事件流的因果序判定。
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
    // 缺失字段同口径（value_i64 对缺失报 must be an integer）。
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
    // sort=false 时不消费排序键，非整数不在此门（调用方自报因果序）。
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
    // 渲染失败折 None 会把等待行变成无期限永久 wait（poke 资格闸按存在性
    // 判永不合资格）——确定性毒输入响亮失败。
    let error = render_due_at(8_210_866_176_000).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("refusing to fold to an undated permanent wait"),
        "{error}"
    );
    // epoch 0 是真实期限，照常渲染（既有口径不回退）。
    assert_eq!(render_due_at(0).unwrap(), "1970-01-01T00:00:00.000Z");
}

// ------------------------------------------------------------------
// Rust 编译产物（无指令轨）直连回放的空洞 PASS 断层。
// ------------------------------------------------------------------

#[test]
fn plan_without_instruction_track_fails_loudly_instead_of_hollow_pass() {
    // Rust 编译产物（uvp-core hook_plan）不携带 instructions（指令轨归
    // TS 编译器），dependencyIndex 的键也是 source::task.stage.signal 而
    // 非链上 signalKey——直连回放时链上信号找不到可求值钩子，旧行为是
    // observed=0/expected=0/mismatches=0 的空洞 PASS。注册门按合约
    // InvalidHook 同口径要求每个钩子携带非空 instructions，断层在
    // PlanRegistered 即响亮失败。
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

    // 空指令数组同罪（合约 InvalidHook：instructions.length == 0）。
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
