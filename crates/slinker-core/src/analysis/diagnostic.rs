use crate::analysis::dynamic_names::CreatorOperation;
use crate::analysis::graph::NodeId;
use crate::package::{BindingName, PackageId, PackageName};
use crate::syntax::source::Span;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RejectCode {
    AirUnsupportedSyntax,
    InvalidInstalledRepresentation,
    ActiveBinding,
    DynamicLookup,
    InvalidDeclaration,
    DynamicPackageDiscovery,
    EnvironmentMutation,
    MissingDependency,
    MissingResource,
    NativeLoadFailure,
    ObjectSystem,
    OptionalAvailability,
    PackageAttachmentUnsupported,
    SemanticAmbiguity,
    SyntaxObservation,
    UnknownClosureEnclosure,
    UnknownNativeEffects,
    UnknownNativeLookup,
    UnresolvedBinding,
    UnsupportedObject,
    UnsupportedResourcePath,
    UnsupportedLinkedLibname,
    UnsupportedRootTransformation,
    UnsupportedTopLevelEffect,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Diagnostic {
    pub package: PackageName,
    pub binding: Option<BindingName>,
    pub code: RejectCode,
    pub message: String,
    pub span: Option<Span>,
    pub node: NodeId,
    #[serde(default)]
    pub evidence: Vec<Evidence>,
}

impl Diagnostic {
    pub fn evidence_summary(&self) -> Option<String> {
        const SHOWN: usize = 4;
        let mut sites = Vec::new();
        for evidence in &self.evidence {
            let site = match &evidence.binding {
                Some(binding) => format!("{}::{binding}", evidence.package),
                None => evidence.package.to_string(),
            };
            if !sites.contains(&site) {
                sites.push(site);
            }
        }
        let (shown, hidden) = sites.split_at(sites.len().min(SHOWN));
        if shown.is_empty() {
            return None;
        }
        let mut summary = shown.join(", ");
        if !hidden.is_empty() {
            summary.push_str(&format!(" and {} more", hidden.len()));
        }
        Some(summary)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Evidence {
    pub package: PackageName,
    pub binding: Option<BindingName>,
    pub span: Option<Span>,
    pub detail: String,
}

impl Evidence {
    fn compare(&self, other: &Self) -> std::cmp::Ordering {
        let location = |evidence: &Self| {
            evidence
                .span
                .as_ref()
                .map(|span| (span.source.0, span.start, span.end))
        };
        self.package
            .cmp(&other.package)
            .then_with(|| self.binding.cmp(&other.binding))
            .then_with(|| location(self).cmp(&location(other)))
            .then_with(|| self.detail.cmp(&other.detail))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(super) enum Cause {
    MissingPackage(PackageName),
    NameCreator {
        package: PackageId,
        binding: BindingName,
        operation: CreatorOperation,
        created: Option<BindingName>,
    },
}

#[derive(Default)]
pub(super) struct DiagnosticSink {
    blockers: Vec<Diagnostic>,
    recorded: HashSet<(NodeId, RejectCode, String)>,
    derived: BTreeMap<Cause, Diagnostic>,
}

impl DiagnosticSink {
    pub(super) fn record(&mut self, node: NodeId, diagnostic: Diagnostic) {
        if self
            .recorded
            .insert((node, diagnostic.code, diagnostic.message.clone()))
        {
            self.blockers.push(diagnostic);
        }
    }

    pub(super) fn record_derived(&mut self, cause: Cause, primary: Diagnostic, evidence: Evidence) {
        let grouped = self.derived.entry(cause).or_insert(primary);
        if !grouped.evidence.contains(&evidence) {
            grouped.evidence.push(evidence);
        }
    }

    pub(super) fn into_sorted(self) -> Vec<Diagnostic> {
        let mut blockers = self.blockers;
        blockers.extend(self.derived.into_values().map(|mut primary| {
            primary.evidence.sort_by(Evidence::compare);
            primary
        }));
        blockers.sort_by(diagnostic_order);
        blockers
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

#[cfg(test)]
#[path = "../../tests/unit/analysis/diagnostic.rs"]
mod tests;
