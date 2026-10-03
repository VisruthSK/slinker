use std::collections::HashMap;
use std::hash::Hash;
use std::sync::{Condvar, Mutex, MutexGuard};
use std::thread::ThreadId;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Phase {
    Queued,
    Running(ThreadId),
    Done,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Claim {
    Mine,
    AlreadyDone,
    Wait,
}

struct Table<K> {
    phases: HashMap<K, Phase>,
    injected: Vec<K>,
    staged: HashMap<ThreadId, Vec<K>>,
    waiting: HashMap<ThreadId, K>,
    pending: usize,
}

pub(super) struct Machine<K> {
    table: Mutex<Table<K>>,
    changed: Condvar,
}

impl<K: Eq + Hash + Clone> Default for Machine<K> {
    fn default() -> Self {
        Self {
            table: Mutex::new(Table {
                phases: HashMap::new(),
                injected: Vec::new(),
                staged: HashMap::new(),
                waiting: HashMap::new(),
                pending: 0,
            }),
            changed: Condvar::new(),
        }
    }
}

impl<K: Eq + Hash + Clone> Machine<K> {
    #[track_caller]
    fn table(&self) -> MutexGuard<'_, Table<K>> {
        super::guarded::contended(&self.table)
    }

    pub(super) fn request(&self, key: K) {
        let mut table = self.table();
        if table.phases.contains_key(&key) {
            return;
        }
        table.phases.insert(key.clone(), Phase::Queued);
        table.pending += 1;
        match table.staged.get_mut(&std::thread::current().id()) {
            Some(staged) => staged.push(key),
            None => table.injected.push(key),
        }
    }

    pub(super) fn publish_staged(&self) {
        let mut table = self.table();
        if let Some(staged) = table.staged.remove(&std::thread::current().id()) {
            table.injected.extend(staged);
        }
    }

    pub(super) fn take_injected(&self) -> Vec<K> {
        std::mem::take(&mut self.table().injected)
    }

    pub(super) fn begin(&self, key: &K) -> bool {
        let mut table = self.table();
        match table.phases.get(key).copied() {
            Some(Phase::Queued) => {
                let me = std::thread::current().id();
                table.phases.insert(key.clone(), Phase::Running(me));
                table.staged.entry(me).or_default();
                true
            }
            _ => false,
        }
    }

    pub(super) fn claim(&self, key: &K) -> Claim {
        let mut table = self.table();
        match table.phases.get(key).copied() {
            None => {
                table
                    .phases
                    .insert(key.clone(), Phase::Running(std::thread::current().id()));
                table.pending += 1;
                Claim::Mine
            }
            Some(Phase::Queued) => {
                table
                    .phases
                    .insert(key.clone(), Phase::Running(std::thread::current().id()));
                Claim::Mine
            }
            Some(Phase::Done) => Claim::AlreadyDone,
            Some(Phase::Running(owner)) => {
                if owner == std::thread::current().id() {
                    Claim::AlreadyDone
                } else {
                    Claim::Wait
                }
            }
        }
    }

    pub(super) fn release_claim(&self, key: &K) {
        let mut table = self.table();
        table.phases.insert(key.clone(), Phase::Done);
        table.pending -= 1;
        self.changed.notify_all();
    }

    pub(super) fn wait_done(&self, key: &K) {
        let me = std::thread::current().id();
        let mut table = self.table();
        loop {
            match table.phases.get(key).copied() {
                Some(Phase::Done) | None => {
                    table.waiting.remove(&me);
                    return;
                }
                Some(Phase::Running(owner)) => {
                    if Self::would_cycle(&table, me, owner) {
                        table.waiting.remove(&me);
                        return;
                    }
                }
                Some(Phase::Queued) => {
                    table.waiting.remove(&me);
                    return;
                }
            }
            table.waiting.insert(me, key.clone());
            table = self.changed.wait(table).expect("scheduler wait");
        }
    }

    fn would_cycle(table: &Table<K>, me: ThreadId, mut owner: ThreadId) -> bool {
        for _ in 0..=table.waiting.len() {
            if owner == me {
                return true;
            }
            let Some(key) = table.waiting.get(&owner) else {
                return false;
            };
            owner = match table.phases.get(key) {
                Some(Phase::Running(next)) => *next,
                _ => return false,
            };
        }
        true
    }

    pub(super) fn pending(&self) -> usize {
        self.table().pending
    }

    pub(super) fn is_quiescent(&self) -> bool {
        let table = self.table();
        table.pending == 0 && table.injected.is_empty()
    }

    pub(super) fn keys(&self) -> Vec<K> {
        self.table().phases.keys().cloned().collect()
    }
}

#[cfg(test)]
#[path = "../../tests/unit/analysis/scheduler.rs"]
mod tests;
