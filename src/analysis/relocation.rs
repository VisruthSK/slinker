use super::NodeId;
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

#[derive(Clone, Debug)]
pub(super) struct SyntaxObservation {
    pub(super) node: NodeId,
    pub(super) package: PackageId,
    pub(super) span: Span,
    pub(super) kind: String,
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
