use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;

use crate::{
    Capability, EdgeKind, Graph, NodeId, NodeKind, PackageId, PackageRole, PackageSet,
    RetentionStep,
};

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum RejectionCode {
    NonProvidedDepends,
    UnsupportedCapability,
    SyntaxObservationAfterRewrite,
    UnresolvedDynamicPackage,
    AmbientPackageLookup,
    UnmodeledOnLoad,
}

impl RejectionCode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NonProvidedDepends => "non_provided_depends",
            Self::UnsupportedCapability => "unsupported_capability",
            Self::SyntaxObservationAfterRewrite => "syntax_observation_after_rewrite",
            Self::UnresolvedDynamicPackage => "unresolved_dynamic_package",
            Self::AmbientPackageLookup => "ambient_package_lookup",
            Self::UnmodeledOnLoad => "unmodeled_on_load",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Rejection {
    NonProvidedDepends { package: String },
    UnsupportedCapability { node: NodeId, capability: Capability },
    SyntaxObservationAfterRewrite { node: NodeId },
    UnresolvedDynamicPackage { node: NodeId },
    AmbientPackageLookup { node: NodeId, package: String },
    UnmodeledOnLoad { node: NodeId },
}

impl Rejection {
    pub const fn code(&self) -> RejectionCode {
        match self {
            Self::NonProvidedDepends { .. } => RejectionCode::NonProvidedDepends,
            Self::UnsupportedCapability { .. } => RejectionCode::UnsupportedCapability,
            Self::SyntaxObservationAfterRewrite { .. } => {
                RejectionCode::SyntaxObservationAfterRewrite
            }
            Self::UnresolvedDynamicPackage { .. } => RejectionCode::UnresolvedDynamicPackage,
            Self::AmbientPackageLookup { .. } => RejectionCode::AmbientPackageLookup,
            Self::UnmodeledOnLoad { .. } => RejectionCode::UnmodeledOnLoad,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Diagnostic {
    pub rejection: Rejection,
    /// Compatibility path containing only node and edge kind.
    pub path: Vec<(NodeId, Option<EdgeKind>)>,
    /// Proof path with the human reason attached to every incoming edge.
    pub detailed_path: Vec<RetentionStep>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LinkError {
    pub diagnostics: Vec<Diagnostic>,
    pub plan: LinkPlan,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LinkPlan {
    retained: BTreeSet<NodeId>,
    predecessor: BTreeMap<NodeId, Predecessor>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Predecessor {
    node: NodeId,
    kind: EdgeKind,
    reason: Option<String>,
}

impl LinkPlan {
    pub fn retains(&self, node: NodeId) -> bool {
        self.retained.contains(&node)
    }

    pub fn retained(&self) -> impl Iterator<Item = NodeId> + '_ {
        self.retained.iter().copied()
    }

    pub fn why(&self, node: NodeId) -> Option<Vec<(NodeId, Option<EdgeKind>)>> {
        self.why_detailed(node).map(|path| {
            path.into_iter()
                .map(|step| (step.node, step.via))
                .collect()
        })
    }

    pub fn why_detailed(&self, node: NodeId) -> Option<Vec<RetentionStep>> {
        if !self.retained.contains(&node) {
            return None;
        }
        Some(build_detailed_path(node, &self.predecessor))
    }
}

pub struct Linker<'a> {
    graph: &'a Graph,
    packages: &'a PackageSet,
}

impl<'a> Linker<'a> {
    pub fn new(graph: &'a Graph, packages: &'a PackageSet) -> Self {
        Self { graph, packages }
    }

    pub fn link(&self) -> Result<LinkPlan, LinkError> {
        let mut diagnostics = Vec::new();
        let mut retained = BTreeSet::new();
        let mut predecessor = BTreeMap::new();
        let mut queue = VecDeque::new();
        let mut checked_packages = BTreeSet::<PackageId>::new();

        for root in self.graph.roots() {
            if retained.insert(root) {
                queue.push_back(root);
            }
        }

        while let Some(node_id) = queue.pop_front() {
            let node = self.graph.node(node_id);
            if checked_packages.insert(node.package) {
                self.check_package(node_id, &retained, &predecessor, &mut diagnostics);
            }
            self.check_node(node_id, &retained, &predecessor, &mut diagnostics);

            for edge in self.graph.edges(node_id) {
                if retained.insert(edge.to) {
                    predecessor.insert(
                        edge.to,
                        Predecessor {
                            node: node_id,
                            kind: edge.kind,
                            reason: edge.reason.clone(),
                        },
                    );
                    queue.push_back(edge.to);
                }
            }

            if matches!(node.kind, NodeKind::NativeComponent { .. }) {
                for widened in self.graph.package_nodes(node.package) {
                    if retained.insert(widened) {
                        predecessor.insert(
                            widened,
                            Predecessor {
                                node: node_id,
                                kind: EdgeKind::Native,
                                reason: Some(
                                    "reachable native component widens the package atomically"
                                        .to_owned(),
                                ),
                            },
                        );
                        queue.push_back(widened);
                    }
                }
            }
        }

        diagnostics.sort_by_key(|diagnostic| diagnostic_sort_key(&diagnostic.rejection));
        diagnostics.dedup_by(|left, right| left.rejection == right.rejection);

        if diagnostics.is_empty() {
            Ok(LinkPlan {
                retained,
                predecessor,
            })
        } else {
            Err(LinkError {
                diagnostics,
                plan: LinkPlan {
                    retained,
                    predecessor,
                },
            })
        }
    }

    fn check_package(
        &self,
        witness: NodeId,
        retained: &BTreeSet<NodeId>,
        predecessor: &BTreeMap<NodeId, Predecessor>,
        diagnostics: &mut Vec<Diagnostic>,
    ) {
        let package = self.packages.get(self.graph.node(witness).package);
        if package.role != PackageRole::TargetProvided && package.has_nonprovided_depends {
            push_diagnostic(
                diagnostics,
                Rejection::NonProvidedDepends {
                    package: package.name.clone(),
                },
                witness,
                retained,
                predecessor,
            );
        }
    }

    fn check_node(
        &self,
        node_id: NodeId,
        retained: &BTreeSet<NodeId>,
        predecessor: &BTreeMap<NodeId, Predecessor>,
        diagnostics: &mut Vec<Diagnostic>,
    ) {
        let node = self.graph.node(node_id);

        if node.transformed_syntax && node.capabilities.contains(&Capability::SyntaxObservation) {
            push_diagnostic(
                diagnostics,
                Rejection::SyntaxObservationAfterRewrite { node: node_id },
                node_id,
                retained,
                predecessor,
            );
        }

        if node.capabilities.contains(&Capability::OnLoad) {
            push_diagnostic(
                diagnostics,
                Rejection::UnmodeledOnLoad { node: node_id },
                node_id,
                retained,
                predecessor,
            );
        }

        if node.capabilities.contains(&Capability::DynamicPackageLookup) {
            match node.dynamic_package.as_deref() {
                None => push_diagnostic(
                    diagnostics,
                    Rejection::UnresolvedDynamicPackage { node: node_id },
                    node_id,
                    retained,
                    predecessor,
                ),
                Some(name) => match self.packages.find(name) {
                    Some(_) => {}
                    None => push_diagnostic(
                        diagnostics,
                        Rejection::AmbientPackageLookup {
                            node: node_id,
                            package: name.to_owned(),
                        },
                        node_id,
                        retained,
                        predecessor,
                    ),
                },
            }
        }

        for capability in &node.capabilities {
            if is_supported(*capability) {
                continue;
            }
            push_diagnostic(
                diagnostics,
                Rejection::UnsupportedCapability {
                    node: node_id,
                    capability: *capability,
                },
                node_id,
                retained,
                predecessor,
            );
        }
    }
}

fn push_diagnostic(
    diagnostics: &mut Vec<Diagnostic>,
    rejection: Rejection,
    witness: NodeId,
    retained: &BTreeSet<NodeId>,
    predecessor: &BTreeMap<NodeId, Predecessor>,
) {
    let detailed_path = path_to_detailed(witness, retained, predecessor);
    let path = detailed_path
        .iter()
        .map(|step| (step.node, step.via))
        .collect();
    diagnostics.push(Diagnostic {
        rejection,
        path,
        detailed_path,
    });
}

fn is_supported(capability: Capability) -> bool {
    matches!(
        capability,
        Capability::Pure
            | Capability::SyntaxObservation
            | Capability::DynamicPackageLookup
            | Capability::NativeOpaque
            | Capability::OnLoad
    )
}

fn path_to_detailed(
    node: NodeId,
    retained: &BTreeSet<NodeId>,
    predecessor: &BTreeMap<NodeId, Predecessor>,
) -> Vec<RetentionStep> {
    if !retained.contains(&node) {
        return Vec::new();
    }
    build_detailed_path(node, predecessor)
}

fn build_detailed_path(
    node: NodeId,
    predecessor: &BTreeMap<NodeId, Predecessor>,
) -> Vec<RetentionStep> {
    let mut nodes = vec![node];
    let mut cursor = node;
    while let Some(parent) = predecessor.get(&cursor) {
        nodes.push(parent.node);
        cursor = parent.node;
    }
    nodes.reverse();

    nodes
        .into_iter()
        .map(|id| match predecessor.get(&id) {
            None => RetentionStep {
                node: id,
                via: None,
                reason: None,
            },
            Some(parent) => RetentionStep {
                node: id,
                via: Some(parent.kind),
                reason: parent.reason.clone(),
            },
        })
        .collect()
}

fn diagnostic_sort_key(rejection: &Rejection) -> (RejectionCode, u32, String) {
    match rejection {
        Rejection::NonProvidedDepends { package } => {
            (rejection.code(), 0, package.clone())
        }
        Rejection::UnsupportedCapability { node, capability } => {
            (rejection.code(), node.0, format!("{capability:?}"))
        }
        Rejection::SyntaxObservationAfterRewrite { node }
        | Rejection::UnresolvedDynamicPackage { node }
        | Rejection::UnmodeledOnLoad { node } => {
            (rejection.code(), node.0, String::new())
        }
        Rejection::AmbientPackageLookup { node, package } => {
            (rejection.code(), node.0, package.clone())
        }
    }
}

impl fmt::Display for LinkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "link rejected with {} diagnostic(s)", self.diagnostics.len())
    }
}

impl std::error::Error for LinkError {}
