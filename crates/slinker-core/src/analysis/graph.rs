use crate::package::{
    BindingName, ClassName, ComponentName, DatasetName, EnvironmentLabel, GenericLabel, MemberPath,
    PackageName, ResourcePath,
};
use crate::syntax::source::{SourceKey, Span};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct NodeId(pub usize);

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum NodeKind {
    Binding {
        name: BindingName,
    },
    PrivateBinding {
        environment: EnvironmentLabel,
        name: BindingName,
    },
    ClosureObject {
        owner: SourceKey,
        path: MemberPath,
        enclosure: EnvironmentLabel,
    },
    Activation,
    Dataset {
        name: DatasetName,
    },
    Lifecycle {
        hook: BindingName,
    },
    S3Registration {
        generic: GenericLabel,
        class: ClassName,
    },
    Resource {
        path: ResourcePath,
    },
    NativeComponent {
        name: ComponentName,
    },
    ExternalBinding {
        name: BindingName,
    },
    PackageMetadata {
        name: BindingName,
    },
    MissingPackage,
}

#[derive(Debug, Clone, Serialize)]
pub struct Node {
    pub id: NodeId,
    pub package: PackageName,
    pub kind: NodeKind,
    pub span: Option<Span>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EdgeKind {
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
    keys: HashMap<(PackageName, NodeKind), NodeId>,
    edge_keys: HashSet<(NodeId, NodeId, EdgeKind, String, Option<Span>)>,
}

impl Graph {
    pub fn add_node(&mut self, package: PackageName, kind: NodeKind, span: Option<Span>) -> NodeId {
        let key = (package, kind);
        if let Some(&id) = self.keys.get(&key) {
            return id;
        }
        let id = NodeId(self.nodes.len());
        let (package, kind) = key.clone();
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
    ) {
        let reason = reason.into();
        if !self
            .edge_keys
            .insert((from, to, kind, reason.clone(), span.clone()))
        {
            return;
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
    }

    pub fn binding(&self, package: &PackageName, name: &BindingName) -> Option<NodeId> {
        let kind = NodeKind::Binding { name: name.clone() };
        self.keys.get(&(package.clone(), kind)).copied()
    }

    pub fn missing_packages(&self) -> impl Iterator<Item = NodeId> + '_ {
        self.nodes
            .iter()
            .filter(|node| matches!(node.kind, NodeKind::MissingPackage))
            .map(|node| node.id)
    }

    pub fn incoming(&self, node: NodeId) -> impl Iterator<Item = &Edge> {
        self.incoming[node.0]
            .iter()
            .map(|&index| &self.edges[index])
    }

    pub fn shortest_path(&self, roots: &[NodeId], target: NodeId) -> Option<Vec<&Edge>> {
        let mut previous = vec![None::<(NodeId, usize)>; self.nodes.len()];
        let mut seen = vec![false; self.nodes.len()];
        let mut queue = VecDeque::new();
        for &root in roots {
            seen[root.0] = true;
            queue.push_back(root);
        }
        while let Some(current) = queue.pop_front().filter(|&current| current != target) {
            for &index in &self.outgoing[current.0] {
                let next = self.edges[index].to;
                if !std::mem::replace(&mut seen[next.0], true) {
                    previous[next.0] = Some((current, index));
                    queue.push_back(next);
                }
            }
        }
        if !seen[target.0] {
            return None;
        }
        let mut path = std::iter::successors(previous[target.0], |&(from, _)| previous[from.0])
            .map(|(_, index)| &self.edges[index])
            .collect::<Vec<_>>();
        path.reverse();
        Some(path)
    }
}
