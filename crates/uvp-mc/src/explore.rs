//! 穷举探索器：按探索宇宙（事实子集）做带记忆化的 BFS，产出全状态图与
//! 父指针（反例最短路径）。超上限响亮报错，不静默截断。

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};

use crate::model::{apply_action, enabled_actions, McState};
use crate::vocab::Vocabulary;
use crate::{McError, Result};

#[derive(Debug, Clone)]
pub struct Graph {
    pub states: Vec<McState>,
    pub parents: Vec<Option<(usize, crate::model::Action)>>,
    index: HashMap<McState, usize>,
    pub edges: Vec<Vec<usize>>,
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
    cache: BTreeMap<BTreeSet<usize>, Graph>,
}

impl<'v> Explorer<'v> {
    pub fn new(vocab: &'v Vocabulary) -> Self {
        Explorer {
            vocab,
            cache: BTreeMap::new(),
        }
    }

    pub fn graph(&mut self, universe: &[usize], max_states: u64) -> Result<&Graph> {
        let key: BTreeSet<usize> = universe.iter().copied().collect();
        if !self.cache.contains_key(&key) {
            let graph = explore(self.vocab, &key, max_states)?;
            self.cache.insert(key.clone(), graph);
        }
        Ok(self
            .cache
            .get(&key)
            .expect("graph was just inserted or present"))
    }
}

fn explore(
    vocab: &Vocabulary,
    universe_key: &BTreeSet<usize>,
    max_states: u64,
) -> Result<Graph> {
    let universe: Vec<bool> = (0..vocab.facts.len())
        .map(|index| universe_key.contains(&index))
        .collect();

    let initial = McState::initial(vocab);

    let mut states = Vec::new();
    let mut parents = Vec::new();
    let mut index: HashMap<McState, usize> = HashMap::new();
    let mut edges: Vec<Vec<usize>> = Vec::new();

    let mut queue = VecDeque::new();
    index.insert(initial.clone(), 0);
    states.push(initial.clone());
    parents.push(None);
    edges.push(Vec::new());
    queue.push_back(0usize);

    while let Some(current) = queue.pop_front() {
        let state = states[current].clone();
        let times = state.signal_times(vocab);
        for action in enabled_actions(vocab, &universe, &state, &times)? {
            let successor = apply_action(&state, action);
            let next = match index.get(&successor) {
                Some(existing) => *existing,
                None => {
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
    })
}
