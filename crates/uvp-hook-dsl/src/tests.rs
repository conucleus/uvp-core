use super::*;
use serde_json::json;

fn evaluate_compiled(
    hook_name: &str,
    hook: &str,
    profile: Profile,
    signals: Vec<SignalFact>,
    now: &str,
) -> EvalCompiledHookOutput {
    let parsed = parse_hook(ParseHookRequest {
        profile,
        gate: Gate::Hook,
        hook_name: hook_name.to_string(),
        hook: hook.to_string(),
    })
    .unwrap();
    eval_compiled_hook(EvalCompiledHookRequest {
        profile,
        gate: Gate::Hook,
        ast: parsed.cloud_ast,
        signals,
        now: now.to_string(),
    })
    .unwrap()
}

#[test]
fn rejects_deeply_nested_expressions_instead_of_overflowing() {
    let deep = format!("buyer::{}a{}", "(".repeat(50_000), ")".repeat(50_000));
    let err = parse_hook(ParseHookRequest {
        profile: Profile::EvmStrict,
        gate: Gate::Hook,
        hook_name: "HOOK".to_string(),
        hook: deep,
    })
    .unwrap_err();
    assert!(err.to_string().contains("maximum depth of 120"));
}

#[test]
fn parse_depth_cap_keeps_parseable_hooks_evaluable_through_json() {
    let legal = format!(
        "buyer::{}task.main.cmp +1s{}",
        "(".repeat(58),
        ")".repeat(58)
    );
    let parsed = parse_hook(ParseHookRequest {
        profile: Profile::EvmStrict,
        gate: Gate::Hook,
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
        gate: Gate::Hook,
        hook_name: "TIMEOUT".to_string(),
        hook: illegal,
    })
    .unwrap_err();
    assert!(err.to_string().contains("maximum depth of 120"));
}

#[test]
fn rejects_deeply_nested_cloud_ast() {
    let mut root = json!({ "type": "signal", "signal": "task.main.cmp" });
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
        gate: Gate::Hook,
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
        gate: Gate::Hook,
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
fn cloud_ast_preserves_delay_operand_and_source() {
    let out = parse_hook(ParseHookRequest {
        profile: Profile::CloudCompat,
        gate: Gate::Hook,
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
fn compiled_subscription_target_rejects_unknown_keys_and_shapes() {
    let mut ast = parse_hook(ParseHookRequest {
        profile: Profile::CloudCompat,
        gate: Gate::Hook,
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
        gate: Gate::Hook,
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
        gate: Gate::Hook,
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
        gate: Gate::Hook,
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
fn compiled_hook_evaluation_requires_schema_version() {
    let err = eval_compiled_hook(EvalCompiledHookRequest {
        profile: Profile::CloudCompat,
        gate: Gate::Hook,
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
        gate: Gate::Hook,
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
fn pseudo_keyword_prefixes_do_not_bypass_the_empty_source_gate() {
    let retired_word: String = ["M", "E", "R", "G", "E"].concat();
    for hook in [
        format!("::{retired_word}X@(seller::task.main.cmp)"),
        "::ANCHORX(@seller::task.main.cmp)".to_string(),
        "::OUTSIDER".to_string(),
    ] {
        let err = parse_hook(ParseHookRequest {
            profile: Profile::CloudCompat,
            gate: Gate::Hook,
            hook_name: "HOOK".to_string(),
            hook,
        })
        .unwrap_err();
        assert!(
            err.to_string().contains("empty source"),
            "unexpected error for pseudo-prefix form: {err}"
        );
    }
    let err = parse_hook(ParseHookRequest {
        profile: Profile::CloudCompat,
        gate: Gate::Hook,
        hook_name: "HOOK".to_string(),
        hook: "::OUTSIDE @seller::task.main.cmp".to_string(),
    })
    .unwrap_err();
    assert!(err.to_string().contains("retired"), "unexpected: {err}");
}

#[test]
fn signed_raw_duration_is_rejected_at_decode() {
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
        gate: Gate::Hook,
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
        gate: Gate::Hook,
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
            gate: Gate::Hook,
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
    eval_compiled_hook(EvalCompiledHookRequest {
        profile: Profile::CloudCompat,
        gate: Gate::Hook,
        ast: parsed.cloud_ast.clone(),
        signals: vec![SignalFact {
            source: "s".repeat(36),
            signal_name: "task.main.cmp".to_string(),
            received_at: "2026-04-27T00:00:00Z".to_string(),
        }],
        now: "2026-04-27T00:00:00Z".to_string(),
    })
    .unwrap();
    let unattributed = eval_compiled_hook(EvalCompiledHookRequest {
        profile: Profile::CloudCompat,
        gate: Gate::Hook,
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
    let poisoned = json!({
        "schemaVersion": CLOUD_AST_SCHEMA_VERSION,
        "source": "buyer",
        "mode": "normal",
        "subscriptionTarget": { "source": "seller", "signal": "trade.listing.cmp" },
        "root": { "type": "signal", "signal": "task.main.cmp" }
    });
    let err = eval_compiled_hook(EvalCompiledHookRequest {
        profile: Profile::CloudCompat,
        gate: Gate::Hook,
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
fn unsupported_hook_modes_are_rejected_at_decode() {
    for mode in ["outside_spawn", "anchor", "bundle"] {
        let err = eval_compiled_hook(EvalCompiledHookRequest {
            profile: Profile::CloudCompat,
            gate: Gate::Hook,
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
        gate: Gate::Hook,
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
            gate: Gate::Hook,
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
        gate: Gate::Hook,
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
        gate: Gate::Hook,
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
        gate: Gate::Hook,
        ast: poisoned_route,
        signals: vec![],
        now: "2026-04-27T00:00:00Z".to_string(),
    })
    .expect_err("unknown route must not decode");
    assert!(
        err.to_string().contains("route is invalid"),
        "unexpected error: {err}"
    );

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
        gate: Gate::Hook,
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
        gate: Gate::Hook,
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
        gate: Gate::Hook,
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
fn rejects_duration_overflow() {
    let err = parse_hook(ParseHookRequest {
        profile: Profile::CloudCompat,
        gate: Gate::Hook,
        hook_name: "TIMEOUT".to_string(),
        hook: "buyer::task.receive.cmp +9223372036854775807d".to_string(),
    })
    .expect_err("duration overflow must be rejected");
    assert!(err.to_string().contains("duration is too large"));
}

#[test]
fn duration_with_multibyte_tail_fails_bounded_instead_of_panicking() {
    for raw in ["ü", "1ü", "10ü"] {
        let err = duration_to_seconds(raw).unwrap_err();
        assert!(
            err.to_string().contains("invalid duration"),
            "unexpected error for {raw:?}: {err}"
        );
    }
    assert!(duration_to_seconds("10x").is_err());
    assert_eq!(duration_to_seconds("48h").unwrap(), 48 * 60 * 60);
    assert_eq!(duration_to_seconds("30d").unwrap(), 30 * 24 * 60 * 60);
}

#[test]
fn signal_facts_with_unknown_keys_are_rejected() {
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
    let ast_with_root = |root: Value, source: &str| {
        json!({
            "schemaVersion": CLOUD_AST_SCHEMA_VERSION,
            "source": source,
            "mode": "normal",
            "root": root
        })
    };
    let poisoned_atoms = [
        ast_with_root(json!({ "type": "signal", "signal": "main.cmp" }), "buyer"),
        ast_with_root(json!({ "type": "signal", "signal": "ta sk.a.b" }), "buyer"),
        ast_with_root(
            json!({ "type": "signal", "signal": format!("{}.{}.{}", "a".repeat(40), "b".repeat(30), "c".repeat(31)) }),
            "buyer",
        ),
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
            gate: Gate::Hook,
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
            gate: Gate::Hook,
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
fn top_level_source_identity_is_validated_on_the_raw_text() {
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
            gate: Gate::Hook,
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
    let err = eval_compiled_hook(EvalCompiledHookRequest {
        profile: Profile::CloudCompat,
        gate: Gate::Hook,
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
            gate: Gate::Hook,
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
            gate: Gate::Hook,
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
fn decaying_veto_expires_at_takes_the_and_minimum() {
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
    let fact = |signal: &str, received_at: &str| SignalFact {
        source: "buyer".to_string(),
        signal_name: signal.to_string(),
        received_at: received_at.to_string(),
    };
    let request_for = |signals: Vec<SignalFact>, now: &str| {
        let parsed = parse_hook(ParseHookRequest {
            profile: Profile::EvmStrict,
            gate: Gate::Hook,
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

#[test]
fn gate_rides_the_json_boundary_and_defaults_to_hook() {
    let request = |gate: Option<&str>| {
        let mut envelope = json!({
            "hookName": "ADMIT",
            "hook": "buyer::~(task.cancel.cmp +14d)"
        });
        if let Some(gate) = gate {
            envelope["gate"] = json!(gate);
        }
        envelope.to_string()
    };
    let hooked = parse_hook_json(&request(None));
    assert!(
        hooked.contains("only allowed as a direct operand of a conjunction"),
        "absent gate must keep hook-gate semantics: {hooked}"
    );
    let filtered = parse_hook_json(&request(Some("filter")));
    assert!(
        filtered.contains("\"ok\":true"),
        "gate=filter must admit the bare veto root: {filtered}"
    );

    let ast = serde_json::from_str::<Value>(&filtered).expect("envelope decodes")["value"]
        ["cloudAst"]
        .clone();
    let eval_request = |gate: Option<&str>| {
        let mut envelope = json!({ "ast": ast, "signals": [], "now": "2026-04-27T00:00:10.000Z" });
        if let Some(gate) = gate {
            envelope["gate"] = json!(gate);
        }
        envelope.to_string()
    };
    let eval_filtered = eval_compiled_hook_json(&eval_request(Some("filter")));
    assert!(
        eval_filtered.contains("\"state\":\"ready\""),
        "gate=filter must evaluate the veto root: {eval_filtered}"
    );
    let eval_hooked = eval_compiled_hook_json(&eval_request(None));
    assert!(
        eval_hooked.contains("only allowed as a direct operand of a conjunction"),
        "absent gate must keep hook-gate decode defense: {eval_hooked}"
    );
}
