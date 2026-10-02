use crate::analysis::object_world::ClosureId;
use crate::package::{
    BindingName, ClassName, ComponentName, DatasetName, EnvironmentLabel, GenericName, PackageId,
    ResourcePath,
};
use std::collections::{HashSet, VecDeque};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum LifecycleHook {
    OnLoad,
}

impl LifecycleHook {
    pub fn binding(self) -> BindingName {
        match self {
            Self::OnLoad => BindingName::from(".onLoad"),
        }
    }
}

impl std::fmt::Display for LifecycleHook {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::OnLoad => f.write_str(".onLoad"),
        }
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct S3Id {
    pub generic: GenericId,
    pub class: ClassName,
    pub method: BindingName,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct GenericId {
    pub package: Option<PackageId>,
    pub name: GenericName,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum Need {
    Binding {
        package: PackageId,
        binding: BindingName,
    },
    PrivateBinding {
        package: PackageId,
        environment: EnvironmentLabel,
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
        resource: ResourcePath,
    },
    Dataset {
        package: PackageId,
        dataset: DatasetName,
    },
    S3Registration {
        package: PackageId,
        registration: S3Id,
    },
    Native {
        package: PackageId,
        component: ComponentName,
    },
    Lifecycle {
        package: PackageId,
        hook: LifecycleHook,
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

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Schedule {
    #[default]
    Fifo,
    Lifo,
    Seeded(u64),
}

#[derive(Default)]
pub(super) struct NeedQueue {
    pending: VecDeque<Need>,
    queued: HashSet<Need>,
    started: HashSet<Need>,
    schedule: Schedule,
    state: u64,
}

impl NeedQueue {
    pub(super) fn with_schedule(schedule: Schedule) -> Self {
        Self {
            schedule,
            state: match schedule {
                Schedule::Seeded(seed) => seed.wrapping_mul(2).wrapping_add(1),
                Schedule::Fifo | Schedule::Lifo => 0,
            },
            ..Self::default()
        }
    }

    fn take(&mut self) -> Option<Need> {
        match self.schedule {
            Schedule::Fifo => self.pending.pop_front(),
            Schedule::Lifo => self.pending.pop_back(),
            Schedule::Seeded(_) => {
                if self.pending.is_empty() {
                    return None;
                }
                self.state ^= self.state << 13;
                self.state ^= self.state >> 7;
                self.state ^= self.state << 17;
                let index = usize::try_from(self.state % self.pending.len() as u64)
                    .expect("index fits usize");
                self.pending.swap_remove_back(index)
            }
        }
    }

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
        let need = self.take()?;
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
