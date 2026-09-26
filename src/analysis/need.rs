use crate::analysis::object_world::ClosureId;
use crate::package::PackageId;
use std::collections::{HashSet, VecDeque};

pub use crate::package::BindingName;
pub type ResourceId = String;
pub type NativeId = String;
pub type LifecycleId = String;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct S3Id {
    pub generic: GenericId,
    pub class: String,
    pub method: String,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct GenericId {
    pub package: Option<PackageId>,
    pub name: String,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum Need {
    Binding {
        package: PackageId,
        binding: BindingName,
    },
    PrivateBinding {
        package: PackageId,
        environment: String,
        binding: BindingName,
    },
    ClosureExecution {
        package: PackageId,
        closure: ClosureId,
    },
    Activation {
        package: PackageId,
    },
    Resource {
        package: PackageId,
        resource: ResourceId,
    },
    Dataset {
        package: PackageId,
        dataset: String,
    },
    S3Registration {
        package: PackageId,
        registration: S3Id,
    },
    Native {
        package: PackageId,
        component: NativeId,
    },
    Lifecycle {
        package: PackageId,
        hook: LifecycleId,
    },
}

impl Need {
    pub fn package(&self) -> PackageId {
        match self {
            Self::Binding { package, .. }
            | Self::PrivateBinding { package, .. }
            | Self::ClosureExecution { package, .. }
            | Self::Activation { package }
            | Self::Resource { package, .. }
            | Self::Dataset { package, .. }
            | Self::S3Registration { package, .. }
            | Self::Native { package, .. }
            | Self::Lifecycle { package, .. } => *package,
        }
    }
}

pub(super) enum Popped {
    Started(Need),
    AlreadyStarted,
}

#[derive(Default)]
pub(super) struct NeedQueue {
    pending: VecDeque<Need>,
    queued: HashSet<Need>,
    started: HashSet<Need>,
}

impl NeedQueue {
    pub(super) fn schedule(&mut self, need: Need) {
        if !self.started.contains(&need) && self.queued.insert(need.clone()) {
            self.pending.push_back(need);
        }
    }

    pub(super) fn len(&self) -> usize {
        self.pending.len()
    }

    pub(super) fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }

    pub(super) fn upcoming(&self, count: usize) -> impl Iterator<Item = &Need> {
        self.pending.iter().take(count)
    }

    pub(super) fn pop(&mut self) -> Option<Popped> {
        let need = self.pending.pop_front()?;
        self.queued.remove(&need);
        Some(if self.started.insert(need.clone()) {
            Popped::Started(need)
        } else {
            Popped::AlreadyStarted
        })
    }

    pub(super) fn start(&mut self, need: &Need) -> bool {
        self.queued.remove(need);
        self.started.insert(need.clone())
    }

    pub(super) fn started(&self) -> impl Iterator<Item = &Need> {
        self.started.iter()
    }
}
