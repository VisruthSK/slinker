use std::collections::BTreeSet;
use std::fmt;

use crate::{Capability, PackageId};

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Hash)]
pub struct NodeId(pub(crate) u32);

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum NodeKind {
    Binding { name: String },
    NamespaceActivation,
    Initialization { ordinal: u32 },
    LifecycleHook { name: String },
    S3Registration { generic: String, method: String },
    Resource { path: String },
    SerializedObject { name: String },
    NativeComponent { name: String },
    BuildInput { path: String },
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum EdgeKind {
    LexicalUse,
    NamespaceAccess,
    Import,
    Activation,
    Initialization,
    S3,
    Resource,
    Serialization,
    Native,
    Build,
    TraceSpecialization,
}

#[derive(Clone, Debug)]
pub struct Node {
    pub id: NodeId,
    pub package: PackageId,
    pub kind: NodeKind,
    pub capabilities: BTreeSet<Capability>,
    pub transformed_syntax: bool,
    pub dynamic_package: Option<String>,
}

impl Node {
    pub fn new(package: PackageId, kind: NodeKind) -> Self {
        Self {
            id: NodeId(u32::MAX),
            package,
            kind,
            capabilities: BTreeSet::new(),
            transformed_syntax: false,
            dynamic_package: None,
        }
    }

    pub fn with_capability(mut self, capability: Capability) -> Self {
        self.capabilities.insert(capability);
        self
    }

    pub fn with_transformed_syntax(mut self) -> Self {
        self.transformed_syntax = true;
        self
    }

    pub fn with_dynamic_package(mut self, package: impl Into<String>) -> Self {
        self.dynamic_package = Some(package.into());
        self
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Edge {
    pub to: NodeId,
    pub kind: EdgeKind,
    pub reason: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RetentionStep {
    pub node: NodeId,
    pub via: Option<EdgeKind>,
    pub reason: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct Graph {
    nodes: Vec<Node>,
    edges: Vec<Vec<Edge>>,
    roots: BTreeSet<NodeId>,
}

impl Graph {
    pub fn add_node(&mut self, mut node: Node) -> NodeId {
        let id = NodeId(self.nodes.len() as u32);
        node.id = id;
        self.nodes.push(node);
        self.edges.push(Vec::new());
        id
    }

    pub fn add_edge(&mut self, from: NodeId, to: NodeId, kind: EdgeKind) {
        self.add_edge_with_reason(from, to, kind, None);
    }

    pub fn add_edge_with_reason(
        &mut self,
        from: NodeId,
        to: NodeId,
        kind: EdgeKind,
        reason: Option<String>,
    ) {
        let edges = &mut self.edges[from.0 as usize];
        if edges
            .iter()
            .any(|edge| edge.to == to && edge.kind == kind && edge.reason == reason)
        {
            return;
        }
        edges.push(Edge { to, kind, reason });
        edges.sort_by(|left, right| {
            (left.to, left.kind, &left.reason).cmp(&(right.to, right.kind, &right.reason))
        });
    }

    pub fn add_root(&mut self, node: NodeId) {
        self.roots.insert(node);
    }

    pub fn node(&self, id: NodeId) -> &Node {
        &self.nodes[id.0 as usize]
    }

    pub fn edges(&self, id: NodeId) -> &[Edge] {
        &self.edges[id.0 as usize]
    }

    pub fn roots(&self) -> impl Iterator<Item = NodeId> + '_ {
        self.roots.iter().copied()
    }

    pub fn nodes(&self) -> impl Iterator<Item = &Node> {
        self.nodes.iter()
    }

    pub fn all_edges(&self) -> impl Iterator<Item = (NodeId, &Edge)> {
        self.edges.iter().enumerate().flat_map(|(from, edges)| {
            let from = NodeId(from as u32);
            edges.iter().map(move |edge| (from, edge))
        })
    }

    pub fn package_nodes(&self, package: PackageId) -> impl Iterator<Item = NodeId> + '_ {
        self.nodes
            .iter()
            .filter(move |node| node.package == package)
            .map(|node| node.id)
    }
}

impl fmt::Display for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "node#{}", self.0)
    }
}
