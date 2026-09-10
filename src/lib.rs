//! Conservative staging, inspection, linking, and materialization primitives for heRmetic.
//!
//! Air owns R syntax. `hrm` owns target-toolchain staging, the typed semantic
//! graph, pre-activation installed-state inspection, reachability, rejection,
//! and construction of canonical synthetic package environments.

mod graph;
mod inspection;
mod linker;
mod materialize;
mod metadata;
mod model;
mod native;
#[cfg(feature = "air")]
mod pipeline;
mod report;
mod staging;
mod target_env;
mod toolchain;

#[cfg(feature = "air")]
pub mod air;

pub use graph::{Edge, EdgeKind, Graph, Node, NodeId, NodeKind, RetentionStep};
pub use inspection::{
    BindingOrigin, ClosureEnvironment, ExportedBinding, ImportDirective, ImportedBinding, InspectError,
    ObjectIssue, ObjectState, S3Registration, SemanticSnapshot, SemanticState,
};
pub use linker::{Diagnostic, LinkError, LinkPlan, Linker, Rejection, RejectionCode};
pub use materialize::{
    MaterializationRequest, MaterializeError, MaterializedArtifact, PackageMaterialization,
};
pub use metadata::{Dependency, Description, MetadataError};
pub use model::{Capability, Package, PackageId, PackageRole, PackageSet, PackageSetError, Target};
pub use native::{NativeHazard, NativeScanError, scan_native_tree};
pub use report::{LinkReport, ReportWriteError, REPORT_SCHEMA_VERSION};
#[cfg(feature = "air")]
pub use pipeline::{PrepareError, PreparedPackage};
pub use staging::{
    ConfiguredSourceView, EffectiveMetadata, InstalledSemanticView, StageError, StageRequest,
    StagedPackage,
};
pub use target_env::{
    TargetEnvironment, TargetEnvironmentError, TargetEnvironmentRequest, TargetProvidedPackage,
};
pub use toolchain::{RToolchain, ToolchainError};
