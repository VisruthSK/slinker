use crate::package::{BindingName, PackageId};
use crate::syntax::Span;

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
    SpecializedDiscovery {
        source: Span,
        package: PackageId,
        result: bool,
    },
}
