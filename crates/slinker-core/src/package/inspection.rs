use crate::worker::client::WorkerClient;
use crate::{Error, Result, TargetEnvironment, WorkerExecutable};
use std::collections::HashMap;
use std::hash::Hash;
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::Duration;

const RETRY: Duration = Duration::from_millis(1);
const BATCH: usize = 64;

pub(super) struct Slot<V> {
    value: Mutex<Option<std::result::Result<V, String>>>,
    ready: Condvar,
}

impl<V: Clone> Slot<V> {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            value: Mutex::new(None),
            ready: Condvar::new(),
        })
    }

    fn fill(&self, outcome: std::result::Result<V, String>) {
        let mut value = self.value.lock().expect("slot lock");
        if value.is_none() {
            *value = Some(outcome);
        }
        self.ready.notify_all();
    }

    fn peek(&self) -> Option<Result<V>> {
        self.value
            .lock()
            .expect("slot lock")
            .clone()
            .map(|outcome| outcome.map_err(Error::Analysis))
    }

    fn wait_briefly(&self) {
        let value = self.value.lock().expect("slot lock");
        if value.is_none() {
            let _ = self.ready.wait_timeout(value, RETRY).expect("slot wait");
        }
    }
}

pub(super) struct Lane {
    client: Mutex<Option<WorkerClient>>,
    id: u64,
}

pub(super) struct Lanes {
    lanes: Vec<Lane>,
    r_home: PathBuf,
    target: TargetEnvironment,
    pub(super) executable: WorkerExecutable,
}

pub(super) struct LaneGuard<'a> {
    guard: MutexGuard<'a, Option<WorkerClient>>,
    lanes: &'a Lanes,
    id: u64,
}

impl LaneGuard<'_> {
    pub(super) fn client(&mut self) -> Result<&mut WorkerClient> {
        if self.guard.is_none() {
            *self.guard = Some(WorkerClient::spawn(
                self.lanes.r_home.clone(),
                &self.lanes.target,
                self.id,
                &self.lanes.executable,
            )?);
        }
        Ok(self.guard.as_mut().expect("lane client was just spawned"))
    }
}

impl Lanes {
    pub(super) fn new(r_home: PathBuf, target: TargetEnvironment, count: usize) -> Self {
        Self {
            lanes: (0..count.max(1))
                .map(|index| Lane {
                    client: Mutex::new(None),
                    id: index as u64 + 1,
                })
                .collect(),
            r_home,
            target,
            executable: WorkerExecutable::default(),
        }
    }

    pub(super) fn prime(&self, client: WorkerClient) {
        *self.lanes[0].client.lock().expect("lane lock") = Some(client);
    }

    pub(super) fn count(&self) -> usize {
        self.lanes.len()
    }

    pub(super) fn try_lane(&self, index: usize) -> Option<LaneGuard<'_>> {
        let lane = &self.lanes[index % self.lanes.len()];
        lane.client.try_lock().ok().map(|guard| LaneGuard {
            guard,
            lanes: self,
            id: lane.id,
        })
    }

    pub(super) fn try_any(&self, start: usize) -> Option<LaneGuard<'_>> {
        (0..self.lanes.len()).find_map(|offset| self.try_lane(start + offset))
    }

    pub(super) fn lane(&self, index: usize) -> LaneGuard<'_> {
        let lane = &self.lanes[index % self.lanes.len()];
        LaneGuard {
            guard: lane.client.lock().expect("lane lock"),
            lanes: self,
            id: lane.id,
        }
    }
}

pub(super) type Execute<'a, Q, V> = &'a dyn Fn(&mut WorkerClient, &[Q]) -> Result<Vec<V>>;

pub(super) struct Batcher<Q, V> {
    slots: Mutex<HashMap<Q, Arc<Slot<V>>>>,
    pending: Mutex<Vec<(Q, Arc<Slot<V>>)>>,
}

impl<Q: Eq + Hash + Clone, V: Clone> Default for Batcher<Q, V> {
    fn default() -> Self {
        Self {
            slots: Mutex::new(HashMap::new()),
            pending: Mutex::new(Vec::new()),
        }
    }
}

impl<Q: Eq + Hash + Clone, V: Clone> Batcher<Q, V> {
    pub(super) fn known(&self, query: &Q) -> Option<Result<V>> {
        self.slots
            .lock()
            .expect("batcher slots")
            .get(query)
            .and_then(|slot| slot.peek())
    }

    pub(super) fn seed(&self, query: &Q, value: V) {
        let slot = self.slot_for(query).0;
        slot.fill(Ok(value));
    }

    fn slot_for(&self, query: &Q) -> (Arc<Slot<V>>, bool) {
        let mut slots = self.slots.lock().expect("batcher slots");
        match slots.get(query) {
            Some(slot) => (Arc::clone(slot), false),
            None => {
                let slot = Slot::new();
                slots.insert(query.clone(), Arc::clone(&slot));
                (slot, true)
            }
        }
    }

    pub(super) fn submit(&self, query: &Q) -> Arc<Slot<V>> {
        let (slot, fresh) = self.slot_for(query);
        if fresh {
            self.pending
                .lock()
                .expect("batcher pending")
                .push((query.clone(), Arc::clone(&slot)));
        }
        slot
    }

    pub(super) fn drive(
        &self,
        lanes: &Lanes,
        slot: &Slot<V>,
        execute: Execute<'_, Q, V>,
    ) -> Result<V> {
        let start = slot as *const Slot<V> as usize;
        loop {
            if let Some(outcome) = slot.peek() {
                return outcome;
            }
            let guard = lanes.try_any(start);
            match guard {
                Some(guard) => self.serve(guard, execute),
                None => slot.wait_briefly(),
            }
        }
    }

    pub(super) fn lead(&self, lanes: &Lanes, execute: Execute<'_, Q, V>) {
        let guard = lanes.try_any(0);
        if let Some(guard) = guard {
            self.serve(guard, execute);
        }
    }

    fn serve(&self, mut guard: LaneGuard<'_>, execute: Execute<'_, Q, V>) {
        loop {
            let batch = {
                let mut pending = self.pending.lock().expect("batcher pending");
                let take = pending.len().min(BATCH);
                pending.drain(..take).collect::<Vec<_>>()
            };
            if batch.is_empty() {
                return;
            }
            let queries = batch
                .iter()
                .map(|(query, _)| query.clone())
                .collect::<Vec<_>>();
            let outcome = guard
                .client()
                .and_then(|client| execute(client, &queries))
                .and_then(|values| {
                    if values.len() == queries.len() {
                        Ok(values)
                    } else {
                        Err(Error::Analysis(
                            "worker answered a different batch size".into(),
                        ))
                    }
                });
            match outcome {
                Ok(values) => {
                    for ((_, slot), value) in batch.into_iter().zip(values) {
                        slot.fill(Ok(value));
                    }
                }
                Err(error) => {
                    let message = error.to_string();
                    for (_, slot) in batch {
                        slot.fill(Err(message.clone()));
                    }
                }
            }
        }
    }
}

impl Lanes {
    pub(super) fn r_home(&self) -> &std::path::Path {
        &self.r_home
    }
}
