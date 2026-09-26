use serde::Deserialize;
use serde_json::Value;
use uvp_hook_dsl::{
    eval_compiled_hook, parse_hook, EvalCompiledHookRequest, Gate, ParseHookRequest, Profile,
    SignalFact,
};

const CORPUS: &str = include_str!("../../../fixtures/hook/semantics.v1.json");

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Corpus {
    schema_version: String,
    parse_cases: Vec<ParseCase>,
    eval_cases: Vec<EvalCase>,
    invalid_cases: Vec<InvalidCase>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ParseCase {
    name: String,
    profile: String,
    /// 校验档（缺省 hook）：发射适格面（filter）用例走过滤档——与
    /// ParseHookRequest.gate 同先例，缺省保持既有钩子档语义。
    #[serde(default)]
    gate: Option<String>,
    hook_name: String,
    hook: String,
    expect: ParseExpect,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ParseExpect {
    source: String,
    mode: String,
    runtime_condition: String,
    normalized_expression: String,
    dependencies: Vec<Value>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct EvalCase {
    name: String,
    profile: String,
    #[serde(default)]
    gate: Option<String>,
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
    expires_at: Option<String>,
    reason_contains: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct InvalidCase {
    name: String,
    profile: String,
    #[serde(default)]
    gate: Option<String>,
    hook_name: String,
    hook: String,
    message_contains: String,
}

fn load_corpus() -> Corpus {
    let corpus: Corpus = serde_json::from_str(CORPUS).expect("semantic corpus should decode");
    // 语料格式版本钉住：v2 迁移时这里必须先响亮失败，消费面不得静默按旧
    // 口径解读新文件（replay/TS/Go 消费测试同款断言）。
    assert_eq!(
        corpus.schema_version, "uvp.hookSemanticsCorpus.v1",
        "corpus schemaVersion drifted; migrate every consumer before shipping the new file"
    );
    corpus
}

fn profile(value: &str) -> Profile {
    match value {
        "cloud_compat" => Profile::CloudCompat,
        "evm_strict" => Profile::EvmStrict,
        other => panic!("unknown profile {other}"),
    }
}

fn gate(value: &Option<String>) -> Gate {
    match value.as_deref() {
        None | Some("hook") => Gate::Hook,
        Some("filter") => Gate::Filter,
        other => panic!("unknown gate {other:?}"),
    }
}

#[test]
fn parses_semantic_corpus() {
    for case in load_corpus().parse_cases {
        let output = parse_hook(ParseHookRequest {
            profile: profile(&case.profile),
            gate: gate(&case.gate),
            hook_name: case.hook_name.clone(),
            hook: case.hook.clone(),
        })
        .unwrap_or_else(|err| panic!("{} failed to parse: {err}", case.name));
        let output_value = serde_json::to_value(&output).expect("parse output should serialize");

        assert_eq!(output.source, case.expect.source, "{}", case.name);
        assert_eq!(output_value["mode"], case.expect.mode, "{}", case.name);
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
        let profile = profile(&case.profile);
        let gate = gate(&case.gate);
        let parsed = parse_hook(ParseHookRequest {
            profile,
            gate,
            hook_name: case.hook_name.clone(),
            hook: case.hook.clone(),
        })
        .unwrap_or_else(|err| panic!("{} failed to parse for eval: {err}", case.name));
        let output = eval_compiled_hook(EvalCompiledHookRequest {
            profile,
            gate,
            ast: parsed.cloud_ast,
            signals: case.signals,
            now: case.now,
        })
        .unwrap_or_else(|err| panic!("{} failed to eval compiled AST: {err}", case.name));
        let output_value = serde_json::to_value(&output).expect("eval output should serialize");

        assert_eq!(output_value["state"], case.expect.state, "{}", case.name);
        if case.expect.ready_at.is_some() {
            assert_eq!(
                output.ready_at, case.expect.ready_at,
                "readyAt mismatch: {}",
                case.name
            );
        }
        // 衰减维度对每个 eval 用例整体钉死（缺席 = 无期限），不做
        // "写了才比对"：否则带否决位的用例漏写 expiresAt 会被静默放过。
        assert_eq!(
            output.expires_at, case.expect.expires_at,
            "expiresAt mismatch: {}",
            case.name
        );
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
            gate: gate(&case.gate),
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
