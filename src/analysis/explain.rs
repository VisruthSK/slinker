use crate::TargetEnvironment;
use crate::analysis::Graph;
use crate::analysis::LinkIr;
use crate::analysis::export::{
    GraphBlockerExport, GraphEdgeReasonExport, GraphNodeKindExport, GraphSourceExport,
    PackageIdentityExport, TargetIdentityExport, blockers, edge_reason, node_kind, root_identity,
    root_reason, semantic_node_ids, stable_source, target_identity,
};
use crate::ir::PackageRole;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;

pub const EXPLANATION_SCHEMA_VERSION: u32 = 2;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExplanationDag {
    pub schema_version: u32,
    pub root_package: PackageIdentityExport,
    pub target: TargetIdentityExport,
    pub components: Vec<ExplanationComponent>,
    pub edges: Vec<ExplanationEdge>,
    pub projected_edges: Vec<ProjectedExplanationEdge>,
    pub roots: Vec<ExplanationRoot>,
    pub packages: Vec<ExplanationPackage>,
    pub diagnostics: Vec<GraphBlockerExport>,
    pub stats: ExplanationStats,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExplanationComponent {
    pub id: String,
    pub members: Vec<ExplanationMember>,
    pub cyclic: bool,
    pub class: PresentationClass,
    pub visibility: PresentationVisibility,
    pub package: Option<PackageIdentityExport>,
    pub root_causes: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub internal_evidence: Vec<ExplanationEvidence>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ExplanationMember {
    pub id: String,
    pub kind: GraphNodeKindExport,
    pub package: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PresentationClass {
    Binding,
    Package,
    Lifecycle,
    S3Registration,
    Native,
    Resource,
    Dataset,
    Closure,
    PrivateObject,
    ExternalBinding,
    Diagnostic,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PresentationVisibility {
    Primary,
    Context,
    Transparent,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExplanationEdge {
    pub id: String,
    pub from: String,
    pub to: String,
    pub reasons: Vec<GraphEdgeReasonExport>,
    pub occurrences: usize,
    pub evidence: Vec<ExplanationEvidence>,
    pub crosses_package_boundary: bool,
    pub source_packages: Vec<String>,
    pub target_packages: Vec<String>,
    pub reachability_redundant: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ExplanationEvidence {
    pub from_member: String,
    pub to_member: String,
    pub reason: GraphEdgeReasonExport,
    pub detail: String,
    pub source: Option<GraphSourceExport>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ProjectedExplanationEdge {
    pub from: String,
    pub to: String,
    pub via: Vec<ProjectedComponent>,
    pub edge_path: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ProjectedComponent {
    pub component: String,
    pub class: PresentationClass,
    pub members: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExplanationRoot {
    pub id: String,
    pub component: String,
    pub reasons: Vec<GraphEdgeReasonExport>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExplanationPackage {
    pub name: String,
    pub version: Option<String>,
    pub image_fingerprint: Option<String>,
    pub external: bool,
    pub retained_components: usize,
    pub retained_bindings: usize,
    pub root_causes: Vec<String>,
    pub entry_bindings: Vec<String>,
    pub boundary_edges: Vec<String>,
    pub boundary_occurrences: Vec<ExplanationBoundaryOccurrence>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ExplanationBoundaryOccurrence {
    pub edge_id: String,
    pub from_member: String,
    pub to_member: String,
    pub reason: GraphEdgeReasonExport,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExplanationStats {
    pub raw_nodes: usize,
    pub raw_edges: usize,
    pub components: usize,
    pub explanation_edges: usize,
    pub nontrivial_sccs: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExplanationError(String);

impl fmt::Display for ExplanationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "internal error: {}", self.0)
    }
}

impl std::error::Error for ExplanationError {}

impl ExplanationDag {
    pub fn from_plan(
        plan: &LinkIr,
        target: &TargetEnvironment,
        root_name: &str,
    ) -> Result<Self, ExplanationError> {
        let root_package = root_identity(plan, root_name).map_err(ExplanationError)?;
        let graph = plan.provenance().graph();
        let node_ids = semantic_node_ids(&graph).map_err(ExplanationError)?;
        let adjacency = adjacency(&graph);
        let (component_of, component_members) = strongly_connected_components(&adjacency);
        let component_ids = component_members
            .iter()
            .map(|members| component_id(members, &node_ids))
            .collect::<Vec<_>>();
        let identities = package_identities(plan)?;

        let mut components = component_members
            .iter()
            .enumerate()
            .map(|(component, members)| {
                let mut exported_members = members
                    .iter()
                    .map(|&member| ExplanationMember {
                        id: node_ids[member].clone(),
                        kind: node_kind(&graph.nodes[member].kind),
                        package: graph.nodes[member].package.clone(),
                    })
                    .collect::<Vec<_>>();
                exported_members.sort();
                let packages = exported_members
                    .iter()
                    .map(|member| member.package.clone())
                    .collect::<BTreeSet<_>>();
                let package = match packages.iter().collect::<Vec<_>>().as_slice() {
                    [name] => identities.get(*name).cloned(),
                    _ => None,
                };
                let (class, visibility) = presentation(&exported_members);
                let cyclic = members.len() > 1
                    || graph
                        .edges
                        .iter()
                        .any(|edge| edge.from.0 == members[0] && edge.to.0 == members[0]);
                ExplanationComponent {
                    id: component_ids[component].clone(),
                    members: exported_members,
                    cyclic,
                    class,
                    visibility,
                    package,
                    root_causes: Vec::new(),
                    internal_evidence: Vec::new(),
                }
            })
            .collect::<Vec<_>>();

        let mut grouped = BTreeMap::<(usize, usize), Vec<ExplanationEvidence>>::new();
        for edge in &graph.edges {
            let from_component = component_of[edge.from.0];
            let to_component = component_of[edge.to.0];
            let evidence = ExplanationEvidence {
                from_member: node_ids[edge.from.0].clone(),
                to_member: node_ids[edge.to.0].clone(),
                reason: edge_reason(
                    edge,
                    &graph.nodes[edge.from.0].kind,
                    &graph.nodes[edge.to.0].kind,
                ),
                detail: edge.reason.clone(),
                source: stable_source(plan.sources(), edge.span.as_ref()),
            };
            if from_component == to_component {
                components[from_component].internal_evidence.push(evidence);
            } else {
                grouped
                    .entry((from_component, to_component))
                    .or_default()
                    .push(evidence);
            }
        }
        for component in &mut components {
            component.internal_evidence.sort();
        }

        let mut edges = grouped
            .into_iter()
            .map(|((from, to), mut evidence)| {
                evidence.sort();
                let reasons = evidence
                    .iter()
                    .map(|item| item.reason)
                    .collect::<BTreeSet<_>>()
                    .into_iter()
                    .collect();
                let source_packages = component_packages(&components[from]);
                let target_packages = component_packages(&components[to]);
                let crosses_package_boundary = source_packages
                    .iter()
                    .any(|package| !target_packages.contains(package));
                ExplanationEdge {
                    id: edge_id(&component_ids[from], &component_ids[to]),
                    from: component_ids[from].clone(),
                    to: component_ids[to].clone(),
                    reasons,
                    occurrences: evidence.len(),
                    evidence,
                    crosses_package_boundary,
                    source_packages,
                    target_packages,
                    reachability_redundant: false,
                }
            })
            .collect::<Vec<_>>();
        edges.sort_by(|left, right| left.id.cmp(&right.id));

        let component_adjacency = component_adjacency(components.len(), &edges, &component_ids);
        mark_redundant_edges(&mut edges, &component_adjacency, &component_ids);
        let projected_edges =
            project_transparent_paths(&components, &edges, &component_adjacency, &component_ids);
        let roots = explanation_roots(
            &graph,
            plan.provenance().roots(),
            &component_of,
            &component_ids,
            &node_ids,
        )?;
        attribute_roots(
            &mut components,
            &component_adjacency,
            &roots,
            &component_ids,
        );
        let packages = package_summaries(plan, root_name, &components, &edges, &identities);

        components.sort_by(|left, right| left.id.cmp(&right.id));
        let stats = ExplanationStats {
            raw_nodes: graph.nodes.len(),
            raw_edges: graph.edges.len(),
            components: components.len(),
            explanation_edges: edges.len(),
            nontrivial_sccs: components
                .iter()
                .filter(|component| component.cyclic)
                .count(),
        };
        Ok(Self {
            schema_version: EXPLANATION_SCHEMA_VERSION,
            root_package,
            target: target_identity(target),
            components,
            edges,
            projected_edges,
            roots,
            packages,
            diagnostics: blockers(plan, &node_ids),
            stats,
        })
    }
}

fn adjacency(graph: &Graph) -> Vec<Vec<usize>> {
    let mut adjacency = vec![Vec::new(); graph.nodes.len()];
    for edge in &graph.edges {
        adjacency[edge.from.0].push(edge.to.0);
    }
    for targets in &mut adjacency {
        targets.sort_unstable();
        targets.dedup();
    }
    adjacency
}

fn strongly_connected_components(adjacency: &[Vec<usize>]) -> (Vec<usize>, Vec<Vec<usize>>) {
    fn visit(node: usize, adjacency: &[Vec<usize>], seen: &mut [bool], order: &mut Vec<usize>) {
        if seen[node] {
            return;
        }
        seen[node] = true;
        for &next in &adjacency[node] {
            visit(next, adjacency, seen, order);
        }
        order.push(node);
    }
    fn assign(
        node: usize,
        reverse: &[Vec<usize>],
        component: usize,
        component_of: &mut [usize],
        members: &mut Vec<usize>,
    ) {
        if component_of[node] != usize::MAX {
            return;
        }
        component_of[node] = component;
        members.push(node);
        for &next in &reverse[node] {
            assign(next, reverse, component, component_of, members);
        }
    }

    let mut reverse = vec![Vec::new(); adjacency.len()];
    for (from, targets) in adjacency.iter().enumerate() {
        for &to in targets {
            reverse[to].push(from);
        }
    }
    let mut seen = vec![false; adjacency.len()];
    let mut order = Vec::with_capacity(adjacency.len());
    for node in 0..adjacency.len() {
        visit(node, adjacency, &mut seen, &mut order);
    }
    let mut component_of = vec![usize::MAX; adjacency.len()];
    let mut components = Vec::new();
    for node in order.into_iter().rev() {
        if component_of[node] != usize::MAX {
            continue;
        }
        let component = components.len();
        let mut members = Vec::new();
        assign(node, &reverse, component, &mut component_of, &mut members);
        members.sort_unstable();
        components.push(members);
    }
    (component_of, components)
}

fn component_id(members: &[usize], node_ids: &[String]) -> String {
    let mut ids = members
        .iter()
        .map(|&member| node_ids[member].as_str())
        .collect::<Vec<_>>();
    ids.sort_unstable();
    let mut digest = Sha256::new();
    for id in ids {
        digest.update(id.len().to_le_bytes());
        digest.update(id.as_bytes());
    }
    format!("component:{}", hex::encode(digest.finalize()))
}

fn edge_id(from: &str, to: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(from.as_bytes());
    digest.update([0]);
    digest.update(to.as_bytes());
    format!("edge:{}", hex::encode(digest.finalize()))
}

fn presentation(members: &[ExplanationMember]) -> (PresentationClass, PresentationVisibility) {
    let kind = members
        .iter()
        .map(|member| member.kind)
        .min_by_key(|kind| presentation_rank(*kind))
        .unwrap_or(GraphNodeKindExport::Rejection);
    match kind {
        GraphNodeKindExport::RBinding => {
            (PresentationClass::Binding, PresentationVisibility::Primary)
        }
        GraphNodeKindExport::Package => {
            (PresentationClass::Package, PresentationVisibility::Primary)
        }
        GraphNodeKindExport::TargetBinding => (
            PresentationClass::ExternalBinding,
            PresentationVisibility::Primary,
        ),
        GraphNodeKindExport::Lifecycle => (
            PresentationClass::Lifecycle,
            PresentationVisibility::Context,
        ),
        GraphNodeKindExport::S3Registration => (
            PresentationClass::S3Registration,
            PresentationVisibility::Context,
        ),
        GraphNodeKindExport::NativeLibrary => {
            (PresentationClass::Native, PresentationVisibility::Context)
        }
        GraphNodeKindExport::Resource => {
            (PresentationClass::Resource, PresentationVisibility::Context)
        }
        GraphNodeKindExport::Dataset => {
            (PresentationClass::Dataset, PresentationVisibility::Context)
        }
        GraphNodeKindExport::Closure => (
            PresentationClass::Closure,
            PresentationVisibility::Transparent,
        ),
        GraphNodeKindExport::PrivateBinding => (
            PresentationClass::PrivateObject,
            PresentationVisibility::Transparent,
        ),
        GraphNodeKindExport::MissingPackage | GraphNodeKindExport::Rejection => (
            PresentationClass::Diagnostic,
            PresentationVisibility::Context,
        ),
        GraphNodeKindExport::PackageMetadata => {
            (PresentationClass::Other, PresentationVisibility::Context)
        }
    }
}

fn presentation_rank(kind: GraphNodeKindExport) -> u8 {
    match kind {
        GraphNodeKindExport::RBinding
        | GraphNodeKindExport::Package
        | GraphNodeKindExport::TargetBinding => 0,
        GraphNodeKindExport::Lifecycle
        | GraphNodeKindExport::S3Registration
        | GraphNodeKindExport::NativeLibrary
        | GraphNodeKindExport::Resource
        | GraphNodeKindExport::Dataset
        | GraphNodeKindExport::MissingPackage
        | GraphNodeKindExport::Rejection
        | GraphNodeKindExport::PackageMetadata => 1,
        GraphNodeKindExport::Closure | GraphNodeKindExport::PrivateBinding => 2,
    }
}

fn package_identities(
    plan: &LinkIr,
) -> Result<BTreeMap<String, PackageIdentityExport>, ExplanationError> {
    let mut identities = BTreeMap::new();
    for (_, package) in plan.program().packages() {
        let package = package.identity();
        let identity = PackageIdentityExport {
            name: package.name.clone(),
            version: package.version.to_string(),
            image_fingerprint: package.image_fingerprint.0.clone(),
        };
        if let Some(existing) = identities.get(&package.name)
            && existing != &identity
        {
            return Err(ExplanationError(format!(
                "conflicting installed identities for package `{}`",
                package.name
            )));
        }
        identities.insert(package.name.clone(), identity);
    }
    Ok(identities)
}

fn component_packages(component: &ExplanationComponent) -> Vec<String> {
    component
        .members
        .iter()
        .map(|member| member.package.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn component_adjacency(count: usize, edges: &[ExplanationEdge], ids: &[String]) -> Vec<Vec<usize>> {
    let by_id = ids
        .iter()
        .enumerate()
        .map(|(index, id)| (id.as_str(), index))
        .collect::<BTreeMap<_, _>>();
    let mut adjacency = vec![Vec::new(); count];
    for edge in edges {
        adjacency[by_id[edge.from.as_str()]].push(by_id[edge.to.as_str()]);
    }
    for targets in &mut adjacency {
        targets.sort_unstable();
        targets.dedup();
    }
    adjacency
}

fn explanation_roots(
    graph: &Graph,
    root_nodes: &[crate::analysis::NodeId],
    component_of: &[usize],
    component_ids: &[String],
    node_ids: &[String],
) -> Result<Vec<ExplanationRoot>, ExplanationError> {
    let mut roots = Vec::new();
    for root in root_nodes {
        let node = graph
            .nodes
            .get(root.0)
            .ok_or_else(|| ExplanationError(format!("missing root node {}", root.0)))?;
        let reason = root_reason(&node.kind)
            .ok_or_else(|| ExplanationError(format!("unsupported root {}", node_ids[root.0])))?;
        roots.push(ExplanationRoot {
            id: node_ids[root.0].clone(),
            component: component_ids[component_of[root.0]].clone(),
            reasons: vec![reason],
        });
    }
    roots.sort_by(|left, right| left.id.cmp(&right.id));
    roots.dedup_by(|left, right| left.id == right.id);
    Ok(roots)
}

fn attribute_roots(
    components: &mut [ExplanationComponent],
    adjacency: &[Vec<usize>],
    roots: &[ExplanationRoot],
    ids: &[String],
) {
    let by_id = ids
        .iter()
        .enumerate()
        .map(|(index, id)| (id.as_str(), index))
        .collect::<BTreeMap<_, _>>();
    for root in roots {
        let start = by_id[root.component.as_str()];
        let mut seen = vec![false; components.len()];
        let mut queue = VecDeque::from([start]);
        while let Some(component) = queue.pop_front() {
            if seen[component] {
                continue;
            }
            seen[component] = true;
            components[component].root_causes.push(root.id.clone());
            queue.extend(adjacency[component].iter().copied());
        }
    }
    for component in components {
        component.root_causes.sort();
        component.root_causes.dedup();
    }
}

fn mark_redundant_edges(edges: &mut [ExplanationEdge], adjacency: &[Vec<usize>], ids: &[String]) {
    let by_id = ids
        .iter()
        .enumerate()
        .map(|(index, id)| (id.as_str(), index))
        .collect::<BTreeMap<_, _>>();
    for edge in edges {
        let from = by_id[edge.from.as_str()];
        let to = by_id[edge.to.as_str()];
        edge.reachability_redundant = adjacency[from]
            .iter()
            .copied()
            .filter(|next| *next != to)
            .any(|next| reaches(next, to, adjacency));
    }
}

fn project_transparent_paths(
    components: &[ExplanationComponent],
    edges: &[ExplanationEdge],
    adjacency: &[Vec<usize>],
    ids: &[String],
) -> Vec<ProjectedExplanationEdge> {
    let edge_ids = edges
        .iter()
        .map(|edge| ((edge.from.as_str(), edge.to.as_str()), edge.id.as_str()))
        .collect::<BTreeMap<_, _>>();
    let mut projected = Vec::new();
    for source in 0..components.len() {
        if components[source].visibility == PresentationVisibility::Transparent {
            continue;
        }
        let mut neighbors = adjacency[source].clone();
        neighbors.sort_by(|left, right| ids[*left].cmp(&ids[*right]));
        let mut queue = VecDeque::new();
        let mut seen = vec![false; components.len()];
        for next in neighbors {
            if components[next].visibility != PresentationVisibility::Transparent {
                continue;
            }
            seen[next] = true;
            queue.push_back((
                next,
                vec![next],
                vec![edge_ids[&(ids[source].as_str(), ids[next].as_str())].to_owned()],
            ));
        }
        while let Some((current, via, edge_path)) = queue.pop_front() {
            let mut targets = adjacency[current].clone();
            targets.sort_by(|left, right| ids[*left].cmp(&ids[*right]));
            for next in targets {
                let mut next_edges = edge_path.clone();
                next_edges.push(edge_ids[&(ids[current].as_str(), ids[next].as_str())].to_owned());
                if components[next].visibility == PresentationVisibility::Transparent {
                    if !seen[next] {
                        seen[next] = true;
                        let mut next_via = via.clone();
                        next_via.push(next);
                        queue.push_back((next, next_via, next_edges));
                    }
                    continue;
                }
                projected.push(ProjectedExplanationEdge {
                    from: ids[source].clone(),
                    to: ids[next].clone(),
                    via: via
                        .iter()
                        .map(|&component| ProjectedComponent {
                            component: ids[component].clone(),
                            class: components[component].class,
                            members: components[component]
                                .members
                                .iter()
                                .map(|member| member.id.clone())
                                .collect(),
                        })
                        .collect(),
                    edge_path: next_edges,
                });
            }
        }
    }
    projected.sort();
    projected.dedup();
    projected
}

fn reaches(start: usize, target: usize, adjacency: &[Vec<usize>]) -> bool {
    let mut seen = vec![false; adjacency.len()];
    let mut queue = VecDeque::from([start]);
    while let Some(node) = queue.pop_front() {
        if node == target {
            return true;
        }
        if seen[node] {
            continue;
        }
        seen[node] = true;
        queue.extend(adjacency[node].iter().copied());
    }
    false
}

fn package_summaries(
    plan: &LinkIr,
    root_name: &str,
    components: &[ExplanationComponent],
    edges: &[ExplanationEdge],
    identities: &BTreeMap<String, PackageIdentityExport>,
) -> Vec<ExplanationPackage> {
    let members = components
        .iter()
        .flat_map(|component| component.members.iter())
        .map(|member| (member.id.as_str(), member))
        .collect::<BTreeMap<_, _>>();
    let target_names = plan
        .program()
        .packages()
        .filter(|(_, package)| package.role() == PackageRole::External)
        .map(|(_, package)| package.identity().name.as_str())
        .collect::<BTreeSet<_>>();
    let names = components
        .iter()
        .flat_map(component_packages)
        .collect::<BTreeSet<_>>();
    let mut packages = names
        .into_iter()
        .map(|name| {
            let component_indexes = components
                .iter()
                .enumerate()
                .filter(|(_, component)| {
                    component
                        .members
                        .iter()
                        .any(|member| member.package == name)
                })
                .map(|(index, _)| index)
                .collect::<BTreeSet<_>>();
            let mut boundary_edges = Vec::new();
            let mut entry_bindings = BTreeSet::new();
            let mut boundary_occurrences = Vec::new();
            for edge in edges {
                for evidence in &edge.evidence {
                    let Some(from) = members.get(evidence.from_member.as_str()) else {
                        continue;
                    };
                    let Some(to) = members.get(evidence.to_member.as_str()) else {
                        continue;
                    };
                    if from.package == name || to.package != name {
                        continue;
                    }
                    boundary_edges.push(edge.id.clone());
                    if matches!(
                        to.kind,
                        GraphNodeKindExport::RBinding | GraphNodeKindExport::TargetBinding
                    ) {
                        entry_bindings.insert(to.id.clone());
                    }
                    boundary_occurrences.push(ExplanationBoundaryOccurrence {
                        edge_id: edge.id.clone(),
                        from_member: evidence.from_member.clone(),
                        to_member: evidence.to_member.clone(),
                        reason: evidence.reason,
                    });
                }
            }
            boundary_edges.sort();
            boundary_edges.dedup();
            boundary_occurrences.sort();
            boundary_occurrences.dedup();
            let root_causes = component_indexes
                .iter()
                .flat_map(|&index| components[index].root_causes.iter().cloned())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect();
            ExplanationPackage {
                version: identities
                    .get(&name)
                    .map(|identity| identity.version.clone()),
                image_fingerprint: identities
                    .get(&name)
                    .map(|identity| identity.image_fingerprint.clone()),
                external: target_names.contains(name.as_str()),
                retained_components: component_indexes.len(),
                retained_bindings: component_indexes
                    .iter()
                    .flat_map(|&index| &components[index].members)
                    .filter(|member| {
                        matches!(
                            member.kind,
                            GraphNodeKindExport::RBinding | GraphNodeKindExport::TargetBinding
                        )
                    })
                    .count(),
                root_causes,
                entry_bindings: entry_bindings.into_iter().collect(),
                boundary_edges,
                boundary_occurrences,
                name,
            }
        })
        .collect::<Vec<_>>();
    packages.sort_by(|left, right| {
        (left.name != root_name)
            .cmp(&(right.name != root_name))
            .then_with(|| left.name.cmp(&right.name))
    });
    packages
}
