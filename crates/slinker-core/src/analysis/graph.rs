use crate::syntax::source::Span;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct NodeId(pub usize);

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum NodeKind {
    Binding {
        name: String,
    },
    PrivateBinding {
        environment: String,
        name: String,
    },
    ClosureObject {
        owner: String,
        path: String,
        enclosure: String,
        derived: bool,
    },
    Activation,
    Dataset {
        name: String,
    },
    Lifecycle {
        hook: String,
    },
    S3Registration {
        generic: String,
        class: String,
    },
    Resource {
        path: String,
    },
    NativeComponent {
        name: String,
    },
    ExternalBinding {
        name: String,
    },
    PackageMetadata {
        name: String,
    },
    MissingPackage,
}

#[derive(Debug, Clone, Serialize)]
pub struct Node {
    pub id: NodeId,
    pub package: String,
    pub kind: NodeKind,
    pub span: Option<Span>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EdgeKind {
    Root,
    Lexical,
    Import,
    PackageQualified,
    NamespaceLoad,
    Export,
    S3Registration,
    Lifecycle,
    Resource,
    Dataset,
    Native,
    Callback,
    ClosureCapture,
    ClosureExecution,
    Discovery,
    Effect,
}

#[derive(Debug, Clone, Serialize)]
pub struct Edge {
    pub from: NodeId,
    pub to: NodeId,
    pub kind: EdgeKind,
    pub reason: String,
    pub span: Option<Span>,
}

#[derive(Clone, Debug, Default)]
pub struct Graph {
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
    outgoing: Vec<Vec<usize>>,
    incoming: Vec<Vec<usize>>,
    keys: HashMap<(String, NodeKind), NodeId>,
    edge_keys: HashSet<(NodeId, NodeId, EdgeKind, String, Option<Span>)>,
}

impl Graph {
    pub fn add_node(
        &mut self,
        package: impl Into<String>,
        kind: NodeKind,
        span: Option<Span>,
    ) -> NodeId {
        let package = package.into();
        let key = (package.clone(), kind.clone());
        if let Some(id) = self.keys.get(&key) {
            return *id;
        }
        let id = NodeId(self.nodes.len());
        self.nodes.push(Node {
            id,
            package,
            kind,
            span,
        });
        self.outgoing.push(Vec::new());
        self.incoming.push(Vec::new());
        self.keys.insert(key, id);
        id
    }

    pub fn add_edge(
        &mut self,
        from: NodeId,
        to: NodeId,
        kind: EdgeKind,
        reason: impl Into<String>,
        span: Option<Span>,
    ) -> bool {
        let reason = reason.into();
        let key = (from, to, kind, reason.clone(), span.clone());
        if !self.edge_keys.insert(key) {
            return false;
        }
        let index = self.edges.len();
        self.edges.push(Edge {
            from,
            to,
            kind,
            reason,
            span,
        });
        self.outgoing[from.0].push(index);
        self.incoming[to.0].push(index);
        true
    }

    pub fn binding(&self, package: &str, name: &str) -> Option<NodeId> {
        self.node_id(
            package,
            &NodeKind::Binding {
                name: name.to_owned(),
            },
        )
    }

    pub fn activation(&self, package: &str) -> Option<NodeId> {
        self.node_id(package, &NodeKind::Activation)
    }

    pub fn missing_packages(&self) -> impl Iterator<Item = NodeId> + '_ {
        self.nodes
            .iter()
            .filter(|node| matches!(&node.kind, NodeKind::MissingPackage))
            .map(|node| node.id)
    }

    pub fn incoming(&self, node: NodeId) -> impl Iterator<Item = &Edge> {
        self.incoming[node.0]
            .iter()
            .map(|index| &self.edges[*index])
    }

    pub fn outgoing(&self, node: NodeId) -> impl Iterator<Item = &Edge> {
        self.outgoing[node.0]
            .iter()
            .map(|index| &self.edges[*index])
    }

    fn node_id(&self, package: &str, kind: &NodeKind) -> Option<NodeId> {
        self.keys.get(&(package.to_owned(), kind.clone())).copied()
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
            for &edge_index in &self.outgoing[id.0] {
                let to = self.edges[edge_index].to;
                if !seen[to.0] {
                    seen[to.0] = true;
                    queue.push_back(to);
                }
            }
        }
        seen
    }

    pub fn shortest_path(&self, roots: &[NodeId], target: NodeId) -> Option<Vec<&Edge>> {
        let mut previous: Vec<Option<(NodeId, usize)>> = vec![None; self.nodes.len()];
        let mut seen = vec![false; self.nodes.len()];
        let mut queue = VecDeque::new();
        for &root in roots {
            seen[root.0] = true;
            queue.push_back(root);
        }
        while let Some(current) = queue.pop_front() {
            if current == target {
                break;
            }
            for &edge_index in &self.outgoing[current.0] {
                let edge = &self.edges[edge_index];
                if !seen[edge.to.0] {
                    seen[edge.to.0] = true;
                    previous[edge.to.0] = Some((current, edge_index));
                    queue.push_back(edge.to);
                }
            }
        }
        if !seen[target.0] {
            return None;
        }
        let mut path = Vec::new();
        let mut current = target;
        while let Some((from, edge_index)) = previous[current.0] {
            path.push(&self.edges[edge_index]);
            current = from;
        }
        path.reverse();
        Some(path)
    }
}
