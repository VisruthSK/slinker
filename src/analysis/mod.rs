mod arguments;
pub mod diagnostic;
mod execute;
pub mod explain;
pub mod export;
mod finalize;
pub mod graph;
mod namespace;
mod native;
pub mod need;
mod object_world;
mod parse_cache;
pub mod policy;
mod reflection;
mod relocation;
mod resolution;
mod s3;
mod state;

use crate::Result;
use crate::package::PackageProvider;
use state::AnalyzerState;
use std::sync::Arc;

pub use diagnostic::{Diagnostic, RejectCode};
pub use explain::{
    EXPLANATION_SCHEMA_VERSION, ExplanationComponent, ExplanationDag, ExplanationEdge,
    ExplanationError, ExplanationEvidence, ExplanationMember, ExplanationPackage, ExplanationRoot,
    ExplanationStats, PresentationClass, PresentationVisibility, ProjectedComponent,
    ProjectedExplanationEdge,
};
pub use export::{
    GraphBlockerExport, GraphEdgeReasonExport, GraphNodeKindExport, GraphSourceExport,
    PackageIdentityExport, TargetIdentityExport,
};
pub use finalize::LinkIr;
pub use graph::{Edge, EdgeKind, Graph, Node, NodeId, NodeKind};
pub use need::{GenericId, LifecycleHook, Need, S3Id};
pub use policy::{DiscoveryPolicy, LinkPolicy};

pub const ANALYSIS_STACK_BYTES: usize = 64 * 1024 * 1024;

/// Demand-driven analysis of one root package against a frozen target universe.
pub struct Linker<P: PackageProvider>(AnalyzerState<P>);

impl<P: PackageProvider> Linker<P> {
    pub fn new(packages: P, jobs: usize) -> Self {
        Self(AnalyzerState::new(packages, jobs))
    }

    #[must_use]
    pub fn with_policy(mut self, policy: LinkPolicy) -> Self {
        self.0.policy = policy;
        self
    }

    #[must_use]
    pub fn without_provenance(mut self) -> Self {
        self.0.provenance = false;
        self
    }

    #[must_use]
    pub fn with_extra_packages(mut self, packages: impl IntoIterator<Item = String>) -> Self {
        self.0.extra_packages.extend(packages);
        self
    }

    /// Keep the named packages External; frozen before any package resolves.
    #[must_use]
    pub fn with_external_packages(mut self, packages: impl IntoIterator<Item = String>) -> Self {
        let packages = packages.into_iter().collect::<Vec<_>>();
        self.0
            .packages
            .set_explicit_external(packages.iter().cloned());
        self.0.explicit_external_packages.extend(packages);
        self
    }

    /// Supply the frozen root DESCRIPTION used to plan the generated package.
    #[must_use]
    pub fn with_root_source(mut self, description: impl Into<Arc<str>>) -> Self {
        self.0.root_description = Some(description.into());
        self
    }

    /// Analyze `root` to a fixed point and finalize the immutable linked program.
    ///
    /// # Errors
    ///
    /// Fails on infrastructure errors such as an absent root or a worker failure; unsupported
    /// semantics accumulate as blockers in the returned [`LinkIr`] instead.
    pub fn analyze(self, root: &str) -> Result<LinkIr> {
        Ok(self.0.run(root)?.finalize())
    }
}
