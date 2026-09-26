use crate::analysis::graph::NodeId;
use crate::syntax::source::Span;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RejectCode {
    AirUnsupportedSyntax,
    InvalidInstalledRepresentation,
    ParserDisagreement,
    ActiveBinding,
    ArbitraryEvaluation,
    DynamicLookup,
    InvalidDeclaration,
    DynamicPackageDiscovery,
    DependsAttachmentUnsupported,
    EnvironmentMutation,
    LifecycleHook,
    MissingDependency,
    MissingResource,
    ObjectSystem,
    PackageAttachmentUnsupported,
    SemanticAmbiguity,
    SyntaxObservation,
    UnknownClosureEnclosure,
    UnknownNativeEffects,
    UnknownNativeLookup,
    UnresolvedBinding,
    UnsupportedObject,
    UnsupportedLinkedLibname,
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

#[derive(Default)]
pub(super) struct DiagnosticSink {
    blockers: Vec<Diagnostic>,
    assumptions: Vec<Diagnostic>,
    recorded: HashSet<(NodeId, RejectCode, String)>,
}

impl DiagnosticSink {
    pub(super) fn record(&mut self, node: NodeId, blocking: bool, diagnostic: Diagnostic) {
        if !self
            .recorded
            .insert((node, diagnostic.code, diagnostic.message.clone()))
        {
            return;
        }
        if blocking {
            self.blockers.push(diagnostic);
        } else {
            self.assumptions.push(diagnostic);
        }
    }

    pub(super) fn into_sorted(self) -> (Vec<Diagnostic>, Vec<Diagnostic>) {
        let Self {
            mut blockers,
            mut assumptions,
            ..
        } = self;
        blockers.sort_by(diagnostic_order);
        assumptions.sort_by(diagnostic_order);
        (blockers, assumptions)
    }
}

fn diagnostic_order(left: &Diagnostic, right: &Diagnostic) -> std::cmp::Ordering {
    (&left.package, left.code, &left.binding, &left.message).cmp(&(
        &right.package,
        right.code,
        &right.binding,
        &right.message,
    ))
}
