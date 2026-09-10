use crate::analysis::graph::NodeId;
use crate::analysis::source::Span;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RejectCode {
    ActiveBinding,
    ArbitraryEvaluation,
    DynamicLookup,
    DynamicPackageDiscovery,
    EnvironmentMutation,
    LifecycleHook,
    MissingDependency,
    NonTargetDepends,
    ObjectSystem,
    SyntaxObservation,
    UnknownNamespaceDirective,
    UnresolvedBinding,
    UnsupportedTopLevelEffect,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Diagnostic {
    pub package: String,
    pub code: RejectCode,
    pub message: String,
    pub span: Option<Span>,
    pub node: Option<NodeId>,
    pub reachable: bool,
}

impl Diagnostic {
    pub fn reject(package: impl Into<String>, code: RejectCode, message: impl Into<String>) -> Self {
        Self {
            package: package.into(),
            code,
            message: message.into(),
            span: None,
            node: None,
            reachable: false,
        }
    }
}
