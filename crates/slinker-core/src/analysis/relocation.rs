use super::NodeId;
use crate::ir::NamespaceOperation;
use crate::package::{
    BindingName, ComponentName, DataSetName, DatasetName, PackageId, ResourcePath, SymbolName,
};
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
        resource: ResourcePath,
    },
    RequireNamespace {
        source: Span,
        loaded: Option<PackageId>,
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
    LoadedQuery {
        source: Span,
        package: PackageId,
    },
    NamespaceArgument {
        source: Span,
        package: PackageId,
    },
    DescriptionArgument {
        source: Span,
        package: PackageId,
    },
    DatasetAccess {
        source: Span,
        package: PackageId,
        dataset: DatasetName,
    },
    DataArgument {
        source: Span,
        package: PackageId,
        sets: Vec<DataSetName>,
    },
    NativeSymbol {
        source: Span,
        package: PackageId,
        component: ComponentName,
        symbol: SymbolName,
    },
    NativeLibrary {
        source: Span,
        package: PackageId,
        component: ComponentName,
    },
    InstalledQuery {
        source: Span,
        package: PackageId,
        check: bool,
    },
}

impl PendingRelocation {
    pub(super) fn namespace(source: Span, package: PackageId, call: NamespaceCall) -> Self {
        match call {
            NamespaceCall::Require => Self::RequireNamespace {
                source,
                loaded: Some(package),
            },
            NamespaceCall::Operation(operation) => Self::NamespaceLoad {
                source,
                package,
                operation,
            },
        }
    }

    pub(super) fn source(&self) -> &Span {
        match self {
            Self::NamespaceAccess { source, .. }
            | Self::ResourceAccess { source, .. }
            | Self::RequireNamespace { source, .. }
            | Self::NamespaceLoad { source, .. }
            | Self::PackageVersion { source, .. }
            | Self::LoadedQuery { source, .. }
            | Self::NamespaceArgument { source, .. }
            | Self::DatasetAccess { source, .. }
            | Self::DataArgument { source, .. }
            | Self::DescriptionArgument { source, .. }
            | Self::NativeSymbol { source, .. }
            | Self::NativeLibrary { source, .. }
            | Self::InstalledQuery { source, .. } => source,
        }
    }

    pub(super) fn reaches_removed_installation(&self) -> bool {
        matches!(
            self,
            Self::ResourceAccess { .. }
                | Self::PackageVersion { .. }
                | Self::DescriptionArgument { .. }
        )
    }

    pub(super) fn named_namespace(&self) -> Option<PackageId> {
        match self {
            Self::NamespaceAccess { package, .. }
            | Self::NamespaceLoad { package, .. }
            | Self::LoadedQuery { package, .. }
            | Self::NamespaceArgument { package, .. }
            | Self::DatasetAccess { package, .. }
            | Self::DataArgument { package, .. }
            | Self::NativeSymbol { package, .. }
            | Self::NativeLibrary { package, .. }
            | Self::InstalledQuery { package, .. } => Some(*package),
            Self::RequireNamespace { loaded, .. } => *loaded,
            Self::ResourceAccess { .. }
            | Self::PackageVersion { .. }
            | Self::DescriptionArgument { .. } => None,
        }
    }
}

#[derive(Clone, Debug)]
pub(super) struct SyntaxObservation {
    pub(super) node: NodeId,
    pub(super) package: PackageId,
    pub(super) span: Span,
    pub(super) callee: BindingName,
}

#[derive(Default)]
pub(super) struct RelocationPlan {
    relocations: Vec<PendingRelocation>,
    dynamic_resource_lookups: Vec<(NodeId, PackageId, Span)>,
    observations: Vec<SyntaxObservation>,
}

impl RelocationPlan {
    pub(super) fn push(&mut self, relocation: PendingRelocation) {
        self.relocations.push(relocation);
    }

    pub(super) fn relocations(&self) -> &[PendingRelocation] {
        &self.relocations
    }

    pub(super) fn defer_dynamic_resource_lookup(
        &mut self,
        node: NodeId,
        package: PackageId,
        span: Span,
    ) {
        self.dynamic_resource_lookups.push((node, package, span));
    }

    pub(super) fn take_dynamic_resource_lookups(&mut self) -> Vec<(NodeId, PackageId, Span)> {
        std::mem::take(&mut self.dynamic_resource_lookups)
    }

    pub(super) fn observe(&mut self, observation: SyntaxObservation) {
        self.observations.push(observation);
    }

    pub(super) fn observations_of_rewritten_syntax(&self) -> Vec<SyntaxObservation> {
        self.observations
            .iter()
            .filter(|observation| {
                self.relocations
                    .iter()
                    .any(|relocation| observation.span.overlaps(relocation.source()))
            })
            .cloned()
            .collect()
    }
}
