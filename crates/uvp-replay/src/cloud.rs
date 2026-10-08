//! 云轨重放走带：把云侧持久化的事实日志（依赖信号集，按受理因果序）
//! 逐条喂给纯函数 kernel，复现在线裁决的时间线，产出 hook_state 口径的
//! 期望状态。与 `transition/`（链轨事件 oracle）同属重放 oracle 家族：
//! 链轨以链上事件流为真理源做观察级 diff，云轨以 DB 事实日志为真理源
//! 做终态 diff——两者输入域与输出域互不重叠，不共享状态模型。

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use uvp_hook_dsl::{
    decode_compiled_hook, valid_signal_identity, DecodedCompiledHook, EvalState, Gate, HookMode,
};

use crate::{ReplayError, Result};

const MATURITY_DECISION_SLACK_MS: i64 = 1;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct CloudReplayRequest {
    pub ast: serde_json::Value,
    #[serde(default)]
    pub facts: Vec<CloudReplayFact>,
    pub now: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct CloudReplayFact {
    pub signal_name: String,
    pub arrived_at: String,
}

#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CloudReplayOutcome {
    pub status: &'static str,
}

pub const STATUS_READY: &str = "ready";
pub const STATUS_WAIT: &str = "wait";
pub const STATUS_CXL: &str = "cxl";
pub const STATUS_INIT: &str = "init";

pub fn replay_compiled_hook(request: CloudReplayRequest) -> Result<CloudReplayOutcome> {
    let decoded = decode_compiled_hook(&request.ast, Gate::Hook)
        .map_err(|err| ReplayError::Message(err.to_string()))?;
    if decoded.mode == HookMode::Subscription {
        return Ok(CloudReplayOutcome {
            status: STATUS_READY,
        });
    }
    let now = parse_cloud_time(&request.now)?;
    let mut facts = Vec::with_capacity(request.facts.len());
    for fact in &request.facts {
        if !valid_signal_identity(&fact.signal_name) {
            return Err(ReplayError::Message(format!(
                "replay fact signal_name must use stage.signal and be at most 100 characters: {:?}",
                fact.signal_name
            )));
        }
        facts.push((
            fact.signal_name.clone(),
            parse_cloud_time(&fact.arrived_at)?,
        ));
    }

    let mut signals: BTreeMap<String, DateTime<Utc>> = BTreeMap::new();
    let mut status = STATUS_INIT;
    let mut terminal = false;
    for (index, (name, arrived_at)) in facts.iter().enumerate() {
        if terminal {
            break;
        }
        let horizon = if index + 1 < facts.len() {
            facts[index + 1].1
        } else {
            now
        };
        signals.insert(name.clone(), *arrived_at);
        let mut step = eval_step(&decoded, &signals, *arrived_at, &mut status, &mut terminal)?;
        while !terminal && step.state == EvalState::Wait {
            let Some(due_at) = step.ready_at else {
                break;
            };
            if due_at > horizon {
                break;
            }
            let decision_at = due_at
                .checked_add_signed(chrono::Duration::milliseconds(MATURITY_DECISION_SLACK_MS))
                .ok_or_else(|| {
                    ReplayError::Message(
                        "maturity decision point overflows the replay timestamp range".to_string(),
                    )
                })?;
            step = eval_step(&decoded, &signals, decision_at, &mut status, &mut terminal)?;
        }
    }
    if !terminal {
        eval_step(&decoded, &signals, now, &mut status, &mut terminal)?;
    }
    Ok(CloudReplayOutcome { status })
}

fn eval_step(
    decoded: &DecodedCompiledHook,
    signals: &BTreeMap<String, DateTime<Utc>>,
    at: DateTime<Utc>,
    status: &mut &'static str,
    terminal: &mut bool,
) -> Result<uvp_hook_dsl::HookEval> {
    let evaluated = decoded
        .eval(signals, at)
        .map_err(|err| ReplayError::Message(err.to_string()))?;
    match evaluated.state {
        EvalState::Ready => {
            *status = STATUS_READY;
            *terminal = true;
        }
        EvalState::Impossible => {
            *status = STATUS_CXL;
            *terminal = true;
        }
        EvalState::Wait => *status = STATUS_WAIT,
        EvalState::NeedsMore => *status = STATUS_INIT,
    }
    Ok(evaluated)
}

fn parse_cloud_time(value: &str) -> Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .map(|timestamp| timestamp.with_timezone(&Utc))
        .map_err(|err| ReplayError::Message(format!("invalid replay timestamp {value:?}: {err}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use uvp_hook_dsl::{parse_hook, ParseHookRequest, Profile};

    fn cloud_ast(hook: &str) -> serde_json::Value {
        let output = parse_hook(ParseHookRequest {
            profile: Profile::CloudCompat,
            gate: Gate::Hook,
            hook_name: "TEST".to_string(),
            hook: hook.to_string(),
        })
        .expect("test hook must compile");
        output.cloud_ast
    }

    fn iso(seconds_ago: i64) -> String {
        (Utc::now() - chrono::Duration::seconds(seconds_ago))
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
    }

    fn walk(hook: &str, facts: &[(&str, String)], now: &str) -> Result<CloudReplayOutcome> {
        replay_compiled_hook(CloudReplayRequest {
            ast: cloud_ast(hook),
            facts: facts
                .iter()
                .map(|(name, arrived_at)| CloudReplayFact {
                    signal_name: (*name).to_string(),
                    arrived_at: arrived_at.clone(),
                })
                .collect(),
            now: now.to_string(),
        })
    }

    const READY_FIRST: &str = "src::b.ready & ~b.rejected";
    const HOLD: &str = "src::b.approved + 1h & ~b.rejected";

    #[test]
    fn terminal_ready_absorbs_late_negative_dependency() {
        let outcome = walk(
            READY_FIRST,
            &[("b.ready", iso(3600)), ("b.rejected", iso(1800))],
            &iso(0),
        )
        .expect("replay must succeed");
        assert_eq!(outcome.status, STATUS_READY);
    }

    #[test]
    fn negative_dependency_wins_when_it_arrives_first() {
        let outcome = walk(
            READY_FIRST,
            &[("b.rejected", iso(3600)), ("b.ready", iso(1800))],
            &iso(0),
        )
        .expect("replay must succeed");
        assert_eq!(outcome.status, STATUS_CXL);
    }

    #[test]
    fn maturity_before_late_negative_adjudicates_terminal_ready() {
        let outcome = walk(
            HOLD,
            &[("b.approved", iso(3 * 3600)), ("b.rejected", iso(3600))],
            &iso(0),
        )
        .expect("replay must succeed");
        assert_eq!(outcome.status, STATUS_READY);
    }

    #[test]
    fn negative_before_maturity_adjudicates_cxl_at_arrival() {
        let outcome = walk(
            HOLD,
            &[("b.approved", iso(3 * 3600)), ("b.rejected", iso(150 * 60))],
            &iso(0),
        )
        .expect("replay must succeed");
        assert_eq!(outcome.status, STATUS_CXL);
    }

    #[test]
    fn unexpired_wait_closes_as_wait_at_now() {
        let outcome = walk(
            "src::b.approved + 1h",
            &[("b.approved", iso(1800))],
            &iso(0),
        )
        .expect("replay must succeed");
        assert_eq!(outcome.status, STATUS_WAIT);
    }

    #[test]
    fn matured_wait_without_poke_closes_terminal_at_now() {
        let outcome = walk(
            "src::b.approved + 1h",
            &[("b.approved", iso(3 * 3600))],
            &iso(0),
        )
        .expect("replay must succeed");
        assert_eq!(outcome.status, STATUS_READY);
    }

    #[test]
    fn empty_fact_log_stays_non_terminal() {
        let outcome = walk(READY_FIRST, &[], &iso(0)).expect("replay must succeed");
        assert_eq!(outcome.status, STATUS_INIT);
    }

    #[test]
    fn facts_beyond_dependency_set_do_not_change_the_verdict() {
        let outcome = walk(
            READY_FIRST,
            &[("b.ready", iso(3600)), ("b.unrelated", iso(1800))],
            &iso(0),
        )
        .expect("replay must succeed");
        assert_eq!(outcome.status, STATUS_READY);
    }

    #[test]
    fn subscription_mode_short_circuits_to_ready() {
        let outcome = replay_compiled_hook(CloudReplayRequest {
            ast: cloud_ast("::ANCHOR(@src::b.signal)"),
            facts: vec![],
            now: iso(0),
        })
        .expect("subscription replay must succeed");
        assert_eq!(outcome.status, STATUS_READY);
    }

    #[test]
    fn re_arrival_is_inert_for_the_pending_window() {
        let outcome = walk(
            "src::b.approved + 2h",
            &[("b.approved", iso(3600)), ("b.approved", iso(1800))],
            &iso(0),
        )
        .expect("replay must succeed");
        assert_eq!(outcome.status, STATUS_WAIT);
    }

    #[test]
    fn malformed_ast_fails_loudly() {
        let result = replay_compiled_hook(CloudReplayRequest {
            ast: json!({"schemaVersion": "uvp.cloudAst.v1", "mode": "normal"}),
            facts: vec![],
            now: iso(0),
        });
        assert!(result.is_err());
    }

    #[test]
    fn malformed_fact_identity_fails_loudly() {
        let result = walk(
            READY_FIRST,
            &[("not-a-two-part-signal", iso(3600))],
            &iso(0),
        );
        assert!(result.is_err());
    }

    #[test]
    fn malformed_timestamp_fails_loudly() {
        let result = walk(
            READY_FIRST,
            &[("b.ready", "not-a-timestamp".to_string())],
            &iso(0),
        );
        assert!(result.is_err());
    }
}
