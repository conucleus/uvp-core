use serde::Deserialize;
use serde_json::Value;
use uvp_replay::{replay_chain_events, ReplayOptions};

const CORPUS: &str = include_str!("../../../fixtures/hook/semantics.v1.json");

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Corpus {
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
    order_key: String,
    signal_key: String,
    sender_id: String,
    event_id: String,
    observed_count: usize,
    mismatch_count: usize,
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
}

#[test]
fn replays_semantic_corpus() {
    let corpus: Corpus = serde_json::from_str(CORPUS).expect("semantic corpus should decode");
    for case in corpus.replay_cases {
        let result = replay_chain_events(case.events, &ReplayOptions::default())
            .unwrap_or_else(|err| panic!("{} failed to replay: {err}", case.name));
        let observed = result["observed"]
            .as_array()
            .expect("observed should be an array");
        let mismatches = result["mismatches"]
            .as_array()
            .expect("mismatches should be an array");
        let signal =
            &result["state"]["orders"][&case.expect.order_key]["signals"][&case.expect.signal_key];

        assert_eq!(observed.len(), case.expect.observed_count, "{}", case.name);
        assert_eq!(
            mismatches.len(),
            case.expect.mismatch_count,
            "{}",
            case.name
        );
        assert_eq!(signal["senderId"], case.expect.sender_id, "{}", case.name);
        assert_eq!(signal["eventId"], case.expect.event_id, "{}", case.name);

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
