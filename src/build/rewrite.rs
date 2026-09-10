use crate::package::{BindingName, PackageId};
use crate::syntax::Span;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PackageOperation {
    RequireNamespace { result: bool },
    LoadNamespace,
    GetNamespace,
    AsNamespace,
    PackageVersion { version: String },
    FindPackage,
}

#[derive(Clone, Debug)]
pub enum Rewrite {
    NamespaceAccess {
        source: Span,
        package: PackageId,
        binding: BindingName,
        internal: bool,
    },
    ResourceAccess {
        source: Span,
        package: PackageId,
        resource: String,
    },
    PackageOperation {
        source: Span,
        package: Option<PackageId>,
        operation: PackageOperation,
    },
}
