//! 不变量检查：bad_state（坏状态谓词）、admission_closed（终态关断适格
//! 位）、deadlock_free（有人能动∨截止将至）、coreach（谓词可达）。违反
//! 给最短事实序列反例。每项检查的探索宇宙由其原子的适格闭包派生——
//! 闭包外原子不参与任何相关求值，窄宇宙与全宇宙在性质上等价；意图关
//! 断按意图逐个独立探索再合并。

use serde_json::{json, Value};
use uvp_hook_dsl::EvalState;

use crate::checks::{Check, CheckKind};
use crate::explore::{Explorer, Graph};
use crate::model::{self, Action, McState};
use crate::vocab::{signal_atoms, Vocabulary};
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
        let vocab = self.vocab;
        let explorer = &mut self.explorer;
        let max_states = self.max_states;
        let mut outcome = match check.kind {
            CheckKind::BadState => bad_state(vocab, explorer, max_states, check)?,
            CheckKind::AdmissionClosed => admission_closed(vocab, explorer, max_states, check)?,
            CheckKind::DeadlockFree => deadlock_free(vocab, explorer, max_states, check)?,
            CheckKind::CoReach => coreach(vocab, explorer, max_states, check)?,
        };
        if outcome.status == Status::Fail {
            if let Some(violation) = outcome.violation.as_mut() {
                violation["check_id"] = Value::String(check.id.clone());
            }
        }
        Ok(outcome)
    }

    /// 死意图：其适格闭包宇宙内从未适格——适格自相矛盾或依赖永不可落
    /// 的意图链，信号永不可发。反例不存在（全称失败），报告意图本身。
    pub fn run_dead_intent(&mut self, fact: usize) -> Result<CheckOutcome> {
        let name = self.vocab.facts[fact].clone();
        let &intent = self
            .vocab
            .intent_fact
            .get(&fact)
            .expect("dead-intent probe is only called on intents");
        let universe: Vec<usize> = self.vocab.positive_closure(&[fact]).into_iter().collect();
        let graph = self.explorer.graph(&universe, self.max_states)?;
        let mut alive = false;
        for state in &graph.states {
            if state.fact_epochs[fact].is_some() {
                continue;
            }
            let times = state.signal_times(self.vocab);
            if model::intent_admissible(self.vocab, intent, &times, state.now)? {
                alive = true;
                break;
            }
        }
        let violation = if alive {
            None
        } else {
            Some(json!({
                "reason": "intent is never admissible in any reachable state: its admission is self-contradictory or depends on intents that can never land",
                "intent": name,
            }))
        };
        Ok(CheckOutcome {
            id: format!("dead-intent:{name}"),
            kind: CheckKind::BadState,
            status: if violation.is_some() {
                Status::Fail
            } else {
                Status::Pass
            },
            states: graph.states.len(),
            violation,
        })
    }
}

fn predicate_holds(vocab: &Vocabulary, raw: Option<&str>, state: &McState) -> Result<bool> {
    let Some(raw) = raw else { return Ok(true) };
    let Some(decoded) = vocab.predicates.get(raw) else {
        return Err(McError::Message(format!(
            "predicate {raw:?} was not parsed during vocabulary build"
        )));
    };
    let times = state.signal_times(vocab);
    Ok(model::eval_state(decoded, &times, state.now)? == EvalState::Ready)
}

fn predicate_atoms(vocab: &Vocabulary, raw: &str) -> Vec<usize> {
    vocab
        .predicates
        .get(raw)
        .map(|decoded| signal_atoms(&decoded.expr, &decoded.source))
        .unwrap_or_default()
        .iter()
        .filter_map(|atom| vocab.fact_index.get(atom).copied())
        .collect()
}

fn bad_state(
    vocab: &Vocabulary,
    explorer: &mut Explorer,
    max_states: u64,
    check: &Check,
) -> Result<CheckOutcome> {
    let predicate = check.predicate.as_deref().expect("validated");
    let when = check.when.as_deref();
    let mut seeds = predicate_atoms(vocab, predicate);
    if let Some(when) = when {
        seeds.extend(predicate_atoms(vocab, when));
    }
    let universe: Vec<usize> = vocab.positive_closure(&seeds).into_iter().collect();
    let graph = explorer.graph(&universe, max_states)?;
    let mut violation = None;
    for (position, state) in graph.states.iter().enumerate() {
        if !predicate_holds(vocab, when, state)? {
            continue;
        }
        if predicate_holds(vocab, Some(predicate), state)? {
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
) -> Result<CheckOutcome> {
    let when = check.when.as_deref().expect("validated");
    let when_atoms = predicate_atoms(vocab, when);
    let mut violation = None;
    let mut states = 0usize;
    'intents: for intent_name in &check.intents {
        let fact = vocab.fact_index.get(intent_name).copied().ok_or_else(|| {
            McError::Message(format!("intent {intent_name:?} is not in the vocabulary"))
        })?;
        let Some(&intent) = vocab.intent_fact.get(&fact) else {
            return Err(McError::Message(format!(
                "intent {intent_name:?} has no admission in the compiled artifact; only gated signals can be checked for closure"
            )));
        };
        let mut seeds = when_atoms.clone();
        seeds.push(fact);
        let universe: Vec<usize> = vocab.positive_closure(&seeds).into_iter().collect();
        let graph = explorer.graph(&universe, max_states)?;
        states = states.max(graph.states.len());
        for (position, state) in graph.states.iter().enumerate() {
            if state.fact_epochs[fact].is_some() {
                continue;
            }
            if !predicate_holds(vocab, Some(when), state)? {
                continue;
            }
            let times = state.signal_times(vocab);
            if model::intent_admissible(vocab, intent, &times, state.now)? {
                violation = Some(violation_json(
                    vocab,
                    graph,
                    position,
                    format!("intent {intent_name} is still admissible at a state the when-clause closed"),
                ));
                break 'intents;
            }
        }
    }
    Ok(outcome(check, states, violation))
}

fn deadlock_free(
    vocab: &Vocabulary,
    explorer: &mut Explorer,
    max_states: u64,
    check: &Check,
) -> Result<CheckOutcome> {
    let when = check.when.as_deref().expect("validated");
    let mut seeds = predicate_atoms(vocab, when);
    seeds.extend(vocab.intent_facts());
    let universe: Vec<usize> = vocab.closure(&seeds).into_iter().collect();
    let graph = explorer.graph(&universe, max_states)?;
    let mut violation = None;
    for (position, state) in graph.states.iter().enumerate() {
        if !predicate_holds(vocab, Some(when), state)? {
            continue;
        }
        let times = state.signal_times(vocab);
        let mut sanctioned = model::next_deadline_epoch(vocab, &times, state.now)?.is_some();
        for fact in &universe {
            if state.fact_epochs[*fact].is_some() {
                continue;
            }
            let Some(&intent) = vocab.intent_fact.get(fact) else {
                continue;
            };
            if model::intent_admissible(vocab, intent, &times, state.now)? {
                sanctioned = true;
                break;
            }
        }
        if !sanctioned {
            violation = Some(violation_json(
                vocab,
                graph,
                position,
                "no admissible intent and no pending deadline can move this state".to_string(),
            ));
            break;
        }
    }
    Ok(outcome(check, graph.states.len(), violation))
}

fn coreach(
    vocab: &Vocabulary,
    explorer: &mut Explorer,
    max_states: u64,
    check: &Check,
) -> Result<CheckOutcome> {
    let predicate = check.predicate.as_deref().expect("validated");
    let mut seeds = predicate_atoms(vocab, predicate);
    seeds.extend(vocab.intent_facts());
    let universe: Vec<usize> = vocab.closure(&seeds).into_iter().collect();
    let graph = explorer.graph(&universe, max_states)?;
    let covered = backward_cover_edges(graph, &graph.edges, |state| {
        predicate_holds(vocab, Some(predicate), state)
    })?;
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

fn backward_cover_edges<F>(
    graph: &Graph,
    edges: &[Vec<usize>],
    target: F,
) -> Result<Vec<bool>>
where
    F: Fn(&McState) -> Result<bool>,
{
    let mut covered = vec![false; graph.states.len()];
    let mut queue = std::collections::VecDeque::new();
    for (position, state) in graph.states.iter().enumerate() {
        if target(state)? {
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
    Ok(covered)
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
    }
    Ok(())
}
