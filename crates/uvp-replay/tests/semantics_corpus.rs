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
    }
}
