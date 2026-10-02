use crate::analysis::object_world::ClosureId;
use crate::package::{
    BindingName, ClassName, ComponentName, DatasetName, EnvironmentLabel, GenericName, PackageId,
    ResourcePath,
};

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

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Schedule {
    #[default]
    Fifo,
    Lifo,
    Seeded(u64),
}

impl Schedule {
    pub(super) fn arrange(self, needs: &mut [Need]) {
        match self {
            Self::Fifo => {}
            Self::Lifo => needs.reverse(),
            Self::Seeded(seed) => {
                let mut state = seed.wrapping_mul(2).wrapping_add(1);
                for position in (1..needs.len()).rev() {
                    state ^= state << 13;
                    state ^= state >> 7;
                    state ^= state << 17;
                    let other =
                        usize::try_from(state % (position as u64 + 1)).expect("index fits usize");
                    needs.swap(position, other);
                }
            }
        }
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(super) enum WorkKey {
    Need(Need),
    Seal(PackageId),
}
