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

/// 成熟决策点相对 due 的推进量。kernel 的成熟判定是闭不等式 now >= due，
/// 与事实到达时刻同刻并列时因果序不可分辨；+1ms 把成熟决策点显式排到
/// due 之后，"先成熟后到达"与"先到达后成熟"才可判定。事实与期限的
/// 时间源都是毫秒精度，+1ms 严格落在两相邻毫秒之间。
const MATURITY_DECISION_SLACK_MS: i64 = 1;

#[derive(Debug, Deserialize)]
// 未知字段确定性拒绝（与链轨请求信封同口径）：拼错的调用方输入不得被
// 静默忽略成缺省语义。
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct CloudReplayRequest {
    /// 云侧编译产物 AST（uvp.cloudAst.v1，与 eval_compiled_hook 的 ast 同形态）。
    pub ast: serde_json::Value,
    /// 因果事实日志（裸信号名 + 到达时刻），调用方按受理序排好；走带
    /// 不重排——受理序就是裁决序。
    #[serde(default)]
    pub facts: Vec<CloudReplayFact>,
    /// 重放时刻（RFC3339）：非终态以此收口（poke 口径）。
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
    /// hook_state 词表口径的期望状态（ready/wait/cxl/init）。
    pub status: &'static str,
}

pub const STATUS_READY: &str = "ready";
pub const STATUS_WAIT: &str = "wait";
pub const STATUS_CXL: &str = "cxl";
pub const STATUS_INIT: &str = "init";

/// 重放单个编译钩子：因果走带后给出期望状态。任何一步求值/解码失败
/// 都整体报错——调用方把"无法重放"按 unknown 期望暴露，不折成某个
/// 状态。
pub fn replay_compiled_hook(request: CloudReplayRequest) -> Result<CloudReplayOutcome> {
    let decoded = decode_compiled_hook(&request.ast, Gate::Hook)
        .map_err(|err| ReplayError::Message(err.to_string()))?;
    if decoded.mode == HookMode::Subscription {
        // 订阅入口不做表达式裁决：每条到达事实本身即是就绪事件，首事件
        // 即 ready（事实日志在此形态下不参与裁决）。
        return Ok(CloudReplayOutcome {
            status: STATUS_READY,
        });
    }
    let now = parse_cloud_time(&request.now)?;
    let mut facts = Vec::with_capacity(request.facts.len());
    for fact in &request.facts {
        if !valid_signal_identity(&fact.signal_name) {
            return Err(ReplayError::Message(format!(
                "replay fact signal_name must use task.stage.signal and be at most 100 characters: {:?}",
                fact.signal_name
            )));
        }
        facts.push((fact.signal_name.clone(), parse_cloud_time(&fact.arrived_at)?));
    }

    let mut signals: BTreeMap<String, DateTime<Utc>> = BTreeMap::new();
    let mut status = STATUS_INIT;
    let mut terminal = false;
    for (index, (name, arrived_at)) in facts.iter().enumerate() {
        if terminal {
            break;
        }
        // 决策视界：下一事实的到达时刻（末条事实以重放时刻为界）——视界
        // 内到期的 wait 必须先按成熟点落定，后到事实才进入裁决。
        let horizon = if index + 1 < facts.len() {
            facts[index + 1].1
        } else {
            now
        };
        // 同名信号的再次到达覆盖到达时刻（在线事实表每名取最新到达）。
        signals.insert(name.clone(), *arrived_at);
        let mut step = eval_step(&decoded, &signals, *arrived_at, &mut status, &mut terminal)?;
        // 成熟决策点：wait 的期限不晚于视界时，先按 poke 口径在成熟点
        // 重算。wait 态下 kernel 给出的期限严格晚于求值时刻，重算后的
        // 期限必然推进、不会驻留。
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
        // 非终态按重放时刻收口：wait 到期由 poke 重算、晚到事实在信号
        // 事务里重算——非终态下聚合口径与因果口径一致。
        eval_step(&decoded, &signals, now, &mut status, &mut terminal)?;
    }
    Ok(CloudReplayOutcome { status })
}

/// 单点求值并落状态：ready/cxl 是语义终态，吸收后续事实（终态之后的
/// 到达只留审计记录，不改变裁决）。
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

/// 云轨时间口径：RFC3339 原文解析（不截断到秒），毫秒及以下精度保留。
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
        (Utc::now() - chrono::Duration::seconds(seconds_ago)).to_rfc3339_opts(
            chrono::SecondsFormat::Millis,
            true,
        )
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

    const READY_FIRST: &str = "src::a.b.ready & ~a.b.rejected";
    const HOLD: &str = "src::a.b.approved + 1h & ~a.b.rejected";

    #[test]
    fn terminal_ready_absorbs_late_negative_dependency() {
        // 先到先成立（互斥结果按接收时间判定）：ready 先落终态，晚到的
        // 负依赖不翻案。
        let outcome = walk(
            READY_FIRST,
            &[
                ("a.b.ready", iso(3600)),
                ("a.b.rejected", iso(1800)),
            ],
            &iso(0),
        )
        .expect("replay must succeed");
        assert_eq!(outcome.status, STATUS_READY);
    }

    #[test]
    fn negative_dependency_wins_when_it_arrives_first() {
        // 因果序敏感：同一对事实反序到达，负依赖先到即判 cxl。
        let outcome = walk(
            READY_FIRST,
            &[
                ("a.b.rejected", iso(3600)),
                ("a.b.ready", iso(1800)),
            ],
            &iso(0),
        )
        .expect("replay must succeed");
        assert_eq!(outcome.status, STATUS_CXL);
    }

    #[test]
    fn maturity_before_late_negative_adjudicates_terminal_ready() {
        // 成熟决策点在视界内先行落定：approved@-3h → wait(-2h) → 成熟
        // 判 ready（终态）→ rejected@-1h 被吸收。
        let outcome = walk(
            HOLD,
            &[
                ("a.b.approved", iso(3 * 3600)),
                ("a.b.rejected", iso(3600)),
            ],
            &iso(0),
        )
        .expect("replay must succeed");
        assert_eq!(outcome.status, STATUS_READY);
    }

    #[test]
    fn negative_before_maturity_adjudicates_cxl_at_arrival() {
        // 负依赖先于成熟到达（-2.5h 早于期限 -2h）：到达时刻即判 cxl，
        // 成熟点不再翻案。
        let outcome = walk(
            HOLD,
            &[
                ("a.b.approved", iso(3 * 3600)),
                ("a.b.rejected", iso(150 * 60)),
            ],
            &iso(0),
        )
        .expect("replay must succeed");
        assert_eq!(outcome.status, STATUS_CXL);
    }

    #[test]
    fn unexpired_wait_closes_as_wait_at_now() {
        // 期限晚于重放时刻：非终态按 now 收口仍是 wait。
        let outcome = walk(
            "src::a.b.approved + 1h",
            &[("a.b.approved", iso(1800))],
            &iso(0),
        )
        .expect("replay must succeed");
        assert_eq!(outcome.status, STATUS_WAIT);
    }

    #[test]
    fn matured_wait_without_poke_closes_terminal_at_now() {
        // 期限已过但事实流停在到期前：按 now 收口判终态（在线由 poke 落
        // 定，重放以重放时刻补上该决策点）。
        let outcome = walk(
            "src::a.b.approved + 1h",
            &[("a.b.approved", iso(3 * 3600))],
            &iso(0),
        )
        .expect("replay must succeed");
        assert_eq!(outcome.status, STATUS_READY);
    }

    #[test]
    fn empty_fact_log_stays_non_terminal() {
        // 零事实：互等（init）——正锚缺席，负项的伪就绪不足以落终态。
        let outcome = walk(READY_FIRST, &[], &iso(0)).expect("replay must succeed");
        assert_eq!(outcome.status, STATUS_INIT);
    }

    #[test]
    fn facts_beyond_dependency_set_do_not_change_the_verdict() {
        // 事实集与依赖集合的交集口径由调用方负责；走带只按喂入的名字
        // 求值——无关名字进日志不改变裁决（每名独立入表）。
        let outcome = walk(
            READY_FIRST,
            &[
                ("a.b.ready", iso(3600)),
                ("a.b.unrelated", iso(1800)),
            ],
            &iso(0),
        )
        .expect("replay must succeed");
        assert_eq!(outcome.status, STATUS_READY);
    }

    #[test]
    fn subscription_mode_short_circuits_to_ready() {
        let outcome = replay_compiled_hook(CloudReplayRequest {
            ast: cloud_ast("::ANCHOR(@src::a.b.signal)"),
            facts: vec![],
            now: iso(0),
        })
        .expect("subscription replay must succeed");
        assert_eq!(outcome.status, STATUS_READY);
    }

    #[test]
    fn re_arrival_is_inert_for_the_pending_window() {
        // 同名信号再次到达取最新时刻：首个窗口未到视界时不成熟，重放
        // 以最新锚点收口（与在线事实表每名取最新到达同口径）。
        let outcome = walk(
            "src::a.b.approved + 2h",
            &[
                ("a.b.approved", iso(3600)),
                ("a.b.approved", iso(1800)),
            ],
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
            &[("not-a-three-part-signal", iso(3600))],
            &iso(0),
        );
        assert!(result.is_err());
    }

    #[test]
    fn malformed_timestamp_fails_loudly() {
        let result = walk(
            READY_FIRST,
            &[("a.b.ready", "not-a-timestamp".to_string())],
            &iso(0),
        );
        assert!(result.is_err());
    }
}
