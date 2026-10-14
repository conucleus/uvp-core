//! 穷举探索器：按探索宇宙做带记忆化的 BFS。去重键用规范化状态——任何
//! 求值只经由延时位点读事实纪元，不被任何可能点火位点读取的纪元是不可
//! 观伪影，折算为 0 后与原状态互模拟。弹出即判定：可携停机条件（违反/
//! 满足命中即终止，不必穷尽全图）与剪枝条件（见证必经意图被永久关死的
//! 子树整枝剪除）。超上限响亮报错，不静默截断。

use std::collections::{BTreeSet, HashMap, VecDeque};

use crate::model::{apply_action, enabled_actions, McState};
use crate::vocab::{signal_atoms, Vocabulary};
use crate::{McError, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopKind {
    /// 命中违反：立即终止，该状态即反例。
    Violation,
    /// 命中满足：立即终止，见证成立。
    Satisfied,
}

#[derive(Debug, Clone)]
pub struct Graph {
    pub states: Vec<McState>,
    pub parents: Vec<Option<(usize, crate::model::Action)>>,
    pub stopped: Option<(usize, StopKind)>,
}

impl Graph {
    pub fn trace_from_root(
        &self,
        state_index: usize,
    ) -> Vec<(usize, crate::model::Action, &McState)> {
        let mut path = Vec::new();
        let mut cursor = state_index;
        while let Some((parent, action)) = self.parents[cursor] {
            path.push((cursor, action, &self.states[cursor]));
            cursor = parent;
        }
        path.reverse();
        path
    }
}

pub type StopCondition<'a> = dyn Fn(&McState) -> Result<Option<StopKind>> + 'a;
pub type PruneCondition<'a> = dyn Fn(&McState) -> bool + 'a;

pub fn explore_universe(
    vocab: &Vocabulary,
    universe: &[usize],
    max_states: u64,
    stop: Option<&StopCondition<'_>>,
    prune: Option<&PruneCondition<'_>>,
) -> Result<Graph> {
    let universe_key: BTreeSet<usize> = universe.iter().copied().collect();
    explore(vocab, &universe_key, max_states, stop, prune)
}

fn explore(
    vocab: &Vocabulary,
    universe_key: &BTreeSet<usize>,
    max_states: u64,
    stop: Option<&StopCondition<'_>>,
    prune: Option<&PruneCondition<'_>>,
) -> Result<Graph> {
    let universe: Vec<bool> = (0..vocab.facts.len())
        .map(|index| universe_key.contains(&index))
        .collect();
    let relevant = relevant_epoch_mask(vocab, universe_key);

    let initial = canonical(&relevant, McState::initial(vocab));

    let mut states = Vec::new();
    let mut parents = Vec::new();
    let mut index: HashMap<McState, usize> = HashMap::new();
    let mut stopped = None;

    let mut queue = VecDeque::new();
    index.insert(initial.clone(), 0);
    states.push(initial.clone());
    parents.push(None);
    queue.push_back(0usize);

    'bfs: while let Some(current) = queue.pop_front() {
        let state = states[current].clone();
        if let Some(stop) = stop {
            if let Some(kind) = stop(&state)? {
                stopped = Some((current, kind));
                break 'bfs;
            }
        }
        let times = state.signal_times(vocab);
        for action in enabled_actions(vocab, &universe, &state, &times)? {
            let successor = canonical(&relevant, apply_action(&state, action));
            if let Some(prune) = prune {
                if prune(&successor) {
                    continue;
                }
            }
            if index.contains_key(&successor) {
                continue;
            }
            let next = states.len() as u64;
            if next >= max_states {
                return Err(McError::Message(format!(
                    "state space exceeded max_states={max_states} while exploring a universe of {} facts; raise max_states or narrow the check",
                    universe_key.len(),
                )));
            }
            index.insert(successor.clone(), states.len());
            states.push(successor.clone());
            parents.push(Some((current, action)));
            queue.push_back(states.len() - 1);
        }
    }

    Ok(Graph {
        states,
        parents,
        stopped,
    })
}

/// 纪元可被求值读到的真相集：任一操作数原子落在宇宙内的延时位点，其
/// 全部操作数原子。其余事实的纪元没有任何表达式能读到。
fn relevant_epoch_mask(vocab: &Vocabulary, universe_key: &BTreeSet<usize>) -> Vec<bool> {
    let mut mask = vec![false; vocab.facts.len()];
    for site in &vocab.delay_sites {
        let atoms = signal_atoms(&site.operand, &site.source);
        let possibly_live = atoms.iter().any(|atom| {
            vocab
                .fact_index
                .get(atom)
                .is_some_and(|fact| universe_key.contains(fact))
        });
        if possibly_live {
            for atom in &atoms {
                if let Some(fact) = vocab.fact_index.get(atom) {
                    mask[*fact] = true;
                }
            }
        }
    }
    mask
}

fn canonical(mask: &[bool], mut state: McState) -> McState {
    for (fact, epoch) in state.fact_epochs.iter_mut().enumerate() {
        if !mask[fact] && epoch.is_some() {
            *epoch = Some(0);
        }
    }
    state
}
