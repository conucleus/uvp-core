//! 状态与时间纪元：状态 =（各事实的落地纪元，当前纪元）。意图事实只在
//! 适格求值为 Ready 时可落；无适格的事实自由落地——世界可观测任何事
//! 实，定义不编码其因果。时间只推进到下一个待决截止。一切求值经
//! uvp-hook-dsl 的真求值器以具体时间戳进行，语义层无双源；求值错误响
//! 亮上抛，绝不折算成「未就绪」。

use std::collections::BTreeMap;

use chrono::{DateTime, TimeZone, Utc};
use uvp_hook_dsl::{DecodedCompiledHook, EvalState, HookMode};

use crate::vocab::Vocabulary;
use crate::{McError, Result};

pub const EPOCH_BASE_SECS: i64 = 1735689600;

pub fn epoch_to_dt(epoch: i64) -> DateTime<Utc> {
    Utc.timestamp_opt(EPOCH_BASE_SECS + epoch, 0)
        .single()
        .expect("model-check epochs stay inside the DateTime range")
}

pub fn dt_to_epoch(dt: DateTime<Utc>) -> i64 {
    dt.timestamp() - EPOCH_BASE_SECS
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct McState {
    pub fact_epochs: Vec<Option<i64>>,
    pub now: i64,
}

pub type SignalTimes = BTreeMap<String, DateTime<Utc>>;

impl McState {
    pub fn initial(vocab: &Vocabulary) -> Self {
        McState {
            fact_epochs: vec![None; vocab.facts.len()],
            now: 0,
        }
    }

    pub fn signal_times(&self, vocab: &Vocabulary) -> SignalTimes {
        let mut times = BTreeMap::new();
        for (index, epoch) in self.fact_epochs.iter().enumerate() {
            let Some(epoch) = epoch else { continue };
            let name = vocab.facts[index]
                .split_once("::")
                .map(|(_, signal)| signal)
                .unwrap_or(&vocab.facts[index]);
            times.insert(name.to_string(), epoch_to_dt(*epoch));
        }
        times
    }

    pub fn landed_facts(&self) -> Vec<(usize, i64)> {
        let mut out = Vec::new();
        for (index, epoch) in self.fact_epochs.iter().enumerate() {
            if let Some(epoch) = epoch {
                out.push((index, *epoch));
            }
        }
        out
    }
}

pub fn eval_state(
    decoded: &DecodedCompiledHook,
    times: &SignalTimes,
    now: i64,
) -> Result<EvalState> {
    decoded
        .eval(times, epoch_to_dt(now))
        .map(|outcome| outcome.state)
        .map_err(|err| McError::Message(format!("evaluate {err}")))
}

pub fn intent_admissible(
    vocab: &Vocabulary,
    intent_index: usize,
    times: &SignalTimes,
    now: i64,
) -> Result<bool> {
    Ok(eval_state(&vocab.intents[intent_index].decoded, times, now)? == EvalState::Ready)
}

pub fn next_deadline_epoch(vocab: &Vocabulary, times: &SignalTimes, now: i64) -> Result<Option<i64>> {
    let mut best: Option<i64> = None;
    let now_dt = epoch_to_dt(now);
    for site in &vocab.delay_sites {
        let operand = DecodedCompiledHook {
            mode: HookMode::Normal,
            source: site.source.clone(),
            expr: site.operand.clone(),
        };
        let outcome = operand
            .eval(times, now_dt)
            .map_err(|err| McError::Message(format!("evaluate delay operand: {err}")))?;
        if outcome.state != EvalState::Ready {
            continue;
        }
        let Some(ready_at) = outcome.ready_at else {
            continue;
        };
        let candidate = dt_to_epoch(ready_at) + site.duration_seconds;
        if candidate > now {
            best = Some(best.map_or(candidate, |current: i64| current.min(candidate)));
        }
    }
    Ok(best)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Land { fact: usize },
    Advance { to_epoch: i64 },
}

pub fn enabled_actions(
    vocab: &Vocabulary,
    universe: &[bool],
    state: &McState,
    times: &SignalTimes,
) -> Result<Vec<Action>> {
    let mut out = Vec::new();
    for (fact, epoch) in state.fact_epochs.iter().enumerate() {
        if !universe[fact] || epoch.is_some() {
            continue;
        }
        match vocab.intent_fact.get(&fact) {
            Some(&intent) => {
                if intent_admissible(vocab, intent, times, state.now)? {
                    out.push(Action::Land { fact });
                }
            }
            None => out.push(Action::Land { fact }),
        }
    }
    if let Some(to_epoch) = next_deadline_epoch(vocab, times, state.now)? {
        out.push(Action::Advance { to_epoch });
    }
    Ok(out)
}

pub fn apply_action(state: &McState, action: Action) -> McState {
    let mut next = state.clone();
    match action {
        Action::Land { fact } => {
            next.fact_epochs[fact] = Some(next.now);
        }
        Action::Advance { to_epoch } => {
            next.now = to_epoch;
        }
    }
    next
}
