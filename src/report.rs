use std::fs;
use std::path::Path;

use serde::Serialize;

use crate::{
    Capability, Diagnostic, EdgeKind, Graph, LinkPlan, NodeKind, PackageRole, PackageSet,
    Rejection, RetentionStep,
};

pub const REPORT_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Serialize)]
pub struct LinkReport {
    pub schema_version: u32,
    pub packages: Vec<PackageReport>,
    pub roots: Vec<u32>,
    pub nodes: Vec<NodeReport>,
    pub edges: Vec<EdgeReport>,
    pub diagnostics: Vec<DiagnosticReport>,
}

#[derive(Debug, Serialize)]
pub struct PackageReport {
    pub id: u32,
    pub name: String,
    pub version: String,
    pub role: &'static str,
    pub retained_nodes: usize,
    pub total_nodes: usize,
}

#[derive(Debug, Serialize)]
pub struct NodeReport {
    pub id: u32,
    pub package: u32,
    #[serde(flatten)]
    pub kind: NodeKindReport,
    pub retained: bool,
    pub capabilities: Vec<&'static str>,
    pub transformed_syntax: bool,
    pub dynamic_package: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum NodeKindReport {
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

#[derive(Debug, Serialize)]
pub struct EdgeReport {
    pub from: u32,
    pub to: u32,
    pub kind: &'static str,
    pub reason: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct DiagnosticReport {
    pub code: &'static str,
    pub message: String,
    pub path: Vec<RetentionStepReport>,
}

#[derive(Debug, Serialize)]
pub struct RetentionStepReport {
    pub node: u32,
    pub via: Option<&'static str>,
    pub reason: Option<String>,
}

impl LinkReport {
    pub fn new(
        graph: &Graph,
        packages: &PackageSet,
        plan: &LinkPlan,
        diagnostics: &[Diagnostic],
    ) -> Self {
        let packages = packages
            .iter()
            .map(|package| {
                let node_ids: Vec<_> = graph.package_nodes(package.id).collect();
                PackageReport {
                    id: package.id.0,
                    name: package.name.clone(),
                    version: package.version.clone(),
                    role: package_role_name(package.role),
                    retained_nodes: node_ids.iter().filter(|id| plan.retains(**id)).count(),
                    total_nodes: node_ids.len(),
                }
            })
            .collect();

        let nodes = graph
            .nodes()
            .map(|node| NodeReport {
                id: node.id.0,
                package: node.package.0,
                kind: node_kind_report(&node.kind),
                retained: plan.retains(node.id),
                capabilities: node.capabilities.iter().copied().map(capability_name).collect(),
                transformed_syntax: node.transformed_syntax,
                dynamic_package: node.dynamic_package.clone(),
            })
            .collect();

        let edges = graph
            .all_edges()
            .map(|(from, edge)| EdgeReport {
                from: from.0,
                to: edge.to.0,
                kind: edge_kind_name(edge.kind),
                reason: edge.reason.clone(),
            })
            .collect();

        let diagnostics = diagnostics
            .iter()
            .map(|diagnostic| DiagnosticReport {
                code: diagnostic.rejection.code().as_str(),
                message: rejection_message(&diagnostic.rejection),
                path: diagnostic
                    .detailed_path
                    .iter()
                    .map(retention_step_report)
                    .collect(),
            })
            .collect();

        Self {
            schema_version: REPORT_SCHEMA_VERSION,
            packages,
            roots: graph.roots().map(|id| id.0).collect(),
            nodes,
            edges,
            diagnostics,
        }
    }

    pub fn to_json_pretty(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }

    pub fn write_json(&self, path: impl AsRef<Path>) -> Result<(), ReportWriteError> {
        let bytes = serde_json::to_vec_pretty(self).map_err(ReportWriteError::Json)?;
        fs::write(path, bytes).map_err(ReportWriteError::Io)
    }
}

#[derive(Debug)]
pub enum ReportWriteError {
    Io(std::io::Error),
    Json(serde_json::Error),
}

impl std::fmt::Display for ReportWriteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(f, "failed to write analysis report: {error}"),
            Self::Json(error) => write!(f, "failed to encode analysis report: {error}"),
        }
    }
}

impl std::error::Error for ReportWriteError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Json(error) => Some(error),
        }
    }
}

fn node_kind_report(kind: &NodeKind) -> NodeKindReport {
    match kind {
        NodeKind::Binding { name } => NodeKindReport::Binding { name: name.clone() },
        NodeKind::NamespaceActivation => NodeKindReport::NamespaceActivation,
        NodeKind::Initialization { ordinal } => NodeKindReport::Initialization { ordinal: *ordinal },
        NodeKind::LifecycleHook { name } => NodeKindReport::LifecycleHook { name: name.clone() },
        NodeKind::S3Registration { generic, method } => NodeKindReport::S3Registration {
            generic: generic.clone(),
            method: method.clone(),
        },
        NodeKind::Resource { path } => NodeKindReport::Resource { path: path.clone() },
        NodeKind::SerializedObject { name } => {
            NodeKindReport::SerializedObject { name: name.clone() }
        }
        NodeKind::NativeComponent { name } => NodeKindReport::NativeComponent { name: name.clone() },
        NodeKind::BuildInput { path } => NodeKindReport::BuildInput { path: path.clone() },
    }
}

fn retention_step_report(step: &RetentionStep) -> RetentionStepReport {
    RetentionStepReport {
        node: step.node.0,
        via: step.via.map(edge_kind_name),
        reason: step.reason.clone(),
    }
}

fn package_role_name(role: PackageRole) -> &'static str {
    match role {
        PackageRole::Root => "root",
        PackageRole::Internalized => "internalized",
        PackageRole::TargetProvided => "target_provided",
    }
}

fn edge_kind_name(kind: EdgeKind) -> &'static str {
    match kind {
        EdgeKind::LexicalUse => "lexical_use",
        EdgeKind::NamespaceAccess => "namespace_access",
        EdgeKind::Import => "import",
        EdgeKind::Activation => "activation",
        EdgeKind::Initialization => "initialization",
        EdgeKind::S3 => "s3",
        EdgeKind::Resource => "resource",
        EdgeKind::Serialization => "serialization",
        EdgeKind::Native => "native",
        EdgeKind::Build => "build",
        EdgeKind::TraceSpecialization => "trace_specialization",
    }
}

fn capability_name(capability: Capability) -> &'static str {
    match capability {
        Capability::Pure => "pure",
        Capability::SyntaxObservation => "syntax_observation",
        Capability::DynamicPackageLookup => "dynamic_package_lookup",
        Capability::FileSystem => "file_system",
        Capability::Network => "network",
        Capability::Rng => "rng",
        Capability::Time => "time",
        Capability::GlobalMutation => "global_mutation",
        Capability::AmbientPackageDiscovery => "ambient_package_discovery",
        Capability::ExternalPointer => "external_pointer",
        Capability::EnvironmentIdentity => "environment_identity",
        Capability::Promise => "promise",
        Capability::WeakReference => "weak_reference",
        Capability::Altrep => "altrep",
        Capability::S4 => "s4",
        Capability::S7 => "s7",
        Capability::R6 => "r6",
        Capability::UnknownCall => "unknown_call",
        Capability::NativeOpaque => "native_opaque",
        Capability::OnLoad => "on_load",
    }
}

fn rejection_message(rejection: &Rejection) -> String {
    match rejection {
        Rejection::NonProvidedDepends { package } => format!(
            "internalized package {package} has a non-target-provided Depends dependency"
        ),
        Rejection::UnsupportedCapability { node, capability } => {
            format!("{node} requires unsupported capability {capability:?}")
        }
        Rejection::SyntaxObservationAfterRewrite { node } => {
            format!("{node} can observe syntax that the linker rewrites")
        }
        Rejection::UnresolvedDynamicPackage { node } => {
            format!("{node} performs package discovery with a non-literal package")
        }
        Rejection::AmbientPackageLookup { node, package } => format!(
            "{node} discovers undeclared package {package} from the ambient library"
        ),
        Rejection::UnmodeledOnLoad { node } => {
            format!("{node} contains an .onLoad hook without an activation model")
        }
    }
}
