//! 状态与时间纪元：状态 =（已落地事实各自 的纪元，已点火守卫位，当前
//! 纪元）；纪元是相对基准秒的整数，落料与求值都经 uvp-hook-dsl 的真求值
//! 器以具体时间戳进行——时间抽象只存在于编码层，语义层无双源。

use std::collections::BTreeMap;

use chrono::{DateTime, TimeZone, Utc};
use uvp_hook_dsl::{DecodedCompiledHook, EvalState, HookMode};

use crate::vocab::Vocabulary;

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
    pub fired: Vec<bool>,
    pub now: i64,
}

pub type SignalTimes = BTreeMap<String, DateTime<Utc>>;

impl McState {
    pub fn initial(vocab: &Vocabulary) -> Self {
        McState {
            fact_epochs: vec![None; vocab.facts.len()],
            fired: vec![false; vocab.guards.len()],
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

pub fn eval_decoded(
    decoded: &DecodedCompiledHook,
    times: &SignalTimes,
    now: i64,
) -> Option<EvalState> {
    decoded
        .eval(times, epoch_to_dt(now))
        .ok()
        .map(|outcome| outcome.state)
}

pub fn eval_decided_or_default(
    decoded: &DecodedCompiledHook,
    times: &SignalTimes,
    now: i64,
) -> bool {
    eval_decoded(decoded, times, now) == Some(EvalState::Ready)
}

pub fn settle(vocab: &Vocabulary, scope: &[bool], state: &mut McState, times: &mut SignalTimes) {
    loop {
        let mut changed = false;
        for (index, guard) in vocab.guards.iter().enumerate() {
            if state.fired[index] {
                continue;
            }
            if eval_decoded(&guard.decoded, times, state.now) == Some(EvalState::Ready) {
                state.fired[index] = true;
                changed = true;
            }
        }
        for fact in 0..vocab.facts.len() {
            if !scope[fact] || state.fact_epochs[fact].is_some() {
                continue;
            }
            if vocab.intent_fact.contains_key(&fact) {
                continue;
            }
            match vocab.effects.get(&fact) {
                Some(effect) if !effect.env_timed && effect_open(vocab, state, fact) => {
                    state.fact_epochs[fact] = Some(state.now);
                    let name = vocab.facts[fact]
                        .split_once("::")
                        .map(|(_, signal)| signal)
                        .unwrap_or(&vocab.facts[fact]);
                    times.insert(name.to_string(), epoch_to_dt(state.now));
                    changed = true;
                }
                _ => {}
            }
        }
        if !changed {
            return;
        }
    }
}

pub fn next_deadline_epoch(vocab: &Vocabulary, times: &SignalTimes, now: i64) -> Option<i64> {
    let mut best: Option<i64> = None;
    let now_dt = epoch_to_dt(now);
    for site in &vocab.delay_sites {
        let operand = DecodedCompiledHook {
            mode: HookMode::Normal,
            source: site.source.clone(),
            expr: site.operand.clone(),
        };
        let Ok(outcome) = operand.eval(times, now_dt) else {
            continue;
        };
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
    best
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Mode {
    Full,
    TimeOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Land { fact: usize },
    Advance { to_epoch: i64 },
}

pub fn effect_open(vocab: &Vocabulary, state: &McState, fact: usize) -> bool {
    let Some(effect) = vocab.effects.get(&fact) else {
        return false;
    };
    if effect
        .unless_facts
        .iter()
        .any(|blocked| state.fact_epochs[*blocked].is_some())
    {
        return false;
    }
    if !effect
        .all_facts
        .iter()
        .all(|required| state.fact_epochs[*required].is_some())
    {
        return false;
    }
    if !effect.any_guards.is_empty() && !effect.any_guards.iter().any(|guard| state.fired[*guard]) {
        return false;
    }
    if !effect.any_facts.is_empty()
        && !effect
            .any_facts
            .iter()
            .any(|required| state.fact_epochs[*required].is_some())
    {
        return false;
    }
    true
}

pub fn intent_admissible(
    vocab: &Vocabulary,
    intent_index: usize,
    times: &SignalTimes,
    now: i64,
) -> bool {
    let intent = &vocab.intents[intent_index];
    eval_decoded(&intent.decoded, times, now) == Some(EvalState::Ready)
}

pub fn enabled_actions(
    vocab: &Vocabulary,
    scope: &[bool],
    state: &McState,
    times: &SignalTimes,
    mode: Mode,
) -> Vec<Action> {
    let mut out = Vec::new();
    if mode == Mode::Full {
        for (fact, _) in state.fact_epochs.iter().enumerate() {
            if !scope[fact] || state.fact_epochs[fact].is_some() {
                continue;
            }
            match vocab.intent_fact.get(&fact) {
                Some(intent) => {
                    if intent_admissible(vocab, *intent, times, state.now) {
                        out.push(Action::Land { fact });
                    }
                }
                None => match vocab.effects.get(&fact) {
                    Some(effect) if effect.env_timed && effect_open(vocab, state, fact) => {
                        out.push(Action::Land { fact });
                    }
                    Some(_) => {}
                    None => out.push(Action::Land { fact }),
                },
            }
        }
    }
    if let Some(to_epoch) = next_deadline_epoch(vocab, times, state.now) {
        out.push(Action::Advance { to_epoch });
    }
    out
}

pub fn apply_action(
    vocab: &Vocabulary,
    scope: &[bool],
    state: &McState,
    times: &SignalTimes,
    action: Action,
) -> McState {
    let mut next = state.clone();
    let mut next_times = times.clone();
    match action {
        Action::Land { fact } => {
            next.fact_epochs[fact] = Some(next.now);
            let name = vocab.facts[fact]
                .split_once("::")
                .map(|(_, signal)| signal)
                .unwrap_or(&vocab.facts[fact]);
            next_times.insert(name.to_string(), epoch_to_dt(next.now));
        }
        Action::Advance { to_epoch } => {
            next.now = to_epoch;
        }
    }
    settle(vocab, scope, &mut next, &mut next_times);
    next
}
