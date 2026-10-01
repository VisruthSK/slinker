use crate::TargetEnvironment;
use crate::analysis::LinkIr;
use crate::analysis::diagnostic::RejectCode;
use crate::analysis::graph::{Edge, EdgeKind, Graph, Node, NodeKind};
use crate::syntax::{Sources, Span};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackageIdentityExport {
    pub name: String,
    pub version: String,
    pub image_fingerprint: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TargetIdentityExport {
    pub r_version: String,
    pub platform: String,
    pub arch: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GraphNodeKindExport {
    RBinding,
    PrivateBinding,
    Closure,
    Package,
    Dataset,
    Lifecycle,
    S3Registration,
    Resource,
    NativeLibrary,
    TargetBinding,
    PackageMetadata,
    MissingPackage,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GraphEdgeReasonExport {
    ExportRoot,
    PackageRoot,
    OnLoadRoot,
    LexicalReference,
    QualifiedReference,
    ImportedBinding,
    NamespaceImport,
    ExportReference,
    S3Registration,
    Lifecycle,
    ResourceReference,
    DatasetReference,
    NativeCall,
    NativeCallback,
    ClosureCapture,
    ClosureExecution,
    SpecializedDiscovery,
    SemanticEffect,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct GraphSourceExport {
    pub owner: String,
    pub start: usize,
    pub end: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct GraphBlockerExport {
    pub kind: RejectCode,
    pub owner: String,
    pub message: String,
}

pub(crate) fn root_identity(
    plan: &LinkIr,
    root_name: &str,
) -> Result<PackageIdentityExport, String> {
    let root_package = plan
        .program()
        .package(plan.program().root_package())
        .identity();
    if root_package.name != root_name {
        return Err(format!(
            "requested root `{root_name}` disagrees with ProgramIr root `{}`",
            root_package.name
        ));
    }
    Ok(PackageIdentityExport {
        name: root_package.name.to_string(),
        version: root_package.version.to_string(),
        image_fingerprint: root_package.image_fingerprint.0.clone(),
    })
}

pub(crate) fn target_identity(target: &TargetEnvironment) -> TargetIdentityExport {
    TargetIdentityExport {
        r_version: target.target.r_version.clone(),
        platform: target.target.os.clone(),
        arch: target.target.arch.clone(),
    }
}

pub(crate) fn semantic_node_ids(graph: &Graph) -> Result<Vec<String>, String> {
    let mut node_ids = Vec::with_capacity(graph.nodes.len());
    let mut seen_ids = BTreeMap::<String, usize>::new();
    for (index, node) in graph.nodes.iter().enumerate() {
        if node.id.0 != index {
            return Err(format!(
                "graph node index {index} carries inconsistent internal id {}",
                node.id.0
            ));
        }
        let id = semantic_node_id(node);
        if let Some(previous) = seen_ids.insert(id.clone(), index) {
            return Err(format!(
                "semantic node id `{id}` is ambiguous between internal nodes {previous} and {index}"
            ));
        }
        node_ids.push(id);
    }
    Ok(node_ids)
}

pub(crate) fn blockers(plan: &LinkIr, node_ids: &[String]) -> Vec<GraphBlockerExport> {
    let mut blockers = plan
        .blockers()
        .iter()
        .filter(|diagnostic| diagnostic.code != RejectCode::MissingDependency)
        .map(|diagnostic| {
            let owner = diagnostic
                .node
                .and_then(|id| node_ids.get(id.0).cloned())
                .or_else(|| {
                    diagnostic
                        .binding
                        .as_ref()
                        .map(|binding| format!("{}::{binding}", diagnostic.package))
                })
                .unwrap_or_else(|| format!("package:{}", diagnostic.package));
            GraphBlockerExport {
                kind: diagnostic.code,
                owner,
                message: diagnostic.message.clone(),
            }
        })
        .collect::<Vec<_>>();
    blockers.sort();
    blockers
}

pub(crate) fn semantic_node_id(node: &Node) -> String {
    match &node.kind {
        NodeKind::Binding { name } | NodeKind::ExternalBinding { name } => {
            format!("{}::{name}", node.package)
        }
        NodeKind::PrivateBinding { environment, name } => {
            format!("private:{}::{environment}::{name}", node.package)
        }
        NodeKind::ClosureObject {
            owner,
            path,
            enclosure,
            derived,
        } => {
            format!(
                "closure:{}::{owner}{path}@{enclosure}:{}",
                node.package,
                if *derived { "derived" } else { "installed" }
            )
        }
        NodeKind::Activation => format!("package:{}", node.package),
        NodeKind::Dataset { name } => format!("dataset:{}::{name}", node.package),
        NodeKind::Lifecycle { hook } => format!("lifecycle:{}::{hook}", node.package),
        NodeKind::S3Registration { generic, class } => {
            format!("s3:{}::{generic}/{class}", node.package)
        }
        NodeKind::Resource { path } => format!("resource:{}::{path}", node.package),
        NodeKind::NativeComponent { name } => {
            format!("native-library:{}::{name}", node.package)
        }
        NodeKind::PackageMetadata { name } => format!("metadata:{}::{name}", node.package),
        NodeKind::MissingPackage => format!("missing-package:{}", node.package),
    }
}

pub(crate) fn node_kind(kind: &NodeKind) -> GraphNodeKindExport {
    match kind {
        NodeKind::Binding { .. } => GraphNodeKindExport::RBinding,
        NodeKind::PrivateBinding { .. } => GraphNodeKindExport::PrivateBinding,
        NodeKind::ClosureObject { .. } => GraphNodeKindExport::Closure,
        NodeKind::Activation => GraphNodeKindExport::Package,
        NodeKind::Dataset { .. } => GraphNodeKindExport::Dataset,
        NodeKind::Lifecycle { .. } => GraphNodeKindExport::Lifecycle,
        NodeKind::S3Registration { .. } => GraphNodeKindExport::S3Registration,
        NodeKind::Resource { .. } => GraphNodeKindExport::Resource,
        NodeKind::NativeComponent { .. } => GraphNodeKindExport::NativeLibrary,
        NodeKind::ExternalBinding { .. } => GraphNodeKindExport::TargetBinding,
        NodeKind::PackageMetadata { .. } => GraphNodeKindExport::PackageMetadata,
        NodeKind::MissingPackage => GraphNodeKindExport::MissingPackage,
    }
}

pub(crate) fn edge_reason(
    edge: &Edge,
    source: &NodeKind,
    target: &NodeKind,
) -> GraphEdgeReasonExport {
    match edge.kind {
        EdgeKind::Root => GraphEdgeReasonExport::ExportRoot,
        EdgeKind::Lexical => GraphEdgeReasonExport::LexicalReference,
        EdgeKind::Import if matches!(target, NodeKind::Activation) => {
            GraphEdgeReasonExport::NamespaceImport
        }
        EdgeKind::Import => GraphEdgeReasonExport::ImportedBinding,
        EdgeKind::PackageQualified => GraphEdgeReasonExport::QualifiedReference,
        EdgeKind::NamespaceLoad => GraphEdgeReasonExport::NamespaceImport,
        EdgeKind::Export => GraphEdgeReasonExport::ExportReference,
        EdgeKind::S3Registration => GraphEdgeReasonExport::S3Registration,
        EdgeKind::Lifecycle if matches!(source, NodeKind::Activation) => {
            GraphEdgeReasonExport::OnLoadRoot
        }
        EdgeKind::Lifecycle => GraphEdgeReasonExport::Lifecycle,
        EdgeKind::Resource => GraphEdgeReasonExport::ResourceReference,
        EdgeKind::Dataset => GraphEdgeReasonExport::DatasetReference,
        EdgeKind::Native => GraphEdgeReasonExport::NativeCall,
        EdgeKind::Callback => GraphEdgeReasonExport::NativeCallback,
        EdgeKind::ClosureCapture => GraphEdgeReasonExport::ClosureCapture,
        EdgeKind::ClosureExecution => GraphEdgeReasonExport::ClosureExecution,
        EdgeKind::Discovery => GraphEdgeReasonExport::SpecializedDiscovery,
        EdgeKind::Effect => GraphEdgeReasonExport::SemanticEffect,
    }
}

pub(crate) fn root_reason(kind: &NodeKind) -> Option<GraphEdgeReasonExport> {
    match kind {
        NodeKind::Binding { .. } => Some(GraphEdgeReasonExport::ExportRoot),
        NodeKind::Activation => Some(GraphEdgeReasonExport::PackageRoot),
        _ => None,
    }
}

pub(crate) fn stable_source(sources: &Sources, span: Option<&Span>) -> Option<GraphSourceExport> {
    let span = span?;
    let source = sources.get(&span.source)?;
    Some(GraphSourceExport {
        owner: source.origin.display(),
        start: span.start,
        end: span.end,
    })
}
