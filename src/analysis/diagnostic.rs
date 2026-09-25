use crate::analysis::graph::NodeId;
use crate::syntax::source::Span;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RejectCode {
    AirUnsupportedSyntax,
    InvalidInstalledRepresentation,
    ParserDisagreement,
    ActiveBinding,
    ArbitraryEvaluation,
    DynamicLookup,
    DynamicPackageDiscovery,
    DependsAttachmentUnsupported,
    EnvironmentMutation,
    LifecycleHook,
    MissingDependency,
    MissingResource,
    ObjectSystem,
    PackageAttachmentUnsupported,
    PotentialUnboundLocal,
    SemanticAmbiguity,
    SyntaxObservation,
    UnknownClosureEnclosure,
    UnknownNativeEffects,
    UnknownNativeLookup,
    UnresolvedBinding,
    UnsupportedObject,
    UnsupportedRootTransformation,
    UnsupportedTopLevelEffect,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Diagnostic {
    pub package: String,
    pub binding: Option<String>,
    pub code: RejectCode,
    pub message: String,
    pub span: Option<Span>,
    pub node: Option<NodeId>,
}
