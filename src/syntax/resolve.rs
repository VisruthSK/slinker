use crate::package::{BindingName, PackageId};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ResolvedName {
    Local(String),
    PackageBinding {
        package: PackageId,
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
    Base(String),
    Unknown(String),
}
