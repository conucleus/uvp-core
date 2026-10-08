//! 穷举探索器：按（scope, mode）做带记忆化的 BFS，产出全状态图与父指针
//! （反例最短路径）。超上限响亮报错，不静默截断。

use std::collections::{BTreeMap, BTreeSet, HashMap};

use crate::model::{apply_action, enabled_actions, McState, Mode};
use crate::vocab::Vocabulary;
use crate::{McError, Result};

#[derive(Debug, Clone)]
pub struct Graph {
    pub states: Vec<McState>,
    pub parents: Vec<Option<(usize, crate::model::Action)>>,
    index: HashMap<McState, usize>,
    pub edges: Vec<Vec<usize>>,
    pub scope_key: BTreeSet<usize>,
    pub mode: Mode,
}

impl Graph {
    pub fn state_index(&self, state: &McState) -> Option<usize> {
        self.index.get(state).copied()
    }

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

pub struct Explorer<'v> {
    vocab: &'v Vocabulary,
    cache: BTreeMap<(BTreeSet<usize>, Mode), Graph>,
}

impl<'v> Explorer<'v> {
    pub fn new(vocab: &'v Vocabulary) -> Self {
        Explorer {
            vocab,
            cache: BTreeMap::new(),
        }
    }

    pub fn graph(&mut self, scope_facts: &[usize], mode: Mode, max_states: u64) -> Result<&Graph> {
        use std::collections::btree_map::Entry;
        let scope_key: BTreeSet<usize> = scope_facts.iter().copied().collect();
        match self.cache.entry((scope_key.clone(), mode)) {
            Entry::Occupied(entry) => Ok(entry.into_mut()),
            Entry::Vacant(entry) => {
                let graph = explore(self.vocab, &scope_key, mode, max_states)?;
                Ok(entry.insert(graph))
            }
        }
    }
}

fn explore(
    vocab: &Vocabulary,
    scope_key: &BTreeSet<usize>,
    mode: Mode,
    max_states: u64,
) -> Result<Graph> {
    let scope: Vec<bool> = (0..vocab.facts.len())
        .map(|index| scope_key.contains(&index))
        .collect();

    let mut initial = McState::initial(vocab);
    let mut initial_times = initial.signal_times(vocab);
    crate::model::settle(vocab, &scope, &mut initial, &mut initial_times);

    let mut states = Vec::new();
    let mut parents = Vec::new();
    let mut index: HashMap<McState, usize> = HashMap::new();
    let mut edges: Vec<Vec<usize>> = Vec::new();

    let mut queue = std::collections::VecDeque::new();
    index.insert(initial.clone(), 0);
    states.push(initial.clone());
    parents.push(None);
    edges.push(Vec::new());
    queue.push_back(0usize);

    while let Some(current) = queue.pop_front() {
        let state = states[current].clone();
        let times = state.signal_times(vocab);
        for action in enabled_actions(vocab, &scope, &state, &times, mode) {
            let successor = apply_action(vocab, &scope, &state, &times, action);
            let next = match index.get(&successor) {
                Some(existing) => *existing,
                None => {
                    let next = states.len() as u64;
                    if next >= max_states {
                        return Err(McError::Message(format!(
                            "state space exceeded max_states={max_states} while exploring scope of {} facts (mode {:?}); raise max_states or narrow the check scope",
                            scope_key.len(),
                            mode
                        )));
                    }
                    index.insert(successor.clone(), states.len());
                    states.push(successor.clone());
                    parents.push(Some((current, action)));
                    edges.push(Vec::new());
                    queue.push_back(states.len() - 1);
                    states.len() - 1
                }
            };
            edges[current].push(next);
        }
    }

    Ok(Graph {
        states,
        parents,
        index,
        edges,
        scope_key: scope_key.clone(),
        mode,
    })
}
