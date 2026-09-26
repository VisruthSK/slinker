use crate::ir::NamespaceOperation;
use crate::package::{BindingName, PackageId};
use crate::syntax::Span;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum NamespaceCall {
    Require,
    Operation(NamespaceOperation),
}

#[derive(Clone, Debug)]
pub(super) enum PendingRelocation {
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
    RequireNamespace {
        source: Span,
        result: bool,
    },
    NamespaceLoad {
        source: Span,
        package: PackageId,
        operation: NamespaceOperation,
    },
    PackageVersion {
        source: Span,
        version: String,
    },
}

impl PendingRelocation {
    pub(super) fn source(&self) -> &Span {
        match self {
            Self::NamespaceAccess { source, .. }
            | Self::ResourceAccess { source, .. }
            | Self::RequireNamespace { source, .. }
            | Self::NamespaceLoad { source, .. }
            | Self::PackageVersion { source, .. } => source,
        }
    }

    pub(super) fn reaches_removed_installation(&self) -> bool {
        matches!(
            self,
            Self::ResourceAccess { .. } | Self::PackageVersion { .. }
        )
    }
}
