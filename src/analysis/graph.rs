use crate::analysis::source::Span;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct NodeId(pub usize);

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum NodeKind {
    Binding { name: String },
    NamespaceDirective { text: String },
    Initialization { ordinal: usize },
    Lifecycle { hook: String },
    S3Registration { generic: String, class: String },
    Resource { path: String },
    SerializedObject { name: String },
    NativeComponent { name: String },
    BuildInput { path: String },
    Rejection { code: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Node {
    pub id: NodeId,
    pub package: String,
    pub kind: NodeKind,
    pub span: Option<Span>,
    pub bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EdgeKind {
    Lexical,
    Initialization,
    Import,
    PackageQualified,
    NamespaceLoad,
    Export,
    ReExport,
    S3Registration,
    Lifecycle,
    Resource,
    Native,
    Build,
    Effect,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Edge {
    pub from: NodeId,
    pub to: NodeId,
    pub kind: EdgeKind,
    pub reason: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Graph {
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
    #[serde(skip)]
    outgoing: Vec<Vec<usize>>,
    #[serde(skip)]
    keys: HashMap<(String, NodeKind), NodeId>,
}

impl Graph {
    pub fn from_parts(nodes: Vec<Node>, edges: Vec<Edge>) -> Self {
        let mut graph = Self { nodes, edges, ..Self::default() };
        graph.rebuild_indexes();
        graph
    }

    pub fn add_node(&mut self, package: impl Into<String>, kind: NodeKind, span: Option<Span>) -> NodeId {
        let package = package.into();
        let key = (package.clone(), kind.clone());
        if let Some(id) = self.keys.get(&key) {
            return *id;
        }
        let id = NodeId(self.nodes.len());
        self.nodes.push(Node { id, package: package.clone(), kind: kind.clone(), span, bytes: 0 });
        self.outgoing.push(Vec::new());
        self.keys.insert(key, id);
        id
    }

    pub fn set_bytes(&mut self, id: NodeId, bytes: u64) {
        self.nodes[id.0].bytes = bytes;
    }

    pub fn add_edge(&mut self, from: NodeId, to: NodeId, kind: EdgeKind, reason: impl Into<String>) {
        let reason = reason.into();
        if self.outgoing[from.0].iter().any(|&index| {
            let edge = &self.edges[index];
            edge.to == to && edge.kind == kind && edge.reason == reason
        }) {
            return;
        }
        let idx = self.edges.len();
        self.edges.push(Edge { from, to, kind, reason });
        self.outgoing[from.0].push(idx);
    }

    pub fn binding(&self, package: &str, name: &str) -> Option<NodeId> {
        self.node_id(package, &NodeKind::Binding { name: name.to_string() })
    }

    pub fn initialization(&self, package: &str, ordinal: usize) -> Option<NodeId> {
        self.node_id(package, &NodeKind::Initialization { ordinal })
    }

    pub fn activation(&self, package: &str) -> Option<NodeId> {
        self.node_id(
            package,
            &NodeKind::Lifecycle {
                hook: "activation".into(),
            },
        )
    }

    fn node_id(&self, package: &str, kind: &NodeKind) -> Option<NodeId> {
        self.keys
            .get(&(package.to_string(), kind.clone()))
            .copied()
    }

    pub fn reachable(&self, roots: impl IntoIterator<Item = NodeId>) -> Vec<bool> {
        let mut seen = vec![false; self.nodes.len()];
        let mut queue = VecDeque::new();
        for root in roots {
            if !seen[root.0] {
                seen[root.0] = true;
                queue.push_back(root);
            }
        }
        while let Some(id) = queue.pop_front() {
            for &edge_idx in &self.outgoing[id.0] {
                let to = self.edges[edge_idx].to;
                if !seen[to.0] {
                    seen[to.0] = true;
                    queue.push_back(to);
                }
            }
        }
        seen
    }

    pub fn shortest_path(&self, roots: &[NodeId], target: NodeId) -> Option<Vec<&Edge>> {
        let mut prev: Vec<Option<(NodeId, usize)>> = vec![None; self.nodes.len()];
        let mut seen = vec![false; self.nodes.len()];
        let mut q = VecDeque::new();
        for &r in roots {
            seen[r.0] = true;
            q.push_back(r);
        }
        while let Some(cur) = q.pop_front() {
            if cur == target { break; }
            for &ei in &self.outgoing[cur.0] {
                let edge = &self.edges[ei];
                if !seen[edge.to.0] {
                    seen[edge.to.0] = true;
                    prev[edge.to.0] = Some((cur, ei));
                    q.push_back(edge.to);
                }
            }
        }
        if !seen[target.0] { return None; }
        let mut edges = Vec::new();
        let mut cur = target;
        while let Some((from, ei)) = prev[cur.0] {
            edges.push(&self.edges[ei]);
            cur = from;
        }
        edges.reverse();
        Some(edges)
    }

    pub fn rebuild_indexes(&mut self) {
        self.outgoing = vec![Vec::new(); self.nodes.len()];
        self.keys.clear();
        for node in &self.nodes {
            self.keys.insert((node.package.clone(), node.kind.clone()), node.id);
        }
        for (i, edge) in self.edges.iter().enumerate() {
            self.outgoing[edge.from.0].push(i);
        }
    }
}
