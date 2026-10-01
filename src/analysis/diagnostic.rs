use crate::analysis::graph::NodeId;
use crate::package::PackageId;
use crate::syntax::source::Span;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};

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
                None => evidence.package.clone(),
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
    pub package: String,
    pub binding: Option<String>,
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
    MissingPackage(String),
    NameCreator {
        package: PackageId,
        binding: String,
        operation: &'static str,
        created: Option<String>,
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
mod tests {
    use super::*;

    fn primary(sites: &[(&str, Option<&str>)]) -> Diagnostic {
        Diagnostic {
            package: "gone".into(),
            binding: None,
            code: RejectCode::MissingDependency,
            message: String::new(),
            span: None,
            node: None,
            evidence: sites
                .iter()
                .map(|(package, binding)| Evidence {
                    package: (*package).into(),
                    binding: binding.map(str::to_owned),
                    span: None,
                    detail: String::new(),
                })
                .collect(),
        }
    }

    #[test]
    fn evidence_summary_names_distinct_sites_and_counts_the_rest() {
        assert_eq!(primary(&[]).evidence_summary(), None);
        assert_eq!(
            primary(&[("a", Some("f")), ("a", Some("f")), ("b", None)])
                .evidence_summary()
                .as_deref(),
            Some("a::f, b")
        );
        let many = (0..7)
            .map(|index| ("p", Some(["a", "b", "c", "d", "e", "f", "g"][index])))
            .collect::<Vec<_>>();
        assert_eq!(
            primary(&many).evidence_summary().as_deref(),
            Some("p::a, p::b, p::c, p::d and 3 more")
        );
    }
}
