use crate::TargetEnvironment;
use crate::analysis::diagnostic::RejectCode;
use crate::analysis::engine::LinkPlan;
use crate::analysis::graph::{Edge, EdgeKind, Node, NodeKind};
use crate::syntax::{SourceOrigin, Sources, Span};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

pub const GRAPH_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GraphExport {
    pub schema_version: u32,
    pub package: PackageIdentityExport,
    pub target: TargetIdentityExport,
    pub nodes: Vec<GraphNodeExport>,
    pub edges: Vec<GraphEdgeExport>,
    pub roots: Vec<String>,
    pub root_reasons: Vec<GraphRootReasonExport>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub blockers: Vec<GraphBlockerExport>,
    pub stats: GraphStatsExport,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackageIdentityExport {
    pub name: String,
    pub version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TargetIdentityExport {
    pub r_version: String,
    pub platform: String,
    pub arch: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GraphNodeExport {
    pub id: String,
    pub kind: GraphNodeKindExport,
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
    Rejection,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct GraphEdgeExport {
    pub from: String,
    pub to: String,
    pub reasons: Vec<GraphEdgeReasonExport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<GraphSourceExport>,
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
pub struct GraphRootReasonExport {
    pub id: String,
    pub reasons: Vec<GraphEdgeReasonExport>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct GraphBlockerExport {
    pub kind: String,
    pub owner: String,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct GraphStatsExport {
    pub nodes: usize,
    pub edges: usize,
    pub roots: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphExportError(String);

impl GraphExportError {
    fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for GraphExportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "internal error: {}", self.0)
    }
}

impl std::error::Error for GraphExportError {}

impl GraphExport {
    pub fn from_plan(
        plan: &LinkPlan,
        target: &TargetEnvironment,
        root_name: &str,
    ) -> Result<Self, GraphExportError> {
        let mut root_packages = plan
            .images
            .keys()
            .filter(|id| id.name == root_name)
            .collect::<Vec<_>>();
        root_packages.sort_by(|left, right| {
            left.version
                .to_string()
                .cmp(&right.version.to_string())
                .then_with(|| left.image_fingerprint.0.cmp(&right.image_fingerprint.0))
        });
        let root_package = match root_packages.as_slice() {
            [root] => *root,
            [] => {
                return Err(GraphExportError::new(format!(
                    "root package `{root_name}` is missing from the analyzed images"
                )));
            }
            _ => {
                return Err(GraphExportError::new(format!(
                    "multiple analyzed images claim root package `{root_name}`"
                )));
            }
        };

        let mut node_ids = Vec::with_capacity(plan.graph.nodes.len());
        let mut seen_ids = BTreeMap::<String, usize>::new();
        let mut nodes = Vec::with_capacity(plan.graph.nodes.len());
        for (index, node) in plan.graph.nodes.iter().enumerate() {
            if node.id.0 != index {
                return Err(GraphExportError::new(format!(
                    "graph node index {index} carries inconsistent internal id {}",
                    node.id.0
                )));
            }
            let id = semantic_node_id(node);
            if let Some(previous) = seen_ids.insert(id.clone(), index) {
                return Err(GraphExportError::new(format!(
                    "semantic node id `{id}` is ambiguous between internal nodes {previous} and {index}"
                )));
            }
            node_ids.push(id.clone());
            nodes.push(GraphNodeExport {
                id,
                kind: node_kind(&node.kind),
            });
        }
        nodes.sort_by(|left, right| {
            left.id
                .cmp(&right.id)
                .then_with(|| left.kind.cmp(&right.kind))
        });

        // Preserve one exported edge for every internal edge. The internal graph
        // deliberately distinguishes provenance by detail/span, so coalescing
        // source/target pairs here would make diagnostic edge counts drift.
        let mut edges = Vec::with_capacity(plan.graph.edges.len());
        for edge in &plan.graph.edges {
            let from = node_ids.get(edge.from.0).cloned().ok_or_else(|| {
                GraphExportError::new(format!(
                    "edge references missing source node {}",
                    edge.from.0
                ))
            })?;
            let from_node = plan.graph.nodes.get(edge.from.0).ok_or_else(|| {
                GraphExportError::new(format!(
                    "edge references missing source node {}",
                    edge.from.0
                ))
            })?;
            let to_node = plan.graph.nodes.get(edge.to.0).ok_or_else(|| {
                GraphExportError::new(format!("edge references missing target node {}", edge.to.0))
            })?;
            let to = node_ids[edge.to.0].clone();
            edges.push(GraphEdgeExport {
                from,
                to,
                reasons: vec![edge_reason(edge, &from_node.kind, &to_node.kind)],
                detail: (!edge.reason.is_empty()).then(|| edge.reason.clone()),
                source: stable_source(&plan.sources, edge.span.as_ref()),
            });
        }
        edges.sort();

        // Roots are metadata in the linker today. Keep them out of `nodes` and
        // `edges` so inspection cannot change graph counts; root_reasons makes
        // their semantic cause explicit without synthesizing graph structure.
        let mut root_indices = BTreeSet::new();
        let mut roots = Vec::with_capacity(plan.roots.len());
        let mut root_reasons = Vec::with_capacity(plan.roots.len());
        for root in &plan.roots {
            if !root_indices.insert(root.0) {
                continue;
            }
            let node = plan.graph.nodes.get(root.0).ok_or_else(|| {
                GraphExportError::new(format!("root references missing node {}", root.0))
            })?;
            let id = node_ids[root.0].clone();
            let reason = root_reason(&node.kind).ok_or_else(|| {
                GraphExportError::new(format!(
                    "graph root `{id}` of kind `{}` has no diagnostic root representation",
                    node_kind(&node.kind).as_str()
                ))
            })?;
            roots.push(id.clone());
            root_reasons.push(GraphRootReasonExport {
                id,
                reasons: vec![reason],
            });
        }
        roots.sort();
        root_reasons.sort();

        let mut blockers = plan
            .diagnostics
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
                    kind: reject_code_name(diagnostic.code).to_owned(),
                    owner,
                    message: diagnostic.message.clone(),
                }
            })
            .collect::<Vec<_>>();
        blockers.sort();

        let stats = GraphStatsExport {
            nodes: nodes.len(),
            edges: edges.len(),
            roots: roots.len(),
        };

        Ok(Self {
            schema_version: GRAPH_SCHEMA_VERSION,
            package: PackageIdentityExport {
                name: root_package.name.clone(),
                version: root_package.version.to_string(),
            },
            target: TargetIdentityExport {
                r_version: target.target.r_version.clone(),
                platform: target.target.os.clone(),
                arch: target.target.arch.clone(),
            },
            nodes,
            edges,
            roots,
            root_reasons,
            blockers,
            stats,
        })
    }

    pub fn render_text(&self) -> String {
        let mut out = String::new();
        out.push_str("graph nodes\n\n");
        for node in &self.nodes {
            out.push_str(&node.id);
            out.push('\n');
            out.push_str("  kind: ");
            out.push_str(node.kind.as_str());
            out.push_str("\n\n");
        }

        out.push_str("graph edges\n\n");
        for edge in &self.edges {
            out.push_str(&edge.from);
            out.push('\n');
            out.push_str("  -> ");
            out.push_str(&edge.to);
            out.push('\n');
            if edge.reasons.len() == 1 {
                out.push_str("  reason: ");
                out.push_str(edge.reasons[0].as_str());
                out.push('\n');
            } else {
                out.push_str("  reasons: ");
                for (index, reason) in edge.reasons.iter().enumerate() {
                    if index > 0 {
                        out.push_str(", ");
                    }
                    out.push_str(reason.as_str());
                }
                out.push('\n');
            }
            if let Some(detail) = &edge.detail {
                out.push_str("  detail: ");
                out.push_str(&escape_text_field(detail));
                out.push('\n');
            }
            if let Some(source) = &edge.source {
                out.push_str("  source: ");
                out.push_str(&source.owner);
                out.push(':');
                out.push_str(&source.start.to_string());
                out.push_str("..");
                out.push_str(&source.end.to_string());
                out.push('\n');
            }
            out.push('\n');
        }

        out.push_str("graph roots\n\n");
        for root in &self.root_reasons {
            out.push_str(&root.id);
            out.push('\n');
            if root.reasons.len() == 1 {
                out.push_str("  reason: ");
                out.push_str(root.reasons[0].as_str());
                out.push('\n');
            } else {
                out.push_str("  reasons: ");
                for (index, reason) in root.reasons.iter().enumerate() {
                    if index > 0 {
                        out.push_str(", ");
                    }
                    out.push_str(reason.as_str());
                }
                out.push('\n');
            }
            out.push('\n');
        }
        out
    }
}

impl GraphNodeKindExport {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RBinding => "r_binding",
            Self::PrivateBinding => "private_binding",
            Self::Closure => "closure",
            Self::Package => "package",
            Self::Dataset => "dataset",
            Self::Lifecycle => "lifecycle",
            Self::S3Registration => "s3_registration",
            Self::Resource => "resource",
            Self::NativeLibrary => "native_library",
            Self::TargetBinding => "target_binding",
            Self::PackageMetadata => "package_metadata",
            Self::MissingPackage => "missing_package",
            Self::Rejection => "rejection",
        }
    }
}

impl GraphEdgeReasonExport {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ExportRoot => "export_root",
            Self::PackageRoot => "package_root",
            Self::OnLoadRoot => "on_load_root",
            Self::LexicalReference => "lexical_reference",
            Self::QualifiedReference => "qualified_reference",
            Self::ImportedBinding => "imported_binding",
            Self::NamespaceImport => "namespace_import",
            Self::ExportReference => "export_reference",
            Self::S3Registration => "s3_registration",
            Self::Lifecycle => "lifecycle",
            Self::ResourceReference => "resource_reference",
            Self::DatasetReference => "dataset_reference",
            Self::NativeCall => "native_call",
            Self::NativeCallback => "native_callback",
            Self::ClosureCapture => "closure_capture",
            Self::ClosureExecution => "closure_execution",
            Self::SpecializedDiscovery => "specialized_discovery",
            Self::SemanticEffect => "semantic_effect",
        }
    }
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
        NodeKind::Rejection { code } => format!("rejection:{}::{code}", node.package),
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
        NodeKind::Rejection { .. } => GraphNodeKindExport::Rejection,
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
    match &source.origin {
        SourceOrigin::InstalledBinding { package, binding } => Some(GraphSourceExport {
            owner: format!("{package}::{binding}"),
            start: span.start,
            end: span.end,
        }),
        SourceOrigin::File(_) => None,
    }
}

fn reject_code_name(code: RejectCode) -> &'static str {
    match code {
        RejectCode::AirUnsupportedSyntax => "air_unsupported_syntax",
        RejectCode::InvalidInstalledRepresentation => "invalid_installed_representation",
        RejectCode::ParserDisagreement => "parser_disagreement",
        RejectCode::ActiveBinding => "active_binding",
        RejectCode::ArbitraryEvaluation => "arbitrary_evaluation",
        RejectCode::DynamicLookup => "dynamic_lookup",
        RejectCode::DynamicPackageDiscovery => "dynamic_package_discovery",
        RejectCode::DependsAttachmentUnsupported => "depends_attachment_unsupported",
        RejectCode::EnvironmentMutation => "environment_mutation",
        RejectCode::LifecycleHook => "lifecycle_hook",
        RejectCode::MissingDependency => "missing_dependency",
        RejectCode::MissingResource => "missing_resource",
        RejectCode::ObjectSystem => "object_system",
        RejectCode::PackageAttachmentUnsupported => "package_attachment_unsupported",
        RejectCode::PotentialUnboundLocal => "potential_unbound_local",
        RejectCode::SemanticAmbiguity => "semantic_ambiguity",
        RejectCode::SyntaxObservation => "syntax_observation",
        RejectCode::UnknownClosureEnclosure => "unknown_closure_enclosure",
        RejectCode::UnknownNativeEffects => "unknown_native_effects",
        RejectCode::UnknownNativeLookup => "unknown_native_lookup",
        RejectCode::UnresolvedBinding => "unresolved_binding",
        RejectCode::UnsupportedObject => "unsupported_object",
        RejectCode::UnsupportedTopLevelEffect => "unsupported_top_level_effect",
    }
}

fn escape_text_field(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
}
