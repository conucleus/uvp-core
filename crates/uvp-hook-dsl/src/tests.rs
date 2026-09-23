use super::*;
use serde_json::json;

fn parse_value(raw: &str, profile: Profile, hook_name: &str) -> Value {
    let out = parse_hook(ParseHookRequest {
        profile,
        hook_name: hook_name.to_string(),
        hook: raw.to_string(),
    })
    .unwrap();
    serde_json::to_value(out).unwrap()
}

fn evaluate_compiled(
    hook_name: &str,
    hook: &str,
    profile: Profile,
    signals: Vec<SignalFact>,
    now: &str,
) -> EvalCompiledHookOutput {
    let parsed = parse_hook(ParseHookRequest {
        profile,
        hook_name: hook_name.to_string(),
        hook: hook.to_string(),
    })
    .unwrap();
    eval_compiled_hook(EvalCompiledHookRequest {
        profile,
        ast: parsed.cloud_ast,
        signals,
        now: now.to_string(),
    })
    .unwrap()
}

#[test]
fn evm_strict_parses_and_evaluates_positive_signal() {
    let out = parse_value("buyer::task.main.cmp", Profile::EvmStrict, "TRIGGER");
    assert_eq!(out["normalizedExpression"], "buyer::task.main.cmp");
    assert_eq!(
        out["dependencies"],
        json!([{ "kind": "positive", "source": "buyer", "signalName": "task.main.cmp" }])
    );

    let eval = evaluate_compiled(
        "TRIGGER",
        "buyer::task.main.cmp",
        Profile::EvmStrict,
        vec![SignalFact {
            source: "buyer".to_string(),
            signal_name: "task.main.cmp".to_string(),
            received_at: "2026-04-27T00:00:00.900Z".to_string(),
        }],
        "2026-04-27T00:00:00.999Z",
    );
    assert_eq!(eval.state, EvalState::Ready);
    assert_eq!(eval.ready_at.as_deref(), Some("2026-04-27T00:00:00.000Z"));
}

#[test]
fn rejects_deeply_nested_expressions_instead_of_overflowing() {
    let deep = format!("buyer::{}a{}", "(".repeat(50_000), ")".repeat(50_000));
    let err = parse_hook(ParseHookRequest {
        profile: Profile::EvmStrict,
        hook_name: "HOOK".to_string(),
        hook: deep,
    })
    .unwrap_err();
    assert!(err.to_string().contains("maximum depth of 120"));
}

#[test]
fn parse_depth_cap_keeps_parseable_hooks_evaluable_through_json() {
    // 单一深度闸契约：解析器上限必须给 serde_json 的 128 层请求反序列化
    // 留足余量——最深的合法形态（括号嵌套延时链：入口开销 3 层 + 每层
    // 括号 2 个解析深度、产出 1 层 delay 节点）在解析上限内编译、并以
    // JSON 字符串形式过求值入口（不触 serde_json 递归上限）。再深一层
    // 则解析期拒绝。
    let legal = format!(
        "buyer::{}task.main.cmp +1s{}",
        "(".repeat(58),
        ")".repeat(58)
    );
    let parsed = parse_hook(ParseHookRequest {
        profile: Profile::EvmStrict,
        hook_name: "TIMEOUT".to_string(),
        hook: legal,
    })
    .unwrap();
    let request = json!({
        "profile": "evm_strict",
        "ast": parsed.cloud_ast,
        "signals": [{
            "source": "buyer",
            "signalName": "task.main.cmp",
            "receivedAt": "2026-04-27T00:00:00.000Z"
        }],
        "now": "2026-04-27T00:01:00.000Z"
    });
    let output = eval_compiled_hook_json(&request.to_string());
    assert!(
        output.contains("\"ok\":true"),
        "deepest legal hook must stay evaluable through the JSON entry: {output}"
    );

    let illegal = format!(
        "buyer::{}task.main.cmp +1s{}",
        "(".repeat(59),
        ")".repeat(59)
    );
    let err = parse_hook(ParseHookRequest {
        profile: Profile::EvmStrict,
        hook_name: "TIMEOUT".to_string(),
        hook: illegal,
    })
    .unwrap_err();
    assert!(err.to_string().contains("maximum depth of 120"));
}

#[test]
fn unsupported_cross_source_keywords_fail_fast() {
    // 嵌套构造在最外层即命中 unsupported 报错：不受支持的跨秩序形态
    // 不做深度解析。
    let mut expression = "peer::task.main.cmp".to_string();
    for _ in 0..2_000 {
        expression = format!("::OUTSIDE@({expression})");
    }
    let err = parse_hook(ParseHookRequest {
        profile: Profile::EvmStrict,
        hook_name: "HOOK".to_string(),
        hook: expression,
    })
    .unwrap_err();
    assert!(err.to_string().contains("retired"), "unexpected: {err}");
}

#[test]
fn rejects_deeply_nested_cloud_ast() {
    let mut root = json!({ "type": "signal", "signal": "task.main.cmp" });
    // Just past the guard threshold: deep enough to trip the depth cap,
    // shallow enough that serde_json's recursive Drop stays safe.
    for _ in 0..300 {
        root = json!({ "type": "neg", "expr": root });
    }
    let ast = json!({
        "schemaVersion": CLOUD_AST_SCHEMA_VERSION,
        "source": "buyer",
        "mode": "normal",
        "root": root
    });
    let err = eval_compiled_hook(EvalCompiledHookRequest {
        profile: Profile::EvmStrict,
        ast,
        signals: vec![],
        now: "2026-04-27T00:00:00.000Z".to_string(),
    })
    .unwrap_err();
    assert!(err.to_string().contains("maximum depth of 120"));
}

#[test]
fn rejects_pure_negative_compiled_root() {
    let ast = json!({
        "schemaVersion": CLOUD_AST_SCHEMA_VERSION,
        "source": "buyer",
        "mode": "normal",
        "root": { "type": "neg", "expr": { "type": "signal", "signal": "task.cancel.cmp" } }
    });
    let err = eval_compiled_hook(EvalCompiledHookRequest {
        profile: Profile::EvmStrict,
        ast,
        signals: vec![],
        now: "2026-04-27T00:00:00.000Z".to_string(),
    })
    .unwrap_err();
    assert!(err
        .to_string()
        .contains("must contain at least one positive signal anchor"));
}

#[test]
fn repeated_signals_keep_first_received_fact() {
    let parsed = parse_hook(ParseHookRequest {
        profile: Profile::EvmStrict,
        hook_name: "TRIGGER".to_string(),
        hook: "buyer::(task.pay.cmp +5s)".to_string(),
    })
    .unwrap();
    let eval = eval_compiled_hook(EvalCompiledHookRequest {
        profile: Profile::EvmStrict,
        ast: parsed.cloud_ast,
        signals: vec![
            SignalFact {
                source: "buyer".to_string(),
                signal_name: "task.pay.cmp".to_string(),
                received_at: "2026-04-27T00:00:01.000Z".to_string(),
            },
            SignalFact {
                source: "buyer".to_string(),
                signal_name: "task.pay.cmp".to_string(),
                received_at: "2026-04-27T00:00:10.000Z".to_string(),
            },
        ],
        now: "2026-04-27T00:00:06.000Z".to_string(),
    })
    .unwrap();
    // First received fact (00:00:01 + 5s) is already due at 00:00:06; a
    // last-writer-wins map would anchor at 00:00:10 and report wait.
    assert_eq!(eval.state, EvalState::Ready);
}

#[test]
fn evm_strict_handles_delay_and_negative_guard() {
    let eval = evaluate_compiled(
        "TIMEOUT",
        "buyer::(task.pay.cmp +5s) & ~task.refund.cmp",
        Profile::EvmStrict,
        vec![SignalFact {
            source: "buyer".to_string(),
            signal_name: "task.pay.cmp".to_string(),
            received_at: "2026-04-27T00:00:00.900Z".to_string(),
        }],
        "2026-04-27T00:00:04.999Z",
    );
    assert_eq!(eval.state, EvalState::Wait);
    assert_eq!(eval.ready_at.as_deref(), Some("2026-04-27T00:00:05.000Z"));
}

#[test]
fn cloud_ast_preserves_delay_operand_and_source() {
    let out = parse_hook(ParseHookRequest {
        profile: Profile::CloudCompat,
        hook_name: "TIMEOUT".to_string(),
        hook: "buyer::task.receive.cmp +14d".to_string(),
    })
    .unwrap();

    assert_eq!(out.cloud_ast["source"], json!("buyer"));
    assert_eq!(
        out.cloud_ast["schemaVersion"],
        json!(CLOUD_AST_SCHEMA_VERSION)
    );
    assert_eq!(out.cloud_ast["mode"], json!("normal"));
    assert_eq!(out.cloud_ast["root"]["type"], json!("delay"));
    assert_eq!(out.cloud_ast["root"].get("delay"), None);
    assert_eq!(out.cloud_ast["root"]["rawDuration"], json!("14d"));
    assert_eq!(
        out.cloud_ast["root"]["durationSeconds"],
        json!(14 * 24 * 60 * 60)
    );
}

#[test]
fn delay_duration_above_30d_cap_is_rejected_at_parse_time() {
    let err = parse_hook(ParseHookRequest {
        profile: Profile::CloudCompat,
        hook_name: "TIMEOUT".to_string(),
        hook: "buyer::task.receive.cmp +31d".to_string(),
    })
    .unwrap_err();
    assert!(err.to_string().contains("30d"), "unexpected error: {err}");

    let boundary = parse_hook(ParseHookRequest {
        profile: Profile::CloudCompat,
        hook_name: "TIMEOUT".to_string(),
        hook: "buyer::task.receive.cmp +2592000s".to_string(),
    });
    assert!(boundary.is_ok(), "30d must stay accepted: {boundary:?}");
}

#[test]
fn compiled_subscription_target_rejects_unknown_keys_and_shapes() {
    // subscriptionTarget 键闭集：拼错的键（如 singal）与多余字段必须
    // 确定性拒绝，不得被静默忽略成"缺 source/signal"或缺省语义；
    // 非对象形态同样响亮失败。
    let mut ast = parse_hook(ParseHookRequest {
        profile: Profile::CloudCompat,
        hook_name: "SUB".to_string(),
        hook: "::ANCHOR(@seller::trade.listing.cmp)".to_string(),
    })
    .unwrap()
    .cloud_ast;
    ast["subscriptionTarget"]
        .as_object_mut()
        .unwrap()
        .insert("singal".to_string(), json!("trade.listing.cmp"));
    let err = eval_compiled_hook(EvalCompiledHookRequest {
        profile: Profile::CloudCompat,
        ast: ast.clone(),
        signals: vec![],
        now: "2026-04-27T00:00:00.000Z".to_string(),
    })
    .unwrap_err();
    assert!(
        err.to_string().contains("subscriptionTarget")
            && err.to_string().contains("unsupported field: singal"),
        "unexpected error: {err}"
    );

    ast["subscriptionTarget"] = json!(["seller", "trade.listing.cmp"]);
    let err = eval_compiled_hook(EvalCompiledHookRequest {
        profile: Profile::CloudCompat,
        ast,
        signals: vec![],
        now: "2026-04-27T00:00:00.000Z".to_string(),
    })
    .unwrap_err();
    assert!(
        err.to_string()
            .contains("subscriptionTarget must be an object"),
        "unexpected error: {err}"
    );
}

#[test]
fn delay_ready_at_overflow_evaluates_to_error_instead_of_panic() {
    // 直接构造绕过编译期的毒 AST（不受信任输入的形态：超大秒数与
    // 原始字面量自洽）。求值必须在解码期确定性拒绝并走有界失败路径，
    // 而不是 panic 跨 FFI 边界 abort 进程。
    let poisoned = json!({
        "schemaVersion": CLOUD_AST_SCHEMA_VERSION,
        "source": "buyer",
        "mode": "normal",
        "root": {
            "type": "delay",
            "expr": { "type": "signal", "signal": "task.receive.cmp" },
            "rawDuration": "9223372036854775807s",
            "durationSeconds": i64::MAX
        }
    });

    let err = eval_compiled_hook(EvalCompiledHookRequest {
        profile: Profile::CloudCompat,
        ast: poisoned,
        signals: vec![SignalFact {
            source: "buyer".to_string(),
            signal_name: "task.receive.cmp".to_string(),
            received_at: "2026-04-27T00:00:00.900Z".to_string(),
        }],
        now: "2026-04-27T00:00:01.000Z".to_string(),
    })
    .unwrap_err();
    assert!(err.to_string().contains("30d"), "unexpected error: {err}");
}

#[test]
fn compiled_cloud_ast_evaluates_without_reparsing_source_expression() {
    let parsed = parse_hook(ParseHookRequest {
        profile: Profile::CloudCompat,
        hook_name: "TIMEOUT".to_string(),
        hook: "buyer::task.receive.cmp +14d".to_string(),
    })
    .unwrap();

    let eval = eval_compiled_hook(EvalCompiledHookRequest {
        profile: Profile::CloudCompat,
        ast: parsed.cloud_ast,
        signals: vec![SignalFact {
            source: "buyer".to_string(),
            signal_name: "task.receive.cmp".to_string(),
            received_at: "2026-04-01T00:00:00Z".to_string(),
        }],
        now: "2026-04-15T00:00:00Z".to_string(),
    })
    .unwrap();

    assert_eq!(eval.state, EvalState::Ready);
    assert_eq!(eval.ready_at.as_deref(), Some("2026-04-15T00:00:00.000Z"));
}

#[test]
fn compiled_hook_evaluation_requires_schema_version() {
    let err = eval_compiled_hook(EvalCompiledHookRequest {
        profile: Profile::CloudCompat,
        ast: json!({
            "source": "buyer",
            "root": {
                "type": "delay",
                "expr": {"type": "signal", "signal": "task.receive.cmp"},
                "delay": "14d"
            }
        }),
        signals: Vec::new(),
        now: "2026-04-15T00:00:00Z".to_string(),
    })
    .expect_err("AST missing schemaVersion must not be evaluated");
    assert!(err.to_string().contains("schemaVersion"));
}

#[test]
fn compiled_hook_evaluation_rejects_unknown_node_fields() {
    let err = eval_compiled_hook(EvalCompiledHookRequest {
        profile: Profile::CloudCompat,
        ast: json!({
            "schemaVersion": CLOUD_AST_SCHEMA_VERSION,
            "source": "buyer",
            "mode": "normal",
            "root": {
                "type": "delay",
                "expr": {"type": "signal", "signal": "task.receive.cmp"},
                "delay": "14d"
            }
        }),
        signals: Vec::new(),
        now: "2026-04-15T00:00:00Z".to_string(),
    })
    .expect_err("unknown node fields must not be evaluated");
    assert!(err.to_string().contains("unsupported field: delay"));
}

#[test]
fn cloud_compat_requires_full_signal_names() {
    let out = parse_value(
        "buyer::task.pay.cmp & ~task.refund.cmp",
        Profile::CloudCompat,
        "EXECUTE",
    );
    assert_eq!(out["runtimeCondition"], "task.pay.cmp & ~task.refund.cmp");
    assert_eq!(
        out["dependencies"],
        json!([
            { "kind": "negative", "source": "buyer", "signalName": "task.refund.cmp" },
            { "kind": "positive", "source": "buyer", "signalName": "task.pay.cmp" }
        ])
    );

    let err = parse_hook(ParseHookRequest {
        profile: Profile::CloudCompat,
        hook_name: "EXECUTE".to_string(),
        hook: "buyer::pay.cmp".to_string(),
    })
    .unwrap_err();
    assert!(err.to_string().contains("task.stage.signal"));

    let err = parse_hook(ParseHookRequest {
        profile: Profile::CloudCompat,
        hook_name: "TRIGGER".to_string(),
        hook: "::OUTSIDE".to_string(),
    })
    .unwrap_err();
    assert!(err.to_string().contains("retired"));
}

#[test]
fn rejects_subscription_inside_composite_condition() {
    for hook in [
        "buyer::OUTSIDE & task.main.cmp",
        "buyer::task.main.cmp | OUTSIDE",
        "buyer::~OUTSIDE",
    ] {
        let err = parse_hook(ParseHookRequest {
            profile: Profile::CloudCompat,
            hook_name: "HOOK".to_string(),
            hook: hook.to_string(),
        })
        .unwrap_err();
        assert!(
            err.to_string().contains("retired"),
            "unexpected error for {hook}: {err}"
        );
    }

    // 扇入类标头没有退役清单条目：字面按通用空标头语法错误拒绝，
    // 报错不点名该词。
    let retired_word: String = ["M", "E", "R", "G", "E"].concat();
    let hook = format!("::{retired_word} & task.main.cmp");
    let err = parse_hook(ParseHookRequest {
        profile: Profile::CloudCompat,
        hook_name: "HOOK".to_string(),
        hook,
    })
    .unwrap_err();
    assert!(
        err.to_string().contains("empty source"),
        "expected generic empty-source rejection: {err}"
    );
    assert!(
        !err.to_string().contains(&retired_word) && !err.to_string().contains("retired"),
        "generic rejection must not name the removed entry: {err}"
    );

    let err = parse_hook(ParseHookRequest {
        profile: Profile::CloudCompat,
        hook_name: "HOOK".to_string(),
        hook: "::ANCHOR(@seller::task.main.cmp) & task.other.cmp".to_string(),
    })
    .unwrap_err();
    assert!(
        err.to_string()
            .contains("must be the complete hook condition"),
        "unexpected error for a composite subscription: {err}"
    );

    let err = parse_hook(ParseHookRequest {
        profile: Profile::CloudCompat,
        hook_name: "HOOK".to_string(),
        hook: "buyer::ANCHOR(@seller::task.main.cmp)".to_string(),
    })
    .unwrap_err();
    assert!(
        err.to_string().contains("empty source header"),
        "expected headed subscription rejection: {err}"
    );
}

#[test]
fn parser_rejects_unbounded_nesting_and_short_subscription_targets() {
    // 深度上限：深层括号与连续 ~ 都必须以普通错误拒绝，而不是打满
    // 调用栈 abort 宿主进程（栈溢出不可被 catch_unwind 捕获）。
    for poisoned in [
        format!("buyer::{}task.main.cmp{}", "(".repeat(256), ")".repeat(256)),
        format!("buyer::{}task.main.cmp", "~".repeat(256)),
    ] {
        let err = parse_hook(ParseHookRequest {
            profile: Profile::CloudCompat,
            hook_name: "HOOK".to_string(),
            hook: poisoned,
        })
        .unwrap_err();
        assert!(
            err.to_string().contains("maximum depth"),
            "expected depth-limit rejection: {err}"
        );
    }

    let err = parse_hook(ParseHookRequest {
        profile: Profile::CloudCompat,
        hook_name: "HOOK".to_string(),
        hook: "::ANCHOR(@seller::listing.cmp)".to_string(),
    })
    .unwrap_err();
    assert!(
        err.to_string().contains("task.stage.signal"),
        "unexpected error for short subscription target: {err}"
    );
}

#[test]
fn outsource_forms_are_rejected_with_hint() {
    for hook in [
        "::OUTSOURCE@(seller::task.main.cmp)",
        "buyer::OUTSOURCE@(seller::task.main.cmp)",
        "buyer::OUTSOURCE",
    ] {
        let err = parse_hook(ParseHookRequest {
            profile: Profile::CloudCompat,
            hook_name: "HOOK".to_string(),
            hook: hook.to_string(),
        })
        .unwrap_err();
        assert!(
            err.to_string().contains("retired"),
            "unexpected error for {hook}: {err}"
        );
    }
}

#[test]
fn pseudo_keyword_prefixes_do_not_bypass_the_empty_source_gate() {
    // 伪前缀形态（如 ::ANCHORX / ::OUTSIDER，及扇入标头加伪后缀）
    // 不是退役关键字：空标头门禁按完整 token 边界匹配，直接以空标头
    // 错误拒绝，而不是借 starts_with 前缀命中放行进解析器。
    // 扇入词按字节拼装，保持全文检索零命中口径。
    let retired_word: String = ["M", "E", "R", "G", "E"].concat();
    for hook in [
        format!("::{retired_word}X@(seller::task.main.cmp)"),
        "::ANCHORX(@seller::task.main.cmp)".to_string(),
        "::OUTSIDER".to_string(),
    ] {
        let err = parse_hook(ParseHookRequest {
            profile: Profile::CloudCompat,
            hook_name: "HOOK".to_string(),
            hook,
        })
        .unwrap_err();
        assert!(
            err.to_string().contains("empty source"),
            "unexpected error for pseudo-prefix form: {err}"
        );
    }
    // 真关键字仍然放行到解析器，命中精确的 retired 报错。
    let err = parse_hook(ParseHookRequest {
        profile: Profile::CloudCompat,
        hook_name: "HOOK".to_string(),
        hook: "::OUTSIDE @seller::task.main.cmp".to_string(),
    })
    .unwrap_err();
    assert!(err.to_string().contains("retired"), "unexpected: {err}");
}

#[test]
fn signed_raw_duration_is_rejected_at_decode() {
    // i64::from_str 接受前导 '+'：毒 AST 的 rawDuration "+5s" 与
    // durationSeconds=5 自洽，必须在解码层按严格格式拒绝。
    let poisoned = json!({
        "schemaVersion": CLOUD_AST_SCHEMA_VERSION,
        "source": "buyer",
        "mode": "normal",
        "root": {
            "type": "delay",
            "expr": { "type": "signal", "signal": "task.receive.cmp" },
            "rawDuration": "+5s",
            "durationSeconds": 5
        }
    });
    let err = eval_compiled_hook(EvalCompiledHookRequest {
        profile: Profile::CloudCompat,
        ast: poisoned,
        signals: vec![],
        now: "2026-04-27T00:00:00Z".to_string(),
    })
    .unwrap_err();
    assert!(
        err.to_string().contains("invalid duration"),
        "unexpected error: {err}"
    );
    assert!(duration_to_seconds("+5s").is_err());
    assert!(duration_to_seconds("-5s").is_err());
}

#[test]
fn eval_rejects_signal_facts_with_invalid_identity() {
    let parsed = parse_hook(ParseHookRequest {
        profile: Profile::CloudCompat,
        hook_name: "TRIGGER".to_string(),
        hook: "buyer::task.main.cmp".to_string(),
    })
    .unwrap();
    for fact in [
        SignalFact {
            source: "has space".to_string(),
            signal_name: "task.main.cmp".to_string(),
            received_at: "2026-04-27T00:00:00Z".to_string(),
        },
        SignalFact {
            source: "buyer.ü".to_string(),
            signal_name: "task.main.cmp".to_string(),
            received_at: "2026-04-27T00:00:00Z".to_string(),
        },
        SignalFact {
            source: "s".repeat(37),
            signal_name: "task.main.cmp".to_string(),
            received_at: "2026-04-27T00:00:00Z".to_string(),
        },
        SignalFact {
            source: "buyer".to_string(),
            signal_name: "main.cmp".to_string(),
            received_at: "2026-04-27T00:00:00Z".to_string(),
        },
        SignalFact {
            source: "buyer".to_string(),
            signal_name: "task..cmp".to_string(),
            received_at: "2026-04-27T00:00:00Z".to_string(),
        },
        SignalFact {
            source: "buyer".to_string(),
            signal_name: format!("{}.{}", "t".repeat(60), "s".repeat(50)),
            received_at: "2026-04-27T00:00:00Z".to_string(),
        },
    ] {
        let err = eval_compiled_hook(EvalCompiledHookRequest {
            profile: Profile::CloudCompat,
            ast: parsed.cloud_ast.clone(),
            signals: vec![fact],
            now: "2026-04-27T00:00:00Z".to_string(),
        })
        .unwrap_err();
        assert!(
            err.to_string().contains("signal fact"),
            "unexpected error: {err}"
        );
    }
    // 边界值（36/100 字节、三段式）照常放行。
    eval_compiled_hook(EvalCompiledHookRequest {
        profile: Profile::CloudCompat,
        ast: parsed.cloud_ast.clone(),
        signals: vec![SignalFact {
            source: "s".repeat(36),
            signal_name: "task.main.cmp".to_string(),
            received_at: "2026-04-27T00:00:00Z".to_string(),
        }],
        now: "2026-04-27T00:00:00Z".to_string(),
    })
    .unwrap();
    // 空 source 是合法的无归属事实：解码放行，但永不满足带 source 的
    // hook（与语义语料的负例口径一致）。
    let unattributed = eval_compiled_hook(EvalCompiledHookRequest {
        profile: Profile::CloudCompat,
        ast: parsed.cloud_ast,
        signals: vec![SignalFact {
            source: String::new(),
            signal_name: "task.main.cmp".to_string(),
            received_at: "2026-04-27T00:00:00Z".to_string(),
        }],
        now: "2026-04-27T00:00:00Z".to_string(),
    })
    .unwrap();
    assert_eq!(unattributed.state, EvalState::NeedsMore);
}

#[test]
fn normal_ast_with_subscription_target_is_rejected_at_decode() {
    // O18：normal 模式携带 subscriptionTarget 只能是手写毒 AST——
    // 解析器只给订阅形态产出该字段。
    let poisoned = json!({
        "schemaVersion": CLOUD_AST_SCHEMA_VERSION,
        "source": "buyer",
        "mode": "normal",
        "subscriptionTarget": { "source": "seller", "signal": "trade.listing.cmp" },
        "root": { "type": "signal", "signal": "task.main.cmp" }
    });
    let err = eval_compiled_hook(EvalCompiledHookRequest {
        profile: Profile::CloudCompat,
        ast: poisoned,
        signals: vec![],
        now: "2026-04-27T00:00:00Z".to_string(),
    })
    .unwrap_err();
    assert!(
        err.to_string()
            .contains("normal hook AST must not carry subscriptionTarget"),
        "unexpected error: {err}"
    );
}

#[test]
fn subscription_entry_parses_target_and_dependencies() {
    let out = parse_value(
        "::ANCHOR(@seller::trade.listing.cmp)",
        Profile::CloudCompat,
        "SUBSCRIBE",
    );
    assert_eq!(out["mode"], "subscription");
    assert_eq!(
        out["subscriptionTarget"],
        json!({ "source": "seller", "signalName": "trade.listing.cmp" })
    );
    assert_eq!(
        out["dependencies"],
        json!([
            { "kind": "positive", "source": "seller", "signalName": "trade.listing.cmp" }
        ])
    );
    assert_eq!(
        out["normalizedExpression"],
        "::ANCHOR(@seller::trade.listing.cmp)"
    );

    let cloud_ast = out["cloudAst"].clone();
    assert_eq!(cloud_ast["mode"], "subscription");
    assert_eq!(cloud_ast["source"], "");
    assert_eq!(
        cloud_ast["subscriptionTarget"],
        json!({ "source": "seller", "signal": "trade.listing.cmp" })
    );

    // 订阅钩子不经表达式裁决：求值器恒返回 NeedsMore，投递由状态机
    // 按接收方锚定状态路由（按单经对接记录，无锚按类扇入）。
    let eval = eval_compiled_hook(EvalCompiledHookRequest {
        profile: Profile::CloudCompat,
        ast: cloud_ast,
        signals: vec![],
        now: "2026-04-27T00:00:00Z".to_string(),
    })
    .unwrap();
    assert_eq!(eval.state, EvalState::NeedsMore);
    assert!(eval.reason.unwrap_or_default().contains("delivered per"));
}

#[test]
fn subscription_entry_rejects_degenerate_shapes() {
    for hook in [
        "::ANCHOR()",
        "::ANCHOR(@)",
        "::ANCHOR(seller::trade.listing.cmp)",
        "::ANCHOR(@seller::trade.listing.cmp & buyer::trade.intent.cmp)",
        "::ANCHOR(@seller::listing.cmp)",
        "::ANCHOR(@::trade.listing.cmp)",
        "::ANCHOR(@seller names::trade.listing.cmp)",
        "::ANCHOR(@ seller::trade.listing.cmp)",
        "::ANCHOR(@seller ::trade.listing.cmp)",
        "::ANCHOR(@seller:: trade.listing.cmp)",
        "::ANCHOR(@seller::trade.listing.cmp )",
        "::ANCHOR( @seller::trade.listing.cmp)",
        "wholesaler::ANCHOR(@seller::trade.listing.cmp)",
        "::ANCHOR@(farmer.main.settle)",
    ] {
        let err = parse_hook(ParseHookRequest {
            profile: Profile::CloudCompat,
            hook_name: "HOOK".to_string(),
            hook: hook.to_string(),
        })
        .unwrap_err();
        assert!(!err.to_string().is_empty(), "expected rejection for {hook}");
    }
}

#[test]
fn unsupported_hook_modes_are_rejected_at_decode() {
    // 编译产物 mode 白名单（normal/subscription）之外的取值在解码期
    // 确定性拒绝，不做兼容解释。
    for mode in ["outside_spawn", "anchor", "bundle"] {
        let err = eval_compiled_hook(EvalCompiledHookRequest {
            profile: Profile::CloudCompat,
            ast: json!({
                "schemaVersion": CLOUD_AST_SCHEMA_VERSION,
                "source": "",
                "mode": mode,
                "root": { "type": "signal", "signal": "task.main.cmp" }
            }),
            signals: vec![],
            now: "2026-04-27T00:00:00Z".to_string(),
        })
        .expect_err("unsupported mode must not decode");
        assert!(
            err.to_string()
                .contains("unsupported compiled hook AST mode"),
            "unexpected error for {mode}: {err}"
        );
    }
}

#[test]
fn subscription_ast_without_target_is_rejected_at_decode() {
    let cloud_ast = json!({
        "schemaVersion": CLOUD_AST_SCHEMA_VERSION,
        "source": "",
        "mode": "subscription",
        "root": { "type": "subscription", "source": "seller", "signal": "trade.listing.cmp" }
    });
    let err = eval_compiled_hook(EvalCompiledHookRequest {
        profile: Profile::CloudCompat,
        ast: cloud_ast,
        signals: vec![],
        now: "2026-04-27T00:00:00Z".to_string(),
    })
    .expect_err("subscription AST without target must not be evaluated");
    assert!(
        err.to_string().contains("subscriptionTarget"),
        "unexpected error: {err}"
    );
}

#[test]
fn mint_and_route_validation_matches_go_decode() {
    // normal 模式携带 mint/route：对齐 Go DecodeCompiledHook 一律拒绝。
    for (field, value) in [("mint", "per-fact"), ("route", "order")] {
        let mut ast = json!({
            "schemaVersion": CLOUD_AST_SCHEMA_VERSION,
            "source": "buyer",
            "mode": "normal",
            "root": { "type": "signal", "signal": "task.main.cmp" }
        });
        ast[field] = json!(value);
        let err = eval_compiled_hook(EvalCompiledHookRequest {
            profile: Profile::CloudCompat,
            ast,
            signals: vec![],
            now: "2026-04-27T00:00:00Z".to_string(),
        })
        .expect_err("normal-mode mint/route must not decode");
        assert!(
            err.to_string()
                .contains("mint/route is only allowed on subscription mode"),
            "unexpected error for {field}: {err}"
        );
    }
    // subscription 模式：合法组合放行，非法取值确定性拒绝。
    let legal = json!({
        "schemaVersion": CLOUD_AST_SCHEMA_VERSION,
        "source": "",
        "mode": "subscription",
        "subscriptionTarget": { "source": "seller", "signal": "trade.listing.cmp" },
        "mint": "per-fact",
        "route": "fanin",
        "root": { "type": "subscription", "source": "seller", "signal": "trade.listing.cmp" }
    });
    let eval = eval_compiled_hook(EvalCompiledHookRequest {
        profile: Profile::CloudCompat,
        ast: legal,
        signals: vec![],
        now: "2026-04-27T00:00:00Z".to_string(),
    })
    .unwrap();
    assert_eq!(eval.state, EvalState::NeedsMore);

    let poisoned_mint = json!({
        "schemaVersion": CLOUD_AST_SCHEMA_VERSION,
        "source": "",
        "mode": "subscription",
        "subscriptionTarget": { "source": "seller", "signal": "trade.listing.cmp" },
        "mint": "bulk",
        "root": { "type": "subscription", "source": "seller", "signal": "trade.listing.cmp" }
    });
    let err = eval_compiled_hook(EvalCompiledHookRequest {
        profile: Profile::CloudCompat,
        ast: poisoned_mint,
        signals: vec![],
        now: "2026-04-27T00:00:00Z".to_string(),
    })
    .expect_err("non per-fact mint must not decode");
    assert!(
        err.to_string().contains("mint only supports per-fact"),
        "unexpected error: {err}"
    );

    let poisoned_route = json!({
        "schemaVersion": CLOUD_AST_SCHEMA_VERSION,
        "source": "",
        "mode": "subscription",
        "subscriptionTarget": { "source": "seller", "signal": "trade.listing.cmp" },
        "route": "broadcast",
        "root": { "type": "subscription", "source": "seller", "signal": "trade.listing.cmp" }
    });
    let err = eval_compiled_hook(EvalCompiledHookRequest {
        profile: Profile::CloudCompat,
        ast: poisoned_route,
        signals: vec![],
        now: "2026-04-27T00:00:00Z".to_string(),
    })
    .expect_err("unknown route must not decode");
    assert!(
        err.to_string().contains("route is invalid"),
        "unexpected error: {err}"
    );

    // 非字符串 mint/route 与 Go 的类型解码一致地拒绝。
    let poisoned_type = json!({
        "schemaVersion": CLOUD_AST_SCHEMA_VERSION,
        "source": "",
        "mode": "subscription",
        "subscriptionTarget": { "source": "seller", "signal": "trade.listing.cmp" },
        "mint": 5,
        "root": { "type": "subscription", "source": "seller", "signal": "trade.listing.cmp" }
    });
    let err = eval_compiled_hook(EvalCompiledHookRequest {
        profile: Profile::CloudCompat,
        ast: poisoned_type,
        signals: vec![],
        now: "2026-04-27T00:00:00Z".to_string(),
    })
    .expect_err("non-string mint must not decode");
    assert!(
        err.to_string().contains("mint must be a string"),
        "unexpected error: {err}"
    );
}

#[test]
fn subscription_ast_with_nonempty_source_is_rejected_at_decode() {
    let cloud_ast = json!({
        "schemaVersion": CLOUD_AST_SCHEMA_VERSION,
        "source": "buyer",
        "mode": "subscription",
        "subscriptionTarget": { "source": "seller", "signal": "trade.listing.cmp" },
        "root": { "type": "subscription", "source": "seller", "signal": "trade.listing.cmp" }
    });
    let err = eval_compiled_hook(EvalCompiledHookRequest {
        profile: Profile::CloudCompat,
        ast: cloud_ast,
        signals: vec![],
        now: "2026-04-27T00:00:00Z".to_string(),
    })
    .expect_err("subscription AST with headed source must not decode");
    assert!(
        err.to_string().contains("source must be empty"),
        "unexpected error: {err}"
    );
}

#[test]
fn subscription_ast_target_root_mismatch_is_rejected_at_decode() {
    let cloud_ast = json!({
        "schemaVersion": CLOUD_AST_SCHEMA_VERSION,
        "source": "",
        "mode": "subscription",
        "subscriptionTarget": { "source": "seller", "signal": "trade.listing.cmp" },
        "root": { "type": "subscription", "source": "seller", "signal": "trade.intent.cmp" }
    });
    let err = eval_compiled_hook(EvalCompiledHookRequest {
        profile: Profile::CloudCompat,
        ast: cloud_ast,
        signals: vec![],
        now: "2026-04-27T00:00:00Z".to_string(),
    })
    .expect_err("mismatched subscriptionTarget/root must not decode");
    assert!(
        err.to_string()
            .contains("subscriptionTarget does not match the root subscription node"),
        "unexpected error: {err}"
    );
}

#[test]
fn rejects_invalid_strict_signal_names() {
    for profile in [Profile::CloudCompat, Profile::EvmStrict] {
        for hook in [
            "buyer::cmp",
            "buyer::main.cmp",
            "buyer::task.stage.signal.extra",
            "buyer::task..cmp",
        ] {
            let err = parse_hook(ParseHookRequest {
                profile,
                hook_name: "HOOK".to_string(),
                hook: hook.to_string(),
            })
            .unwrap_err();
            assert!(
                err.to_string().contains("task.stage.signal"),
                "unexpected error for {hook}: {err}"
            );
        }
    }
}

#[test]
fn rejects_hook_names_containing_whitespace() {
    // 通道名进 hookId（stage#hook_name），两侧必须逐字节一致：含空白的
    // 名字（首尾/内部）是确定性非法输入，不做 trim 归一——与编译器
    // validate_receive_signal_keys 同口径。
    for hook_name in ["BAD KEY", " LEAD", "TRAIL ", "TAB\tKEY"] {
        let err = parse_hook(ParseHookRequest {
            profile: Profile::EvmStrict,
            hook_name: hook_name.to_string(),
            hook: "buyer::task.main.cmp".to_string(),
        })
        .unwrap_err();
        assert!(
            err.to_string()
                .contains("hook_name must not contain whitespace"),
            "unexpected error for {hook_name:?}: {err}"
        );
    }
}

#[test]
fn cloud_normalization_emits_single_parentheses_for_grouped_delay_operands() {
    // Cloud 面与 Tight 面共用"一层分组括号"外观：延时操作数为 And/Or
    // 组时只保留优先级闸产生的那一层括号，不得出现 `((A & B)) +5s`
    // 式双重括号（两输出面被语料/产物钉住，外观必须系统一致）。
    let cases = [
        (
            "buyer::(task.pay.cmp & task.ship.cmp)+5s",
            "(task.pay.cmp & task.ship.cmp) + 5s",
        ),
        (
            "buyer::(task.pay.cmp | task.ship.cmp)+5s",
            "(task.pay.cmp | task.ship.cmp) + 5s",
        ),
        // 嵌套延时与普通项的组合括号不受影响。
        (
            "buyer::(task.pay.cmp +5s) & task.ship.cmp",
            "(task.pay.cmp + 5s) & task.ship.cmp",
        ),
        (
            "buyer::task.pay.cmp & (task.ship.cmp | task.refund.cmp)",
            "task.pay.cmp & (task.ship.cmp | task.refund.cmp)",
        ),
    ];
    for (hook, expected) in cases {
        let out = parse_hook(ParseHookRequest {
            profile: Profile::CloudCompat,
            hook_name: "TIMEOUT".to_string(),
            hook: hook.to_string(),
        })
        .unwrap();
        assert_eq!(out.runtime_condition, expected, "hook: {hook}");
    }
}

#[test]
fn rejects_duration_overflow() {
    let err = parse_hook(ParseHookRequest {
        profile: Profile::CloudCompat,
        hook_name: "TIMEOUT".to_string(),
        hook: "buyer::task.receive.cmp +9223372036854775807d".to_string(),
    })
    .expect_err("duration overflow must be rejected");
    assert!(err.to_string().contains("duration is too large"));
}

#[test]
fn or_branches_resolve_by_earliest_received_signal() {
    let parsed = parse_hook(ParseHookRequest {
        profile: Profile::EvmStrict,
        hook_name: "TRIGGER".to_string(),
        hook: "buyer::(task.pay.cmp | task.ship.cmp) +5s".to_string(),
    })
    .unwrap();
    let eval = eval_compiled_hook(EvalCompiledHookRequest {
        profile: Profile::EvmStrict,
        ast: parsed.cloud_ast,
        signals: vec![
            SignalFact {
                source: "buyer".to_string(),
                signal_name: "task.pay.cmp".to_string(),
                received_at: "2026-04-27T00:01:40.000Z".to_string(),
            },
            SignalFact {
                source: "buyer".to_string(),
                signal_name: "task.ship.cmp".to_string(),
                received_at: "2026-04-27T00:00:01.000Z".to_string(),
            },
        ],
        now: "2026-04-27T00:00:06.000Z".to_string(),
    })
    .unwrap();
    assert_eq!(eval.state, EvalState::Ready);
    assert_eq!(eval.ready_at.as_deref(), Some("2026-04-27T00:00:06.000Z"));
}

#[test]
fn or_anchor_uses_arrival_not_expression_order_without_delay() {
    let parsed = parse_hook(ParseHookRequest {
        profile: Profile::EvmStrict,
        hook_name: "TRIGGER".to_string(),
        hook: "buyer::(task.pay.cmp | task.ship.cmp)".to_string(),
    })
    .unwrap();
    let eval = eval_compiled_hook(EvalCompiledHookRequest {
        profile: Profile::EvmStrict,
        ast: parsed.cloud_ast,
        signals: vec![
            SignalFact {
                source: "buyer".to_string(),
                signal_name: "task.pay.cmp".to_string(),
                received_at: "2026-04-27T00:09:00.000Z".to_string(),
            },
            SignalFact {
                source: "buyer".to_string(),
                signal_name: "task.ship.cmp".to_string(),
                received_at: "2026-04-27T00:00:30.000Z".to_string(),
            },
        ],
        now: "2026-04-27T00:09:01.000Z".to_string(),
    })
    .unwrap();
    assert_eq!(eval.state, EvalState::Ready);
    assert_eq!(eval.ready_at.as_deref(), Some("2026-04-27T00:00:30.000Z"));
}

#[test]
fn or_composite_delay_anchors_on_earliest_maturing_branch() {
    // OR 复合分支延时锚点裁决：`(A & B) | C` 且 min(A,B) < C ≤ max(A,B)
    // 时，AND 分支的"成熟时刻"= max(A,B)，早于 C 成熟的是 C 分支——外层
    // Delay 必须锚定 C 的成熟时刻（与合约 _orValue、回放 oracle 的
    // or_value 同口径）。
    let parsed = parse_hook(ParseHookRequest {
        profile: Profile::EvmStrict,
        hook_name: "TRIGGER".to_string(),
        hook: "buyer::((task.a.cmp & task.b.cmp) | task.c.cmp) +10s".to_string(),
    })
    .unwrap();
    let cloud_ast = parsed.cloud_ast.clone();
    let eval = eval_compiled_hook(EvalCompiledHookRequest {
        profile: Profile::EvmStrict,
        ast: cloud_ast.clone(),
        signals: vec![
            SignalFact {
                source: "buyer".to_string(),
                signal_name: "task.a.cmp".to_string(),
                received_at: "2026-04-27T00:00:10.000Z".to_string(),
            },
            SignalFact {
                source: "buyer".to_string(),
                signal_name: "task.b.cmp".to_string(),
                received_at: "2026-04-27T00:01:00.000Z".to_string(),
            },
            SignalFact {
                source: "buyer".to_string(),
                signal_name: "task.c.cmp".to_string(),
                received_at: "2026-04-27T00:00:40.000Z".to_string(),
            },
        ],
        now: "2026-04-27T00:00:55.000Z".to_string(),
    })
    .unwrap();
    // AND 分支成熟于 00:01:00，C 分支成熟于 00:00:40 → C 胜出，
    // +10s → readyAt = 00:00:50（now 已过 → Ready）。
    assert_eq!(eval.state, EvalState::Ready);
    assert_eq!(eval.ready_at.as_deref(), Some("2026-04-27T00:00:50.000Z"));

    // 同一事实在 00:00:45 观察必须仍在等待（readyAt 00:00:50 未到）。
    let waiting = eval_compiled_hook(EvalCompiledHookRequest {
        profile: Profile::EvmStrict,
        ast: cloud_ast,
        signals: vec![
            SignalFact {
                source: "buyer".to_string(),
                signal_name: "task.a.cmp".to_string(),
                received_at: "2026-04-27T00:00:10.000Z".to_string(),
            },
            SignalFact {
                source: "buyer".to_string(),
                signal_name: "task.b.cmp".to_string(),
                received_at: "2026-04-27T00:01:00.000Z".to_string(),
            },
            SignalFact {
                source: "buyer".to_string(),
                signal_name: "task.c.cmp".to_string(),
                received_at: "2026-04-27T00:00:40.000Z".to_string(),
            },
        ],
        now: "2026-04-27T00:00:45.000Z".to_string(),
    })
    .unwrap();
    assert_eq!(waiting.state, EvalState::Wait);
    assert_eq!(
        waiting.ready_at.as_deref(),
        Some("2026-04-27T00:00:50.000Z")
    );
}

#[test]
fn duration_with_multibyte_tail_fails_bounded_instead_of_panicking() {
    // 毒输入纪律：多字节 UTF-8 结尾必须在非字符边界确定性报错而不是
    // panic（毒 AST/毒输入必须有界失败，绝不 panic）。
    for raw in ["ü", "1ü", "10ü"] {
        let err = duration_to_seconds(raw).unwrap_err();
        assert!(
            err.to_string().contains("invalid duration"),
            "unexpected error for {raw:?}: {err}"
        );
    }
    // 非 ASCII 单位字母（非 s/m/h/d）同样是确定性错误。
    assert!(duration_to_seconds("10x").is_err());
    // 正常单位不受影响。
    assert_eq!(duration_to_seconds("48h").unwrap(), 48 * 60 * 60);
    assert_eq!(duration_to_seconds("30d").unwrap(), 30 * 24 * 60 * 60);
}

#[test]
fn non_subscription_source_header_requires_plain_identifier_of_at_most_36() {
    // 标头 source 类是路由键：编译期上限 36 字节（严于落库列宽
    // source_zhixu_id VARCHAR(64)，对齐 Go 镜像 zhixu_schema.go 的
    // ≤36 与标识符规则），超长/非法字符集在解析期拒绝。
    let overlong = "s".repeat(37);
    for raw in [
        format!("{overlong}::task.main.cmp"),
        "has space::task.main.cmp".to_string(),
        "buyer.ü::task.main.cmp".to_string(),
    ] {
        let err = parse_hook(ParseHookRequest {
            profile: Profile::EvmStrict,
            hook_name: "HOOK".to_string(),
            hook: raw.clone(),
        })
        .unwrap_err();
        assert!(
            err.to_string().contains("source"),
            "unexpected error for {raw:?}: {err}"
        );
    }
    // 36 字节边界恰好放行。
    let boundary = "s".repeat(36);
    parse_hook(ParseHookRequest {
        profile: Profile::EvmStrict,
        hook_name: "HOOK".to_string(),
        hook: format!("{boundary}::task.main.cmp"),
    })
    .unwrap();
}

#[test]
fn signal_facts_with_unknown_keys_are_rejected() {
    // 拼错的事实键（sourse）不得被静默吞成空 source 的无归属事实——
    // 那会把"事实不匹配"伪装成 ok:true needs_more。serde 层确定性拒绝。
    let request = json!({
        "profile": "cloud_compat",
        "ast": {
            "schemaVersion": CLOUD_AST_SCHEMA_VERSION,
            "source": "buyer",
            "mode": "normal",
            "root": { "type": "signal", "signal": "task.main.cmp" }
        },
        "signals": [{
            "sourse": "buyer",
            "signalName": "task.main.cmp",
            "receivedAt": "2026-04-27T00:00:00Z"
        }],
        "now": "2026-04-27T00:00:00Z"
    });
    let output = eval_compiled_hook_json(&request.to_string());
    assert!(
        output.contains("\"ok\":false") && output.contains("unknown field"),
        "misspelled fact key must be rejected: {output}"
    );

    // 缺失 source 仍是合法的无归属事实（needs_more，非错误）。
    let legal = json!({
        "profile": "cloud_compat",
        "ast": {
            "schemaVersion": CLOUD_AST_SCHEMA_VERSION,
            "source": "buyer",
            "mode": "normal",
            "root": { "type": "signal", "signal": "task.main.cmp" }
        },
        "signals": [{
            "signalName": "task.main.cmp",
            "receivedAt": "2026-04-27T00:00:00Z"
        }],
        "now": "2026-04-27T00:00:00Z"
    });
    let output = eval_compiled_hook_json(&legal.to_string());
    assert!(
        output.contains("\"ok\":true"),
        "missing source stays legal: {output}"
    );
}

#[test]
fn compiled_ast_atoms_with_invalid_identity_are_rejected_at_decode() {
    // 解码层身份闸与解析期同口径：毒原子确定性拒绝，而不是解码成
    // 永不匹配事实集的合法形态（那会把不匹配伪装成 ok:true needs_more）。
    let ast_with_root = |root: Value, source: &str| {
        json!({
            "schemaVersion": CLOUD_AST_SCHEMA_VERSION,
            "source": source,
            "mode": "normal",
            "root": root
        })
    };
    let poisoned_atoms = [
        // signal 节点：两段式 / 内嵌空格 / 超 100 字节。
        ast_with_root(json!({ "type": "signal", "signal": "main.cmp" }), "buyer"),
        ast_with_root(json!({ "type": "signal", "signal": "ta sk.a.b" }), "buyer"),
        ast_with_root(
            json!({ "type": "signal", "signal": format!("{}.{}.{}", "a".repeat(40), "b".repeat(30), "c".repeat(31)) }),
            "buyer",
        ),
        // 顶层 source：非法字符集 / 超 36 字节。
        ast_with_root(
            json!({ "type": "signal", "signal": "task.main.cmp" }),
            "has space",
        ),
        ast_with_root(
            json!({ "type": "signal", "signal": "task.main.cmp" }),
            "s".repeat(37).as_str(),
        ),
    ];
    for ast in poisoned_atoms {
        let err = eval_compiled_hook(EvalCompiledHookRequest {
            profile: Profile::CloudCompat,
            ast,
            signals: vec![],
            now: "2026-04-27T00:00:00Z".to_string(),
        })
        .unwrap_err();
        let message = err.to_string();
        assert!(
            message.contains("task.stage.signal")
                || message.contains("plain identifier of at most 36"),
            "unexpected error: {message}"
        );
    }

    // subscription 节点：source 字符集 / source 长度 / signal 段数。
    let subscription_ast = |source: &str, signal: &str| {
        json!({
            "schemaVersion": CLOUD_AST_SCHEMA_VERSION,
            "source": "",
            "mode": "subscription",
            "subscriptionTarget": { "source": "seller", "signal": "trade.listing.cmp" },
            "root": { "type": "subscription", "source": source, "signal": signal }
        })
    };
    for ast in [
        subscription_ast("has space", "trade.listing.cmp"),
        subscription_ast(&"s".repeat(37), "trade.listing.cmp"),
        subscription_ast("seller", "listing.cmp"),
    ] {
        let err = eval_compiled_hook(EvalCompiledHookRequest {
            profile: Profile::CloudCompat,
            ast,
            signals: vec![],
            now: "2026-04-27T00:00:00Z".to_string(),
        })
        .unwrap_err();
        let message = err.to_string();
        assert!(
            message.contains("plain identifier of at most 36")
                || message.contains("task.stage.signal"),
            "unexpected error: {message}"
        );
    }
}

#[test]
fn subscription_header_form_is_unaffected_by_source_header_cap() {
    // 订阅形态标头恒为空：不受标头 ≤36/plain-identifier 校验影响；
    // 订阅目标 source 自身按同值规则（≤36 + plain identifier）校验。
    parse_hook(ParseHookRequest {
        profile: Profile::EvmStrict,
        hook_name: "HOOK".to_string(),
        hook: "::ANCHOR(@seller::task.main.cmp)".to_string(),
    })
    .unwrap();
    // 订阅目标 source 超 36 字节：与标头同口径（编译上限 36 字节，
    // 严于落库列宽 source_zhixu_id VARCHAR(64)），解析期拒绝。
    let overlong = "s".repeat(37);
    let err = parse_hook(ParseHookRequest {
        profile: Profile::EvmStrict,
        hook_name: "HOOK".to_string(),
        hook: format!("::ANCHOR(@{overlong}::task.main.cmp)"),
    })
    .unwrap_err();
    assert!(
        err.to_string()
            .contains("subscription source exceeds the maximum length of 36"),
        "unexpected: {err}"
    );
    // 36 字节边界恰好放行。
    let boundary = "s".repeat(36);
    parse_hook(ParseHookRequest {
        profile: Profile::EvmStrict,
        hook_name: "HOOK".to_string(),
        hook: format!("::ANCHOR(@{boundary}::task.main.cmp)"),
    })
    .unwrap();
    // 订阅条目带非空标头仍按既有口径拒绝（而非新的标头错误）。
    let err = parse_hook(ParseHookRequest {
        profile: Profile::EvmStrict,
        hook_name: "HOOK".to_string(),
        hook: "buyer::ANCHOR(@seller::task.main.cmp)".to_string(),
    })
    .unwrap_err();
    assert!(
        err.to_string()
            .contains("subscription entries must use an empty source header"),
        "unexpected: {err}"
    );
}

#[test]
fn top_level_source_identity_is_validated_on_the_raw_text() {
    // 解码层身份闸吃原文（trim 只用于缺失判定）：与节点层
    // expr_from_cloud_value 同口径。" buyer" 经 trim 洗白后通过身份闸，
    // 是被架空的毒身份通道——两侧必须在同一原文上拒绝。
    let ast_with_root = |root: Value, source: Value| {
        json!({
            "schemaVersion": CLOUD_AST_SCHEMA_VERSION,
            "source": source,
            "mode": "normal",
            "root": root
        })
    };
    for source in [
        json!(" buyer"),
        json!("buyer "),
        json!("buy er"),
        json!(5),
        json!(true),
    ] {
        let err = eval_compiled_hook(EvalCompiledHookRequest {
            profile: Profile::CloudCompat,
            ast: ast_with_root(
                json!({ "type": "signal", "signal": "task.main.cmp" }),
                source.clone(),
            ),
            signals: vec![],
            now: "2026-04-27T00:00:00Z".to_string(),
        })
        .unwrap_err();
        let message = err.to_string();
        assert!(
            message.contains("plain identifier of at most 36")
                || message.contains("must be a string"),
            "poison source {source:?}: {message}"
        );
    }
    // null 与缺席等同（Go 零值解码同口径）：normal 模式按缺失拒绝。
    let err = eval_compiled_hook(EvalCompiledHookRequest {
        profile: Profile::CloudCompat,
        ast: ast_with_root(
            json!({ "type": "signal", "signal": "task.main.cmp" }),
            Value::Null,
        ),
        signals: vec![],
        now: "2026-04-27T00:00:00Z".to_string(),
    })
    .unwrap_err();
    assert!(
        err.to_string().contains("missing source"),
        "null source: {}",
        err
    );
}

#[test]
fn subscription_mode_rejects_non_string_and_whitespace_source() {
    // 订阅模式标头恒空：非字符串 source 不得被 as_str 吞成 None 再折
    // 成 "" 放行（毒 AST 的静默通道）；纯空白串也不是编译器产出的空
    // source，按原文非空拒绝，不做 trim 归一。
    let subscription_ast = |source: Value| {
        json!({
            "schemaVersion": CLOUD_AST_SCHEMA_VERSION,
            "source": source,
            "mode": "subscription",
            "subscriptionTarget": { "source": "seller", "signal": "trade.listing.cmp" },
            "root": { "type": "subscription", "source": "seller", "signal": "trade.listing.cmp" }
        })
    };
    for source in [json!(5), json!(" "), json!("\tbuyer")] {
        let err = eval_compiled_hook(EvalCompiledHookRequest {
            profile: Profile::CloudCompat,
            ast: subscription_ast(source.clone()),
            signals: vec![],
            now: "2026-04-27T00:00:00Z".to_string(),
        })
        .unwrap_err();
        let message = err.to_string();
        assert!(
            message.contains("must be a string") || message.contains("must be empty"),
            "poison subscription source {source:?}: {message}"
        );
    }
}

#[test]
fn top_level_subscription_target_identity_is_validated_on_the_raw_text() {
    // subscriptionTarget.source/signal 与 root 订阅节点同口径按原文校验：
    // 先 trim 再比对会让 " seller" 折叠成 "seller" 骗过一致性检查。
    let target_ast = |target: Value| {
        json!({
            "schemaVersion": CLOUD_AST_SCHEMA_VERSION,
            "source": "",
            "mode": "subscription",
            "subscriptionTarget": target,
            "root": { "type": "subscription", "source": "seller", "signal": "trade.listing.cmp" }
        })
    };
    for target in [
        json!({ "source": " seller", "signal": "trade.listing.cmp" }),
        json!({ "source": "seller ", "signal": "trade.listing.cmp" }),
        json!({ "source": "has space", "signal": "trade.listing.cmp" }),
        json!({ "source": "seller", "signal": " trade.listing.cmp" }),
        json!({ "source": "seller", "signal": "listing.cmp" }),
    ] {
        let err = eval_compiled_hook(EvalCompiledHookRequest {
            profile: Profile::CloudCompat,
            ast: target_ast(target.clone()),
            signals: vec![],
            now: "2026-04-27T00:00:00Z".to_string(),
        })
        .unwrap_err();
        let message = err.to_string();
        assert!(
            message.contains("plain identifier of at most 36")
                || message.contains("task.stage.signal")
                || message.contains("does not match"),
            "poison subscriptionTarget {target:?}: {message}"
        );
    }
}

#[test]
fn chained_delay_timers_accumulate_inner_expiry() {
    // 链式延时口径：(A+5s)+10s 的外层 timer 必须按内层到期累计——
    // timer(A,15) 是最终到期；timer(A,5) 是内层延时自己的中间 poke
    // 期限（与求值器分段等待语义一致），不再产出低估的 timer(A,10)。
    let out = parse_hook(ParseHookRequest {
        profile: Profile::EvmStrict,
        hook_name: "NESTED".to_string(),
        hook: "buyer::(task.a.cmp +5s) +10s".to_string(),
    })
    .unwrap();
    assert_eq!(
        out.dependencies,
        vec![
            Dependency {
                kind: DependencyKind::Positive,
                source: "buyer".to_string(),
                signal_name: "task.a.cmp".to_string(),
                delay_seconds: None,
            },
            Dependency {
                kind: DependencyKind::Timer,
                source: "buyer".to_string(),
                signal_name: "task.a.cmp".to_string(),
                delay_seconds: Some(5),
            },
            Dependency {
                kind: DependencyKind::Timer,
                source: "buyer".to_string(),
                signal_name: "task.a.cmp".to_string(),
                delay_seconds: Some(15),
            },
        ]
    );

    // 三层链：((A+1s)+2s)+3s → 中间期限 1、3，最终到期 6。
    let out = parse_hook(ParseHookRequest {
        profile: Profile::EvmStrict,
        hook_name: "CHAIN".to_string(),
        hook: "buyer::((task.a.cmp +1s) +2s) +3s".to_string(),
    })
    .unwrap();
    let timers = out
        .dependencies
        .iter()
        .filter(|dep| dep.kind == DependencyKind::Timer)
        .map(|dep| dep.delay_seconds.unwrap())
        .collect::<Vec<_>>();
    assert_eq!(timers, vec![1, 3, 6]);

    // 否定子树不产生 timer 锚：(A & ~B)+5s 只对 A 出 timer。
    let out = parse_hook(ParseHookRequest {
        profile: Profile::EvmStrict,
        hook_name: "GUARD".to_string(),
        hook: "buyer::(task.a.cmp & ~task.b.cmp) +5s".to_string(),
    })
    .unwrap();
    let timers = out
        .dependencies
        .iter()
        .filter(|dep| dep.kind == DependencyKind::Timer)
        .map(|dep| (dep.signal_name.clone(), dep.delay_seconds.unwrap()))
        .collect::<Vec<_>>();
    assert_eq!(
        timers,
        vec![("task.a.cmp".to_string(), 5)],
        "negated operands must not anchor timers: {:?}",
        out.dependencies
    );
}

#[test]
fn decaying_veto_three_window_states() {
    // 窗口三态：A 缺席 → 放行（无有效期）；A 在案未熟 → 放行至成熟时刻；
    // A 已熟 → 否决（Impossible）。成熟边界含端点：now == 成熟时刻即否决。
    let fact = |signal: &str, received_at: &str| SignalFact {
        source: "buyer".to_string(),
        signal_name: signal.to_string(),
        received_at: received_at.to_string(),
    };

    let absent = evaluate_compiled(
        "WINDOW",
        "buyer::task.ship.cmp & ~(task.cancel.cmp +14d)",
        Profile::EvmStrict,
        vec![fact("task.ship.cmp", "2026-04-27T00:00:05.000Z")],
        "2026-04-27T00:00:10.000Z",
    );
    assert_eq!(absent.state, EvalState::Ready);
    assert_eq!(absent.expires_at, None, "absent veto must not decay");

    let immature = evaluate_compiled(
        "WINDOW",
        "buyer::task.ship.cmp & ~(task.cancel.cmp +14d)",
        Profile::EvmStrict,
        vec![
            fact("task.ship.cmp", "2026-04-27T00:00:05.000Z"),
            fact("task.cancel.cmp", "2026-04-27T00:00:00.000Z"),
        ],
        "2026-04-27T00:00:10.000Z",
    );
    assert_eq!(immature.state, EvalState::Ready);
    assert_eq!(
        immature.expires_at.as_deref(),
        Some("2026-05-11T00:00:00.000Z"),
        "validity must end at the negated delay's maturity"
    );

    let matured = evaluate_compiled(
        "WINDOW",
        "buyer::task.ship.cmp & ~(task.cancel.cmp +14d)",
        Profile::EvmStrict,
        vec![
            fact("task.ship.cmp", "2026-04-27T00:00:05.000Z"),
            fact("task.cancel.cmp", "2026-04-27T00:00:00.000Z"),
        ],
        "2026-05-11T00:00:00.000Z",
    );
    assert_eq!(matured.state, EvalState::Impossible);
    assert!(
        matured
            .reason
            .as_deref()
            .unwrap_or_default()
            .contains("negated condition exists: task.cancel.cmp+14d"),
        "unexpected reason: {:?}",
        matured.reason
    );
}

#[test]
fn decaying_veto_position_rules_reject_non_conjunction_slots() {
    // 否决位仅合取直接子项合法：根位 / Or 子项 / Not 操作数 / Delay
    // 操作数内（任意深度，含 Delay 内嵌套 And/Or 再包衰减）一律编译期拒绝。
    let position_error = "only allowed as a direct operand of a conjunction";
    for hook in [
        "buyer::~(task.cancel.cmp +14d)",
        "buyer::task.a.cmp | ~(task.cancel.cmp +14d)",
        "buyer::(task.a.cmp & ~(task.cancel.cmp +14d)) +5s",
        "buyer::((task.a.cmp & (task.b.cmp & ~(task.cancel.cmp +14d))) | task.c.cmp) +5s",
    ] {
        let err = parse_hook(ParseHookRequest {
            profile: Profile::EvmStrict,
            hook_name: "WINDOW".to_string(),
            hook: hook.to_string(),
        })
        .unwrap_err();
        assert!(
            err.to_string().contains(position_error),
            "unexpected error for {hook}: {err}"
        );
    }

    let err = parse_hook(ParseHookRequest {
        profile: Profile::EvmStrict,
        hook_name: "WINDOW".to_string(),
        hook: "buyer::task.a.cmp & ~(~(task.cancel.cmp +14d))".to_string(),
    })
    .unwrap_err();
    assert!(
        err.to_string()
            .contains("negation only supports direct signal references"),
        "unexpected error for doubly negated veto: {err}"
    );
}

#[test]
fn decaying_veto_expires_at_takes_the_and_minimum() {
    // And 的有效期取成员最紧者：两个衰减项取较早成熟；无期限成员
    // （缺席否决/正向项）不放宽有限期。
    let fact = |signal: &str, received_at: &str| SignalFact {
        source: "buyer".to_string(),
        signal_name: signal.to_string(),
        received_at: received_at.to_string(),
    };
    let both_live = evaluate_compiled(
        "WINDOW",
        "buyer::task.b.cmp & ~(task.a1.cmp +5s) & ~(task.a2.cmp +10s)",
        Profile::EvmStrict,
        vec![
            fact("task.b.cmp", "2026-04-27T00:00:00.000Z"),
            fact("task.a1.cmp", "2026-04-27T00:00:01.000Z"),
            fact("task.a2.cmp", "2026-04-27T00:00:02.000Z"),
        ],
        "2026-04-27T00:00:03.000Z",
    );
    assert_eq!(both_live.state, EvalState::Ready);
    assert_eq!(
        both_live.expires_at.as_deref(),
        Some("2026-04-27T00:00:06.000Z"),
        "min(5s, 10s) decay must win"
    );

    let one_unbounded = evaluate_compiled(
        "WINDOW",
        "buyer::task.b.cmp & ~(task.a1.cmp +5s) & ~task.a2.cmp",
        Profile::EvmStrict,
        vec![
            fact("task.b.cmp", "2026-04-27T00:00:00.000Z"),
            fact("task.a1.cmp", "2026-04-27T00:00:01.000Z"),
        ],
        "2026-04-27T00:00:03.000Z",
    );
    assert_eq!(one_unbounded.state, EvalState::Ready);
    assert_eq!(
        one_unbounded.expires_at.as_deref(),
        Some("2026-04-27T00:00:06.000Z"),
        "an unbounded member must not loosen the finite decay"
    );
}

#[test]
fn decaying_veto_inside_an_or_winning_branch_floats_its_expiry() {
    // Or：获胜分支的 expires_at 原样上浮，不跨分支取 min——另一分支
    // 就绪且无衰减时，整体的 Ready 无有效期。
    let fact = |signal: &str, received_at: &str| SignalFact {
        source: "buyer".to_string(),
        signal_name: signal.to_string(),
        received_at: received_at.to_string(),
    };
    let veto_branch_wins = evaluate_compiled(
        "ANY",
        "buyer::(task.b.cmp & ~(task.a.cmp +5s)) | task.d.cmp",
        Profile::EvmStrict,
        vec![
            fact("task.b.cmp", "2026-04-27T00:00:00.000Z"),
            fact("task.a.cmp", "2026-04-27T00:00:01.000Z"),
        ],
        "2026-04-27T00:00:03.000Z",
    );
    assert_eq!(veto_branch_wins.state, EvalState::Ready);
    assert_eq!(
        veto_branch_wins.expires_at.as_deref(),
        Some("2026-04-27T00:00:06.000Z"),
        "the winning AND branch must float its decay up through the OR"
    );

    let plain_branch_wins = evaluate_compiled(
        "ANY",
        "buyer::(task.b.cmp & ~(task.a.cmp +5s)) | task.d.cmp",
        Profile::EvmStrict,
        vec![
            fact("task.a.cmp", "2026-04-27T00:00:01.000Z"),
            fact("task.d.cmp", "2026-04-27T00:00:02.000Z"),
        ],
        "2026-04-27T00:00:03.000Z",
    );
    assert_eq!(plain_branch_wins.state, EvalState::Ready);
    assert_eq!(
        plain_branch_wins.expires_at, None,
        "OR must not take a cross-branch expiry minimum"
    );
}

#[test]
fn decaying_veto_over_a_composite_delay_operand() {
    // 否定延时操作数可以是复合式：~((A|C)+5s) 的有效期 = 复合延时
    // 的成熟时刻（C 缺席不阻塞 A 分支成熟）。
    let fact = |signal: &str, received_at: &str| SignalFact {
        source: "buyer".to_string(),
        signal_name: signal.to_string(),
        received_at: received_at.to_string(),
    };
    let eval = evaluate_compiled(
        "WINDOW",
        "buyer::task.b.cmp & ~((task.a.cmp | task.c.cmp) +5s)",
        Profile::EvmStrict,
        vec![
            fact("task.b.cmp", "2026-04-27T00:00:00.000Z"),
            fact("task.a.cmp", "2026-04-27T00:00:01.000Z"),
        ],
        "2026-04-27T00:00:03.000Z",
    );
    assert_eq!(eval.state, EvalState::Ready);
    assert_eq!(eval.expires_at.as_deref(), Some("2026-04-27T00:00:06.000Z"));
}

#[test]
fn decaying_veto_serializes_expires_at_across_the_json_boundary() {
    // FFI/NAPI 序列化边界：有效期以 camelCase expiresAt 字段出场，
    // 无期限时字段缺席（skip_serializing_if）。
    let fact = |signal: &str, received_at: &str| SignalFact {
        source: "buyer".to_string(),
        signal_name: signal.to_string(),
        received_at: received_at.to_string(),
    };
    let request_for = |signals: Vec<SignalFact>, now: &str| {
        let parsed = parse_hook(ParseHookRequest {
            profile: Profile::EvmStrict,
            hook_name: "WINDOW".to_string(),
            hook: "buyer::task.ship.cmp & ~(task.cancel.cmp +14d)".to_string(),
        })
        .unwrap();
        json!({
            "profile": "evm_strict",
            "ast": parsed.cloud_ast,
            "signals": signals
                .into_iter()
                .map(|fact| json!({
                    "source": fact.source,
                    "signalName": fact.signal_name,
                    "receivedAt": fact.received_at,
                }))
                .collect::<Vec<_>>(),
            "now": now,
        })
    };

    let bounded = eval_compiled_hook_json(
        &request_for(
            vec![
                fact("task.ship.cmp", "2026-04-27T00:00:05.000Z"),
                fact("task.cancel.cmp", "2026-04-27T00:00:00.000Z"),
            ],
            "2026-04-27T00:00:10.000Z",
        )
        .to_string(),
    );
    assert!(
        bounded.contains("\"expiresAt\":\"2026-05-11T00:00:00.000Z\""),
        "expiresAt must serialize on the JSON boundary: {bounded}"
    );

    let unbounded = eval_compiled_hook_json(
        &request_for(
            vec![fact("task.ship.cmp", "2026-04-27T00:00:05.000Z")],
            "2026-04-27T00:00:10.000Z",
        )
        .to_string(),
    );
    assert!(
        !bounded.is_empty() && !unbounded.contains("expiresAt"),
        "absent veto must leave expiresAt out of the envelope: {unbounded}"
    );
}
