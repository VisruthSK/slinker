use crate::package::PackageId;

pub type BindingName = String;
pub type ResourceId = String;
pub type NativeId = String;
pub type LifecycleId = String;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct S3Id {
    pub generic: String,
    pub class: String,
    pub method: String,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum Need {
    Binding {
        package: PackageId,
        binding: BindingName,
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
    pub fn package(&self) -> &PackageId {
        match self {
            Self::Binding { package, .. }
            | Self::Activation { package }
            | Self::Resource { package, .. }
            | Self::Dataset { package, .. }
            | Self::S3Registration { package, .. }
            | Self::Native { package, .. }
            | Self::Lifecycle { package, .. } => package,
        }
    }
}
