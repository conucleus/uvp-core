use serde::Deserialize;
use serde_json::Value;
use uvp_replay::{replay_chain_events, ReplayOptions};

const CORPUS: &str = include_str!("../../../fixtures/hook/semantics.v1.json");

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Corpus {
    schema_version: String,
    replay_cases: Vec<ReplayCase>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReplayCase {
    name: String,
    events: Vec<Value>,
    expect: ReplayExpect,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReplayExpect {
    // 计数对 error-only 条目无意义（回放在断言前响亮失败），给默认 0。
    #[serde(default)]
    observed_count: usize,
    #[serde(default)]
    mismatch_count: usize,
    // 事实探针是可选的：mismatch/error 条目的事件流可以不落任何信号，
    // 强制字段会把"无事实"伪装成必填断言。
    #[serde(default)]
    order_key: Option<String>,
    #[serde(default)]
    signal_key: Option<String>,
    #[serde(default)]
    sender_id: Option<String>,
    #[serde(default)]
    event_id: Option<String>,
    /// Assert the eventName sequence of every observed oracle emission.
    #[serde(default)]
    observed_events: Option<Vec<String>>,
    /// Assert the dueAt carried by the single wait observation, if any.
    #[serde(default)]
    wait_due_at: Option<String>,
    /// Assert terminal hook statuses: {orderKey -> {hookId -> status}}.
    #[serde(default)]
    final_hook_statuses:
        Option<std::collections::BTreeMap<String, std::collections::BTreeMap<String, String>>>,
    /// Assert these order keys exist in the replayed state (lineage/multi-order facts).
    #[serde(default)]
    state_order_keys: Option<Vec<String>>,
    /// Assert the exact mismatch set (reason + hook + occurrence): replay must
    /// not only count a broken stream's mismatches but report them at the
    /// right hook observation.
    #[serde(default)]
    mismatch_details: Option<Vec<MismatchDetail>>,
    /// Assert the replay fails loudly with a message containing this text.
    #[serde(default)]
    error_contains: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MismatchDetail {
    reason: String,
    hook: String,
    occurrence: i64,
}

#[test]
fn replays_semantic_corpus() {
    let corpus: Corpus = serde_json::from_str(CORPUS).expect("semantic corpus should decode");
    // 语料格式版本钉住：v2 迁移时这里必须先响亮失败，消费面不得静默按旧
    // 口径解读新文件（TS/Go 消费测试同款断言）。
    assert_eq!(
        corpus.schema_version, "uvp.hookSemanticsCorpus.v1",
        "corpus schemaVersion drifted; migrate every consumer before shipping the new file"
    );
    for case in corpus.replay_cases {
        if let Some(expected) = &case.expect.error_contains {
            let err = replay_chain_events(case.events, &ReplayOptions::default())
                .expect_err(format!("{} must fail loudly", case.name).as_str());
            assert!(
                err.to_string().contains(expected),
                "{} error {err:?} did not contain {expected:?}",
                case.name
            );
            continue;
        }
        let result = replay_chain_events(case.events, &ReplayOptions::default())
            .unwrap_or_else(|err| panic!("{} failed to replay: {err}", case.name));
        let observed = result["observed"]
            .as_array()
            .expect("observed should be an array");
        let mismatches = result["mismatches"]
            .as_array()
            .expect("mismatches should be an array");

        assert_eq!(observed.len(), case.expect.observed_count, "{}", case.name);
        assert_eq!(
            mismatches.len(),
            case.expect.mismatch_count,
            "{}",
            case.name
        );

        if let Some(order_key) = &case.expect.order_key {
            let signal_key = case
                .expect
                .signal_key
                .as_deref()
                .unwrap_or_else(|| panic!("{} pins orderKey without signalKey", case.name));
            let signal = &result["state"]["orders"][order_key]["signals"][signal_key];
            let sender_id = case.expect.sender_id.as_deref().unwrap_or_default();
            let event_id = case.expect.event_id.as_deref().unwrap_or_default();
            assert_eq!(signal["senderId"], sender_id, "{}", case.name);
            assert_eq!(signal["eventId"], event_id, "{}", case.name);
        }

        if let Some(expected_events) = &case.expect.observed_events {
            let names: Vec<&str> = observed
                .iter()
                .map(|item| item["eventName"].as_str().unwrap_or_default())
                .collect();
            assert_eq!(&names, expected_events, "{}", case.name);
        }
        if let Some(due_at) = &case.expect.wait_due_at {
            let waits: Vec<&Value> = observed
                .iter()
                .filter(|item| item["eventName"] == "HookStatusChanged" && item["status"] == "wait")
                .collect();
            assert_eq!(
                waits.len(),
                1,
                "{} expected exactly one wait observation",
                case.name
            );
            assert_eq!(
                waits[0]["dueAt"].as_str().unwrap_or_default(),
                due_at,
                "{}",
                case.name
            );
        }
        if let Some(details) = &case.expect.mismatch_details {
            assert_eq!(
                mismatches.len(),
                details.len(),
                "{} mismatch set size",
                case.name
            );
            for (index, detail) in details.iter().enumerate() {
                assert_eq!(
                    mismatches[index]["reason"], detail.reason,
                    "{} mismatch[{index}] reason",
                    case.name
                );
                assert_eq!(
                    mismatches[index]["hook"], detail.hook,
                    "{} mismatch[{index}] hook",
                    case.name
                );
                assert_eq!(
                    mismatches[index]["occurrence"], detail.occurrence,
                    "{} mismatch[{index}] occurrence",
                    case.name
                );
            }
        }
        if let Some(finals) = &case.expect.final_hook_statuses {
            for (order_key, hooks) in finals {
                for (hook_id, status) in hooks {
                    let actual =
                        &result["state"]["orders"][order_key]["hookStatuses"][hook_id]["status"];
                    assert_eq!(actual.as_str().unwrap_or_default(), status, "{}", case.name);
                }
            }
        }
        if let Some(order_keys) = &case.expect.state_order_keys {
            let orders = result["state"]["orders"]
                .as_object()
                .expect("state orders should be an object");
            for key in order_keys {
                assert!(
                    orders.contains_key(key),
                    "{} missing order {key}",
                    case.name
                );
            }
        }
    }
}
