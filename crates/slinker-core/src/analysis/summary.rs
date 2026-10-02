use super::execute::AbstractValue;
use super::lattice::{Bounded, Lattice};
use super::need::Need;
use super::object_world::{EnvironmentId, GraphStamps};
use crate::analysis::EdgeKind;
use crate::package::{BindingName, EnvironmentLabel, PackageId};
use crate::profile::{self, Counter};
use crate::syntax::{SourceKey, Span};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

pub(super) type ReadSite = (EnvironmentId, BindingName);
pub(super) type WriteSite = (EnvironmentId, Option<BindingName>);

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(super) struct SummaryKey {
    pub(super) package: PackageId,
    pub(super) owner: SourceKey,
    pub(super) arguments: Vec<(Option<String>, AbstractValue)>,
}

#[derive(Clone, Debug)]
pub(super) enum Effect {
    Require {
        need: Need,
        kind: EdgeKind,
        reason: String,
    },
    ReflectiveName {
        name: String,
        span: Span,
        lexical_environment: EnvironmentLabel,
    },
}

#[derive(Clone, Debug)]
pub(super) struct Summary {
    pub(super) value: AbstractValue,
    pub(super) effects: Arc<[Effect]>,
    pub(super) reads: Arc<[ReadSite]>,
}

struct Frame {
    key: SummaryKey,
    effects: Vec<Effect>,
    before: GraphStamps,
    inherited_reads: Vec<ReadSite>,
    shallowest_assumption: usize,
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
    pub(super) assumed: bool,
}

#[derive(Default)]
pub(super) struct SummaryTable {
    summaries: HashMap<SummaryKey, Summary>,
    readers: HashMap<(PackageId, EnvironmentId), HashMap<BindingName, Vec<SummaryKey>>>,
    write_cursors: HashMap<PackageId, usize>,
    frames: Vec<Frame>,
    epoch: u64,
}

impl SummaryTable {
    pub(super) fn epoch(&self) -> u64 {
        self.epoch
    }

    pub(super) fn is_active(&self) -> bool {
        !self.frames.is_empty()
    }

    pub(super) fn absorb_writes(&mut self, package: PackageId, writes: &[WriteSite]) {
        let cursor = self.write_cursors.entry(package).or_default();
        let fresh = writes.get(*cursor..).unwrap_or_default();
        *cursor = writes.len();
        for (environment, name) in fresh {
            let Some(by_name) = self.readers.get_mut(&(package, *environment)) else {
                continue;
            };
            let stale = match name {
                Some(name) => by_name.remove(name).unwrap_or_default(),
                None => by_name.drain().flat_map(|(_, keys)| keys).collect(),
            };
            for key in stale {
                self.summaries.remove(&key);
            }
        }
    }

    pub(super) fn lookup(&self, key: &SummaryKey) -> Option<&Summary> {
        self.summaries.get(key)
    }

    pub(super) fn in_progress(&self, key: &SummaryKey) -> Option<usize> {
        self.frames.iter().position(|frame| &frame.key == key)
    }

    pub(super) fn callee_active(&self, package: PackageId, owner: &SourceKey) -> bool {
        self.frames
            .iter()
            .any(|frame| frame.key.package == package && &frame.key.owner == owner)
    }

    pub(super) fn begin(&mut self, key: SummaryKey, before: GraphStamps) {
        self.frames.push(Frame {
            key,
            effects: Vec::new(),
            before,
            inherited_reads: Vec::new(),
            shallowest_assumption: usize::MAX,
            cut: false,
            approximation: AbstractValue::bottom(),
            recursive: false,
            reach: 0,
            iterations: 0,
        });
    }

    pub(super) fn inherit_reads(&mut self, reads: &[ReadSite]) {
        if let Some(top) = self.frames.last_mut() {
            top.inherited_reads.extend_from_slice(reads);
        }
    }

    pub(super) fn recursive_hit(&mut self, frame: usize) -> AbstractValue {
        let height = self.frames.len();
        if let Some(top) = self.frames.last_mut() {
            top.shallowest_assumption = top.shallowest_assumption.min(frame);
        }
        let root = &mut self.frames[frame];
        root.recursive = true;
        root.reach = root.reach.max(height);
        root.approximation.clone()
    }

    pub(super) fn advance(&mut self, produced: AbstractValue) -> Advance {
        let own_index = self.frames.len().saturating_sub(1);
        let Some(frame) = self.frames.last_mut() else {
            return Advance::Done(produced);
        };
        if !frame.recursive {
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
        if !next.join(&produced) {
            return Advance::Done(next);
        }
        profile::count(Counter::LatticeGrowths);
        frame.approximation = next;
        frame.recursive = false;
        frame.effects.clear();
        self.epoch += 1;
        Advance::Again
    }

    pub(super) fn note_cut(&mut self) {
        if let Some(top) = self.frames.last_mut() {
            top.cut = true;
        }
    }

    pub(super) fn record(&mut self, effect: Effect) {
        if let Some(top) = self.frames.last_mut() {
            top.effects.push(effect);
        }
    }

    pub(super) fn finish(
        &mut self,
        value: &AbstractValue,
        after: GraphStamps,
        logged_reads: impl FnOnce(usize) -> Vec<ReadSite>,
        arguments_are_stable: bool,
    ) -> Finished {
        let frame = self
            .frames
            .pop()
            .expect("a summary frame is open while its evaluation runs");
        let own_index = self.frames.len();
        let mutated = frame.before.writes != after.writes
            || frame.before.derived_reads != after.derived_reads;
        let assumed = frame.shallowest_assumption < own_index;
        if profile::enabled() {
            for (rejected, counter) in [
                (!arguments_are_stable, Counter::SummaryRejectedArguments),
                (frame.cut, Counter::SummaryRejectedCut),
                (assumed, Counter::SummaryRejectedAssumption),
                (mutated, Counter::SummaryRejectedWrites),
            ] {
                if rejected {
                    profile::count(counter);
                }
            }
        }
        let cacheable = arguments_are_stable && !frame.cut && !assumed && !mutated;
        if let Some(parent) = self.frames.last_mut() {
            parent.effects.extend(frame.effects.iter().cloned());
            parent
                .inherited_reads
                .extend(frame.inherited_reads.iter().cloned());
            parent.shallowest_assumption = parent
                .shallowest_assumption
                .min(frame.shallowest_assumption);
            parent.cut |= frame.cut;
        }
        if cacheable {
            let mut seen = HashSet::new();
            let reads = frame
                .inherited_reads
                .into_iter()
                .chain(logged_reads(frame.before.read_cursor))
                .filter(|read| seen.insert(read.clone()))
                .collect::<Vec<_>>();
            for (environment, name) in &reads {
                self.readers
                    .entry((frame.key.package, *environment))
                    .or_default()
                    .entry(name.clone())
                    .or_default()
                    .push(frame.key.clone());
            }
            self.summaries.insert(
                frame.key,
                Summary {
                    value: value.clone(),
                    effects: frame.effects.into(),
                    reads: reads.into(),
                },
            );
        }
        Finished { cacheable, assumed }
    }
}

impl SummaryTable {
    pub(super) fn depends_on_enclosing_frame(&self) -> bool {
        let own_index = self.frames.len().saturating_sub(1);
        self.frames
            .last()
            .is_some_and(|frame| frame.shallowest_assumption < own_index)
    }
}

impl SummaryTable {
    pub(super) fn invalidate_package(&mut self, package: PackageId) {
        self.summaries.retain(|key, _| key.package != package);
        self.readers.retain(|(owner, _), _| *owner != package);
    }
}
