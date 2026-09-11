use crate::package::{BindingName, ClosureId, PackageId};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ResolvedName {
    Local(String),
    PackageBinding {
        package: PackageId,
        binding: BindingName,
    },
    PrivateBinding {
        package: PackageId,
        environment: String,
        binding: BindingName,
    },
    ClosureObject {
        package: PackageId,
        closure: ClosureId,
    },
    NativeSymbol {
        package: PackageId,
        component: String,
        binding: BindingName,
    },
    Imported {
        package: PackageId,
        binding: BindingName,
    },
    TargetProvided {
        package: PackageId,
        binding: BindingName,
    },
    PackageMetadata {
        package: PackageId,
        name: String,
    },
    MissingPackage {
        package: String,
        binding: Option<BindingName>,
    },
    Base(String),
    Unknown(String),
}
