//! 不变量检查：bad_state（坏状态谓词）、admission_closed（终态关断适格
//! 位）、deadlock_free（有人能动∨截止将至∨在途指令）、coreach（终态可
//! 达）、time_only_closure（钱不无故沉睡：仅时间推进与在途完成也能收敛）。
//! 违反给最短事实序列反例。

use serde_json::{json, Value};

use crate::explore::{Explorer, Graph};
use crate::manifest::{Check, CheckKind};
use crate::model::{self, Action, McState, Mode, SignalTimes};
use crate::vocab::Vocabulary;
use crate::{McError, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Pass,
    Fail,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub struct CheckOutcome {
    pub id: String,
    pub kind: CheckKind,
    pub status: Status,
    pub states: usize,
    pub violation: Option<Value>,
}

pub struct Checker<'v> {
    vocab: &'v Vocabulary,
    explorer: Explorer<'v>,
    max_states: u64,
}

impl<'v> Checker<'v> {
    pub fn new(vocab: &'v Vocabulary, max_states: u64) -> Self {
        Checker {
            vocab,
            explorer: Explorer::new(vocab),
            max_states,
        }
    }

    pub fn run(&mut self, check: &Check) -> Result<CheckOutcome> {
        validate_shape(check)?;
        let scope = scope_indices(self.vocab, &check.scope)?;
        let vocab = self.vocab;
        let explorer = &mut self.explorer;
        let max_states = self.max_states;
        let mut outcome = match check.kind {
            CheckKind::BadState => bad_state(vocab, explorer, max_states, check, &scope)?,
            CheckKind::AdmissionClosed => {
                admission_closed(vocab, explorer, max_states, check, &scope)?
            }
            CheckKind::DeadlockFree => deadlock_free(vocab, explorer, max_states, check, &scope)?,
            CheckKind::CoReach => coreach(vocab, explorer, max_states, check, &scope)?,
            CheckKind::TimeOnlyClosure => {
                time_only_closure(vocab, explorer, max_states, check, &scope)?
            }
        };
        if outcome.status == Status::Fail {
            if let Some(violation) = outcome.violation.as_mut() {
                violation["check_id"] = Value::String(check.id.clone());
            }
        }
        Ok(outcome)
    }
}

fn bad_state(
    vocab: &Vocabulary,
    explorer: &mut Explorer,
    max_states: u64,
    check: &Check,
    scope: &[usize],
) -> Result<CheckOutcome> {
    let graph = explorer.graph(scope, Mode::Full, max_states)?;
    let predicate = check.predicate.as_deref().expect("validated");
    let when = check.when.as_deref();
    let mut violation = None;
    for (position, state) in graph.states.iter().enumerate() {
        if !predicate_holds(vocab, when, state) {
            continue;
        }
        if predicate_holds(vocab, Some(predicate), state) {
            violation = Some(violation_json(
                vocab,
                graph,
                position,
                "bad-state predicate is ready at this state".to_string(),
            ));
            break;
        }
    }
    Ok(outcome(check, graph.states.len(), violation))
}

fn admission_closed(
    vocab: &Vocabulary,
    explorer: &mut Explorer,
    max_states: u64,
    check: &Check,
    scope: &[usize],
) -> Result<CheckOutcome> {
    let graph = explorer.graph(scope, Mode::Full, max_states)?;
    let when = check.when.as_deref().expect("validated");
    let mut violation = None;
    'states: for (position, state) in graph.states.iter().enumerate() {
        if !predicate_holds(vocab, Some(when), state) {
            continue;
        }
        let times = state.signal_times(vocab);
        for intent_name in &check.intents {
            let fact = vocab.fact_index.get(intent_name).copied().ok_or_else(|| {
                McError::Message(format!("intent {intent_name:?} is not in the vocabulary"))
            })?;
            if state.fact_epochs[fact].is_some() {
                continue;
            }
            let Some(intent_index) = vocab.intent_fact.get(&fact).copied() else {
                return Err(McError::Message(format!(
                    "intent {intent_name:?} has no admission in the compiled artifact; only gated signals can be checked for closure"
                )));
            };
            if model::intent_admissible(vocab, intent_index, &times, state.now) {
                violation = Some(violation_json(
                    vocab,
                    graph,
                    position,
                    format!(
                        "intent {intent_name} is still admissible at a state the when-clause closed"
                    ),
                ));
                break 'states;
            }
        }
    }
    Ok(outcome(check, graph.states.len(), violation))
}

fn deadlock_free(
    vocab: &Vocabulary,
    explorer: &mut Explorer,
    max_states: u64,
    check: &Check,
    scope: &[usize],
) -> Result<CheckOutcome> {
    let graph = explorer.graph(scope, Mode::Full, max_states)?;
    let when = check.when.as_deref().expect("validated");
    let mut violation = None;
    for (position, state) in graph.states.iter().enumerate() {
        if !predicate_holds(vocab, Some(when), state) {
            continue;
        }
        let times = state.signal_times(vocab);
        let in_flight = scope.iter().any(|fact| {
            state.fact_epochs[*fact].is_none()
                && vocab.effects.get(fact).is_some_and(|effect| {
                    effect.env_timed && model::effect_open(vocab, state, *fact)
                })
        });
        let sanctioned = model::next_deadline_epoch(vocab, &times, state.now).is_some()
            || any_sanctioned_intent(vocab, scope, state, &times)
            || in_flight;
        if !sanctioned {
            violation = Some(violation_json(
                vocab,
                graph,
                position,
                "no sanctioned action, no pending deadline and no in-flight relay can move this state"
                    .to_string(),
            ));
            break;
        }
    }
    Ok(outcome(check, graph.states.len(), violation))
}

fn any_sanctioned_intent(
    vocab: &Vocabulary,
    scope: &[usize],
    state: &McState,
    times: &SignalTimes,
) -> bool {
    scope.iter().any(|fact| {
        state.fact_epochs[*fact].is_none()
            && vocab
                .intent_fact
                .get(fact)
                .is_some_and(|intent| model::intent_admissible(vocab, *intent, times, state.now))
    })
}

fn coreach(
    vocab: &Vocabulary,
    explorer: &mut Explorer,
    max_states: u64,
    check: &Check,
    scope: &[usize],
) -> Result<CheckOutcome> {
    let graph = explorer.graph(scope, Mode::Full, max_states)?;
    let predicate = check.predicate.as_deref().expect("validated");
    let covered = backward_cover_edges(graph, &graph.edges, |state| {
        predicate_holds(vocab, Some(predicate), state)
    });
    let violation = graph
        .states
        .iter()
        .enumerate()
        .find(|(position, _)| !covered[*position])
        .map(|(position, _)| {
            violation_json(
                vocab,
                graph,
                position,
                "no extension from this state reaches the target predicate".to_string(),
            )
        });
    Ok(outcome(check, graph.states.len(), violation))
}

fn time_only_closure(
    vocab: &Vocabulary,
    explorer: &mut Explorer,
    max_states: u64,
    check: &Check,
    scope: &[usize],
) -> Result<CheckOutcome> {
    let graph = explorer.graph(scope, Mode::Full, max_states)?;
    let when = check.when.as_deref().expect("validated");
    let predicate = check.predicate.as_deref().expect("validated");
    let in_scope = scope_mask(vocab, scope);
    let mut restricted_edges: Vec<Vec<usize>> = vec![Vec::new(); graph.states.len()];
    for (position, state) in graph.states.iter().enumerate() {
        let times = state.signal_times(vocab);
        for action in model::enabled_actions(vocab, &in_scope, state, &times, Mode::TimeOnly) {
            let successor = model::apply_action(vocab, &in_scope, state, &times, action);
            if let Some(target) = graph.state_index(&successor) {
                restricted_edges[position].push(target);
            }
        }
    }
    let covered = backward_cover_edges(graph, &restricted_edges, |state| {
        predicate_holds(vocab, Some(predicate), state)
    });
    let violation = graph
        .states
        .iter()
        .enumerate()
        .find(|(position, state)| predicate_holds(vocab, Some(when), state) && !covered[*position])
        .map(|(position, _)| {
            violation_json(
                vocab,
                graph,
                position,
                "time alone (deadline firing plus in-flight completions) never reaches the target from this state"
                    .to_string(),
            )
        });
    Ok(outcome(check, graph.states.len(), violation))
}

fn predicate_holds(vocab: &Vocabulary, raw: Option<&str>, state: &McState) -> bool {
    let Some(raw) = raw else { return true };
    let Some(decoded) = vocab.predicates.get(raw) else {
        return false;
    };
    let times = state.signal_times(vocab);
    model::eval_decided_or_default(decoded, &times, state.now)
}

fn scope_mask(vocab: &Vocabulary, scope: &[usize]) -> Vec<bool> {
    let mut mask = vec![false; vocab.facts.len()];
    for fact in scope {
        mask[*fact] = true;
    }
    mask
}

fn outcome(check: &Check, states: usize, violation: Option<Value>) -> CheckOutcome {
    CheckOutcome {
        id: check.id.clone(),
        kind: check.kind,
        status: if violation.is_some() {
            Status::Fail
        } else {
            Status::Pass
        },
        states,
        violation,
    }
}

fn violation_json(vocab: &Vocabulary, graph: &Graph, position: usize, reason: String) -> Value {
    let state = &graph.states[position];
    let facts: Vec<Value> = state
        .landed_facts()
        .into_iter()
        .map(|(fact, epoch)| json!({"fact": vocab.facts[fact], "epoch": epoch}))
        .collect();
    let trace: Vec<Value> = graph
        .trace_from_root(position)
        .into_iter()
        .map(|(_index, action, state)| match action {
            Action::Land { fact } => json!({
                "land": vocab.facts[fact],
                "at_epoch": state.fact_epochs[fact],
            }),
            Action::Advance { to_epoch } => json!({"advance_to_epoch": to_epoch}),
        })
        .collect();
    json!({
        "reason": reason,
        "state": {"facts": facts, "now": state.now},
        "trace": trace,
    })
}

fn backward_cover_edges<F>(graph: &Graph, edges: &[Vec<usize>], target: F) -> Vec<bool>
where
    F: Fn(&McState) -> bool,
{
    let mut covered = vec![false; graph.states.len()];
    let mut queue = std::collections::VecDeque::new();
    for (position, state) in graph.states.iter().enumerate() {
        if target(state) {
            covered[position] = true;
            queue.push_back(position);
        }
    }
    let mut reverse: Vec<Vec<usize>> = vec![Vec::new(); graph.states.len()];
    for (source, targets) in edges.iter().enumerate() {
        for target in targets {
            reverse[*target].push(source);
        }
    }
    while let Some(current) = queue.pop_front() {
        for predecessor in &reverse[current] {
            if !covered[*predecessor] {
                covered[*predecessor] = true;
                queue.push_back(*predecessor);
            }
        }
    }
    covered
}

fn validate_shape(check: &Check) -> Result<()> {
    let missing = |field: &str| {
        McError::Message(format!(
            "check {} ({}): {field} is required for this kind",
            check.id,
            check.kind.as_str()
        ))
    };
    match check.kind {
        CheckKind::BadState => {
            if check.predicate.is_none() {
                return Err(missing("predicate"));
            }
        }
        CheckKind::AdmissionClosed => {
            if check.when.is_none() {
                return Err(missing("when"));
            }
            if check.intents.is_empty() {
                return Err(missing("intents"));
            }
        }
        CheckKind::DeadlockFree => {
            if check.when.is_none() {
                return Err(missing("when"));
            }
        }
        CheckKind::CoReach => {
            if check.predicate.is_none() {
                return Err(missing("predicate"));
            }
        }
        CheckKind::TimeOnlyClosure => {
            if check.predicate.is_none() {
                return Err(missing("predicate"));
            }
            if check.when.is_none() {
                return Err(missing("when"));
            }
        }
    }
    Ok(())
}

pub fn scope_indices(vocab: &Vocabulary, scope: &[String]) -> Result<Vec<usize>> {
    if scope.is_empty() {
        return Ok((0..vocab.facts.len()).collect());
    }
    let mut out = Vec::with_capacity(scope.len());
    for fact in scope {
        out.push(vocab.fact_index.get(fact).copied().ok_or_else(|| {
            McError::Message(format!("scope fact {fact:?} is not in the vocabulary"))
        })?);
    }
    out.sort_unstable();
    out.dedup();
    Ok(out)
}
