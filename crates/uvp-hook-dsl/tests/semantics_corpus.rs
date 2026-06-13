use serde::Deserialize;
use serde_json::Value;
use uvp_hook_dsl::{eval_hook, parse_hook, EvalHookRequest, ParseHookRequest, Profile, SignalFact};

const CORPUS: &str = include_str!("../../../fixtures/hook/semantics.v1.json");

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Corpus {
    parse_cases: Vec<ParseCase>,
    eval_cases: Vec<EvalCase>,
    invalid_cases: Vec<InvalidCase>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ParseCase {
    name: String,
    profile: String,
    hook_name: String,
    hook: String,
    expect: ParseExpect,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ParseExpect {
    source: String,
    mode: String,
    upstream_source: Option<String>,
    runtime_condition: String,
    normalized_expression: String,
    dependencies: Vec<Value>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct EvalCase {
    name: String,
    profile: String,
    hook_name: String,
    hook: String,
    signals: Vec<SignalFact>,
    now: String,
    expect: EvalExpect,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct EvalExpect {
    state: String,
    ready_at: Option<String>,
    reason_contains: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct InvalidCase {
    name: String,
    profile: String,
    hook_name: String,
    hook: String,
    message_contains: String,
}

fn load_corpus() -> Corpus {
    serde_json::from_str(CORPUS).expect("semantic corpus should decode")
}

fn profile(value: &str) -> Profile {
    match value {
        "cloud_compat" => Profile::CloudCompat,
        "evm_strict" => Profile::EvmStrict,
        other => panic!("unknown profile {other}"),
    }
}

#[test]
fn parses_semantic_corpus() {
    for case in load_corpus().parse_cases {
        let output = parse_hook(ParseHookRequest {
            profile: profile(&case.profile),
            hook_name: case.hook_name.clone(),
            hook: case.hook.clone(),
        })
        .unwrap_or_else(|err| panic!("{} failed to parse: {err}", case.name));
        let output_value = serde_json::to_value(&output).expect("parse output should serialize");

        assert_eq!(output.source, case.expect.source, "{}", case.name);
        assert_eq!(output_value["mode"], case.expect.mode, "{}", case.name);
        assert_eq!(
            output.upstream_source, case.expect.upstream_source,
            "{}",
            case.name
        );
        assert_eq!(
            output.runtime_condition, case.expect.runtime_condition,
            "{}",
            case.name
        );
        assert_eq!(
            output.normalized_expression, case.expect.normalized_expression,
            "{}",
            case.name
        );
        assert_eq!(
            output_value["dependencies"],
            Value::Array(case.expect.dependencies),
            "{}",
            case.name
        );
    }
}

#[test]
fn evaluates_semantic_corpus() {
    for case in load_corpus().eval_cases {
        let output = eval_hook(EvalHookRequest {
            profile: profile(&case.profile),
            hook_name: case.hook_name.clone(),
            hook: case.hook.clone(),
            signals: case.signals,
            now: case.now,
        })
        .unwrap_or_else(|err| panic!("{} failed to eval: {err}", case.name));
        let output_value = serde_json::to_value(&output).expect("eval output should serialize");

        assert_eq!(output_value["state"], case.expect.state, "{}", case.name);
        if case.expect.ready_at.is_some() {
            assert_eq!(
                output.ready_at, case.expect.ready_at,
                "readyAt mismatch: {}",
                case.name
            );
        }
        if let Some(expected) = case.expect.reason_contains {
            let reason = output.reason.unwrap_or_default();
            assert!(
                reason.contains(&expected),
                "{} reason {reason:?} did not contain {expected:?}",
                case.name
            );
        }
    }
}

#[test]
fn rejects_invalid_semantic_corpus() {
    for case in load_corpus().invalid_cases {
        let err = parse_hook(ParseHookRequest {
            profile: profile(&case.profile),
            hook_name: case.hook_name.clone(),
            hook: case.hook.clone(),
        })
        .unwrap_err();
        assert!(
            err.to_string().contains(&case.message_contains),
            "{} error {:?} did not contain {:?}",
            case.name,
            err.to_string(),
            case.message_contains
        );
    }
}
