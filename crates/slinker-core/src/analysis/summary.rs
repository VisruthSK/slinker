use super::execute::AbstractValue;
use super::lattice::{Bounded, Lattice};
use super::need::Need;
use super::object_world::{EnvironmentId, GraphStamps};
use crate::analysis::EdgeKind;
use crate::package::{Atom, BindingName, EnvironmentLabel, PackageId};
use crate::profile::{self, Counter};
use crate::syntax::{SourceKey, Span};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard};

pub(super) type ReadSite = (EnvironmentId, BindingName);
pub(super) type Assumption = (u64, usize);
pub(super) type WriteSite = (EnvironmentId, Option<BindingName>);

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(super) struct SummaryKey {
    pub(super) package: PackageId,
    pub(super) owner: Arc<SourceKey>,
    pub(super) arguments: Arc<[(Option<Atom>, AbstractValue)]>,
}

#[derive(Clone, Debug)]
pub(super) enum RequireReason {
    ExecutableClosure,
    NamespaceMember(BindingName),
}

impl std::fmt::Display for RequireReason {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ExecutableClosure => {
                formatter.write_str("runtime construction installs an executable closure")
            }
            Self::NamespaceMember(name) => {
                write!(formatter, "namespace member access `${name}`")
            }
        }
    }
}

#[derive(Clone, Debug)]
pub(super) enum Effect {
    Require {
        need: Need,
        kind: EdgeKind,
        reason: RequireReason,
        span: Option<Span>,
    },
    ReflectiveName {
        name: Atom,
        span: Span,
        lexical_environment: EnvironmentLabel,
    },
}

#[derive(Clone, Debug)]
pub(super) struct Summary {
    pub(super) value: AbstractValue,
    pub(super) effects: Arc<[Arc<Effect>]>,
    pub(super) reads: Arc<[ReadSite]>,
}

struct Frame {
    id: u64,
    key: SummaryKey,
    effects: Vec<Arc<Effect>>,
    before: GraphStamps,
    inherited_reads: Vec<ReadSite>,
    pending: Vec<(SummaryKey, Summary)>,
    inner: HashMap<SummaryKey, AbstractValue>,
    dirty: bool,
    assumed: Vec<Assumption>,
    cut: bool,
    approximation: AbstractValue,
    recursive: bool,
    reach: usize,
    iterations: usize,
}

pub(super) enum Advance {
    Done(AbstractValue),
    Again,
}

pub(super) struct Finished {
    pub(super) cacheable: bool,
    pub(super) assumed: Vec<Assumption>,
}

#[derive(Default)]
struct Shared {
    summaries: HashMap<SummaryKey, Summary>,
    readers: HashMap<(PackageId, EnvironmentId), HashMap<BindingName, Vec<SummaryKey>>>,
    write_cursors: HashMap<PackageId, usize>,
}

#[derive(Default)]
struct Local {
    frames: Vec<Frame>,
    suspended: Vec<Vec<Frame>>,
    namespace_grew: bool,
}

thread_local! {
    static LOCAL: RefCell<Local> = RefCell::new(Local::default());
}

fn local<R>(use_local: impl FnOnce(&mut Local) -> R) -> R {
    LOCAL.with(|local| use_local(&mut local.borrow_mut()))
}

#[derive(Default)]
pub(super) struct SummaryTable {
    shared: Mutex<Shared>,
    next_frame: AtomicU64,
}

impl SummaryTable {
    fn shared(&self) -> MutexGuard<'_, Shared> {
        self.shared.lock().expect("summary table")
    }

    pub(super) fn reset_thread(&self) {
        local(|local| *local = Local::default());
    }

    pub(super) fn assumptions_hold(&self, assumed: &[Assumption]) -> bool {
        local(|local| {
            assumed.iter().all(|(id, iterations)| {
                local
                    .frames
                    .iter()
                    .any(|frame| frame.id == *id && frame.iterations == *iterations)
            })
        })
    }

    pub(super) fn is_active(&self) -> bool {
        local(|local| !local.frames.is_empty() || !local.suspended.is_empty())
    }

    pub(super) fn suspend(&self) {
        local(|local| {
            let frames = std::mem::take(&mut local.frames);
            local.suspended.push(frames);
        });
    }

    pub(super) fn resume(&self) {
        local(|local| {
            local.frames = local.suspended.pop().unwrap_or_default();
            if std::mem::take(&mut local.namespace_grew) {
                for frame in &mut local.frames {
                    frame.cut = true;
                }
            }
        });
    }

    pub(super) fn absorb_writes(&self, package: PackageId, writes: &[WriteSite]) {
        let mut shared = self.shared();
        let cursor = shared.write_cursors.entry(package).or_default();
        let fresh = writes.get(*cursor..).unwrap_or_default();
        *cursor = writes.len();
        for (environment, name) in fresh {
            let Some(by_name) = shared.readers.get_mut(&(package, *environment)) else {
                continue;
            };
            let stale = match name {
                Some(name) => by_name.remove(name).unwrap_or_default(),
                None => by_name.drain().flat_map(|(_, keys)| keys).collect(),
            };
            for key in stale {
                shared.summaries.remove(&key);
            }
        }
    }

    pub(super) fn lookup(&self, key: &SummaryKey) -> Option<Summary> {
        self.shared().summaries.get(key).cloned()
    }

    pub(super) fn in_progress(&self, key: &SummaryKey) -> Option<usize> {
        local(|local| local.frames.iter().position(|frame| &frame.key == key))
    }

    pub(super) fn callee_active(&self, package: PackageId, owner: &SourceKey) -> bool {
        local(|local| {
            local
                .frames
                .iter()
                .any(|frame| frame.key.package == package && *frame.key.owner == *owner)
        })
    }

    pub(super) fn begin(&self, key: SummaryKey, before: GraphStamps) {
        let id = self.next_frame.fetch_add(1, Ordering::Relaxed);
        local(|local| {
            let approximation = local
                .frames
                .iter()
                .find_map(|frame| frame.inner.get(&key))
                .cloned()
                .unwrap_or_else(AbstractValue::bottom);
            local.frames.push(Frame {
                id,
                key,
                effects: Vec::new(),
                before,
                inherited_reads: Vec::new(),
                pending: Vec::new(),
                inner: HashMap::new(),
                dirty: false,
                assumed: Vec::new(),
                cut: false,
                approximation,
                recursive: false,
                reach: 0,
                iterations: 0,
            });
        });
    }

    pub(super) fn inherit_reads(&self, reads: &[ReadSite]) {
        local(|local| {
            if let Some(top) = local.frames.last_mut() {
                top.inherited_reads.extend_from_slice(reads);
            }
        });
    }

    pub(super) fn recursive_hit(&self, frame: usize) -> AbstractValue {
        local(|local| {
            let height = local.frames.len();
            let root = &mut local.frames[frame];
            root.recursive = true;
            root.reach = root.reach.max(height);
            let assumption = (root.id, root.iterations);
            let approximation = root.approximation.clone();
            if let Some(top) = local.frames.last_mut()
                && !top.assumed.contains(&assumption)
            {
                top.assumed.push(assumption);
            }
            approximation
        })
    }

    pub(super) fn advance(&self, produced: AbstractValue) -> Advance {
        local(|local| {
            let own_index = local.frames.len().saturating_sub(1);
            let leader_index = local.frames.last().and_then(|frame| {
                local.frames.iter().position(|candidate| {
                    candidate.id != frame.id
                        && frame.assumed.iter().any(|(id, _)| *id == candidate.id)
                })
            });
            let Some(frame) = local.frames.last_mut() else {
                return Advance::Done(produced);
            };
            if !frame.recursive && !frame.dirty {
                return Advance::Done(produced);
            }
            if frame.iterations == 0 {
                profile::count(Counter::SccCount);
            }
            frame.iterations += 1;
            profile::count(Counter::SccIterations);
            profile::max(
                Counter::SccLargest,
                (frame.reach.saturating_sub(own_index)) as u64,
            );
            profile::count(Counter::LatticeJoins);
            let mut next = frame.approximation.clone();
            let grew = next.join(&produced);
            let unsettled = grew || frame.dirty;
            if grew {
                profile::count(Counter::LatticeGrowths);
            }
            if let Some(leader) = leader_index {
                let key = frame.key.clone();
                frame.dirty = false;
                frame.approximation = next.clone();
                let leader = &mut local.frames[leader];
                leader.inner.insert(key, next.clone());
                leader.dirty |= unsettled;
                return Advance::Done(next);
            }
            if !unsettled {
                return Advance::Done(next);
            }
            frame.approximation = next;
            frame.recursive = false;
            frame.dirty = false;
            frame.effects.clear();
            frame.pending.clear();
            Advance::Again
        })
    }

    pub(super) fn note_cut(&self) {
        local(|local| {
            if let Some(top) = local.frames.last_mut() {
                top.cut = true;
            }
        });
    }

    pub(super) fn record(&self, effect: Arc<Effect>) {
        local(|local| {
            if let Some(top) = local.frames.last_mut() {
                top.effects.push(effect);
            }
        });
    }

    pub(super) fn depends_on_enclosing_frame(&self) -> bool {
        local(|local| {
            local
                .frames
                .last()
                .is_some_and(|frame| frame.assumed.iter().any(|(id, _)| *id != frame.id))
        })
    }

    pub(super) fn invalidate_package(&self, package: PackageId) {
        local(|local| local.namespace_grew = true);
        let mut shared = self.shared();
        shared.summaries.retain(|key, _| key.package != package);
        shared.readers.retain(|(owner, _), _| *owner != package);
    }

    pub(super) fn finish(
        &self,
        value: &AbstractValue,
        after: GraphStamps,
        logged_reads: impl FnOnce(usize) -> Vec<ReadSite>,
        arguments_are_stable: bool,
    ) -> Finished {
        let (mut frame, cacheable, assumed) = local(|local| {
            let frame = local
                .frames
                .pop()
                .expect("a summary frame is open while its evaluation runs");
            let mutated = frame.before.writes != after.writes
                || frame.before.derived_reads != after.derived_reads;
            let outer = frame
                .assumed
                .iter()
                .copied()
                .filter(|(id, _)| *id != frame.id)
                .collect::<Vec<_>>();

            if profile::enabled() {
                for (rejected, counter) in [
                    (!arguments_are_stable, Counter::SummaryRejectedArguments),
                    (frame.cut, Counter::SummaryRejectedCut),
                    (!outer.is_empty(), Counter::SummaryRejectedAssumption),
                    (mutated, Counter::SummaryRejectedWrites),
                ] {
                    if rejected {
                        profile::count(counter);
                    }
                }
            }
            let cacheable = arguments_are_stable && !frame.cut && outer.is_empty() && !mutated;
            if let Some(parent) = local.frames.last_mut() {
                parent.effects.extend(frame.effects.iter().cloned());
                parent
                    .inherited_reads
                    .extend(frame.inherited_reads.iter().cloned());
                for assumption in &outer {
                    if !parent.assumed.contains(assumption) {
                        parent.assumed.push(*assumption);
                    }
                }
                parent.cut |= frame.cut;
            }
            (frame, cacheable, outer)
        });
        let sound_under_one_root = arguments_are_stable
            && !frame.cut
            && frame.before.writes == after.writes
            && frame.before.derived_reads == after.derived_reads
            && assumed.len() == 1;
        if cacheable || sound_under_one_root {
            let mut seen = HashSet::new();
            let reads = std::mem::take(&mut frame.inherited_reads)
                .into_iter()
                .chain(logged_reads(frame.before.read_cursor))
                .filter(|read| seen.insert(read.clone()))
                .collect::<Vec<_>>();
            let summary = Summary {
                value: value.clone(),
                effects: std::mem::take(&mut frame.effects).into(),
                reads: reads.into(),
            };
            if cacheable {
                profile::count(Counter::SummariesStored);
                profile::add(Counter::SummaryEffectsStored, summary.effects.len() as u64);
                profile::add(Counter::SummaryReadsStored, summary.reads.len() as u64);
                let mut shared = self.shared();
                for (key, summary) in std::mem::take(&mut frame.pending)
                    .into_iter()
                    .chain(std::iter::once((frame.key.clone(), summary)))
                {
                    for (environment, name) in summary.reads.iter() {
                        shared
                            .readers
                            .entry((key.package, *environment))
                            .or_default()
                            .entry(name.clone())
                            .or_default()
                            .push(key.clone());
                    }
                    shared.summaries.insert(key, summary);
                }
            } else {
                let root = assumed[0].0;
                local(|local| {
                    if let Some(frame_root) = local
                        .frames
                        .iter_mut()
                        .find(|candidate| candidate.id == root)
                    {
                        frame_root.pending.push((frame.key.clone(), summary));
                    }
                });
            }
        }
        Finished { cacheable, assumed }
    }
}
