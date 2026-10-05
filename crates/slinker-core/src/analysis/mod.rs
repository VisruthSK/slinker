mod activation;
mod arguments;
mod calls;
mod dataset;
pub mod diagnostic;
mod discovery;
mod dynamic_names;
pub mod explain;
pub mod export;
mod finalize;
pub mod graph;
mod guarded;
mod guards;
mod invocation;
mod namespace;
mod native;
pub mod need;
mod parse_cache;
mod process;
mod reflection;
mod relocation;
mod resolution;
mod s3;
mod scheduler;
mod state;

use crate::Result;
use crate::package::{PackageName, PackageProvider};
use state::{AnalysisOptions, AnalyzerState};
use std::collections::HashSet;
use std::sync::Arc;

pub use diagnostic::{Diagnostic, Evidence, RejectCode};
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
pub use need::{GenericId, LifecycleHook, Need, S3Id, Schedule};

pub const ANALYSIS_STACK_BYTES: usize = 64 * 1024 * 1024;

pub struct Linker<P: PackageProvider> {
    packages: P,
    options: AnalysisOptions,
}

impl<P: PackageProvider> Linker<P> {
    pub fn new(packages: P, threads: usize) -> Self {
        Self {
            packages,
            options: AnalysisOptions {
                threads,
                schedule: Schedule::default(),
                provenance: true,
                linked_packages: HashSet::new(),
                explicit_external_packages: HashSet::new(),
                root_description: None,
            },
        }
    }

    #[doc(hidden)]
    #[must_use]
    pub fn with_schedule(mut self, schedule: Schedule) -> Self {
        self.options.schedule = schedule;
        self
    }

    #[must_use]
    pub fn without_provenance(mut self) -> Self {
        self.options.provenance = false;
        self
    }

    #[must_use]
    pub fn with_linked_packages(
        mut self,
        packages: impl IntoIterator<Item = impl Into<PackageName>>,
    ) -> Self {
        self.options
            .linked_packages
            .extend(packages.into_iter().map(Into::into));
        self
    }

    #[must_use]
    pub fn with_external_packages(
        mut self,
        packages: impl IntoIterator<Item = impl Into<PackageName>>,
    ) -> Self {
        self.options
            .explicit_external_packages
            .extend(packages.into_iter().map(Into::into));
        self
    }

    #[must_use]
    pub fn with_root_source(mut self, description: impl Into<Arc<str>>) -> Self {
        self.options.root_description = Some(description.into());
        self
    }

    pub fn analyze(self, root: &str) -> Result<LinkIr> {
        let _span = crate::profile::span(crate::profile::Probe::Analysis);
        let state = AnalyzerState::new(self.packages, root, self.options)?.run()?;
        let _finalize = crate::profile::span(crate::profile::Probe::Finalize);
        state.finalize()
    }
}
