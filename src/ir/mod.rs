//! Immutable linked-program representation consumed by build preflight and materialization.

use crate::analysis::{Edge, Graph, Node, NodeId};
pub use crate::package::{PackageId, PackageIdentity, PackageRole};

use crate::package::Digest;
use std::collections::BTreeMap;
use std::sync::Arc;

macro_rules! id_type {
    ($name:ident) => {
        #[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
        pub struct $name(u32);

        impl $name {
            fn from_index(index: usize) -> Self {
                Self(u32::try_from(index).expect("linked IR table exceeds u32"))
            }

            fn index(self) -> usize {
                self.0 as usize
            }
        }
    };
}

id_type!(NamespaceId);
id_type!(BindingId);
id_type!(ValueId);
id_type!(ClosureId);
id_type!(EnvironmentId);
id_type!(CodeId);
id_type!(CodeOccurrenceId);
id_type!(S3RegistrationId);
id_type!(NativeComponentId);
id_type!(ResourceId);

/// Final package runtime contract.
#[derive(Clone, Debug)]
pub enum PackageIr {
    Root {
        build_identity: PackageIdentity,
    },
    Linked {
        build_identity: PackageIdentity,
    },
    External {
        analyzed_identity: PackageIdentity,
        contract: ExternalPackageContract,
    },
}

impl PackageIr {
    pub fn role(&self) -> PackageRole {
        match self {
            Self::Root { .. } => PackageRole::Root,
            Self::Linked { .. } => PackageRole::Linked,
            Self::External { .. } => PackageRole::External,
        }
    }

    pub fn identity(&self) -> &PackageIdentity {
        match self {
            Self::Root { build_identity } | Self::Linked { build_identity } => build_identity,
            Self::External {
                analyzed_identity, ..
            } => analyzed_identity,
        }
    }
}

/// DESCRIPTION-governed runtime requirement for an External package. Platform packages ship with
/// the selected R, so the target contract already satisfies them.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExternalPackageContract {
    pub package: String,
    pub platform: bool,
    pub requirements: Vec<crate::Relation>,
}

/// One stable namespace binding slot.
#[derive(Clone, Debug)]
pub struct Binding {
    pub name: String,
    pub state: LinkBindingState,
}

/// Value state before runtime activation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InitialBindingState {
    Unbound,
    Value(ValueId),
}

/// Physical realization of a final binding slot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LinkBindingState {
    Materialized {
        namespace: NamespaceId,
        initial: InitialBindingState,
    },
    External {
        namespace: NamespaceId,
        access: ExternalBindingAccess,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExternalBindingAccess {
    Exported,
    Internal,
}

/// Canonically ordered final export membership.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ExportTable {
    bindings: Vec<BindingId>,
}

impl ExportTable {
    pub fn new(mut bindings: Vec<BindingId>) -> Self {
        bindings.sort();
        bindings.dedup();
        Self { bindings }
    }

    pub fn bindings(&self) -> &[BindingId] {
        &self.bindings
    }
}

/// Final namespace runtime state.
#[derive(Clone, Debug)]
pub enum LinkNamespaceState {
    Root(MaterializedNamespaceState),
    Linked(MaterializedNamespaceState),
    External { package: PackageId },
}

#[derive(Clone, Debug)]
pub struct MaterializedNamespaceState {
    pub namespace_environment: EnvironmentId,
    pub imports_environment: EnvironmentId,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MaterializedRole {
    Root,
    Linked,
}

#[derive(Clone, Debug)]
pub struct Namespace {
    pub package: PackageId,
    pub bindings: BTreeMap<String, BindingId>,
    pub imports: Vec<ImportBindingIr>,
    pub state: LinkNamespaceState,
    pub s3_registrations: Vec<S3RegistrationId>,
    pub native_components: Vec<NativeComponentId>,
}

#[derive(Clone, Debug)]
pub struct ImportBindingIr {
    pub local: String,
    pub target: BindingId,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EnvironmentKind {
    Namespace(NamespaceId),
    Imports(NamespaceId),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EnvironmentParentIr {
    Materialized(EnvironmentId),
    BaseNamespace,
}

#[derive(Clone, Debug)]
pub struct Environment {
    pub kind: EnvironmentKind,
    pub parent: EnvironmentParentIr,
}

#[derive(Clone, Debug)]
pub struct Closure {
    pub code: CodeId,
    pub enclosure: EnvironmentId,
    pub payload: PayloadRef,
}

/// Supported persistent runtime value without an Unknown state.
#[derive(Clone, Debug)]
pub enum Value {
    Closure(ClosureId),
    Payload(PayloadRef),
}

/// Physical payload source selected only after linked identity is fixed.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct PayloadRef {
    pub package: PackageId,
    pub binding: String,
}

#[derive(Clone, Debug)]
pub struct CodeIr {
    source: Arc<str>,
    value_start: Option<usize>,
    occurrences: Vec<CodeOccurrence>,
    normalized_shape: Digest,
}

impl CodeIr {
    pub fn new(
        source: Arc<str>,
        occurrences: Vec<CodeOccurrence>,
        normalized_shape: Digest,
    ) -> Self {
        Self {
            value_start: crate::syntax::assigned_value_start(&source),
            source,
            occurrences,
            normalized_shape,
        }
    }

    pub fn source(&self) -> &str {
        &self.source
    }

    pub fn assigned_value_start(&self) -> Option<usize> {
        self.value_start
    }

    pub fn occurrences(&self) -> &[CodeOccurrence] {
        &self.occurrences
    }

    pub fn indexed_occurrences(&self) -> impl Iterator<Item = (CodeOccurrenceId, &CodeOccurrence)> {
        self.occurrences
            .iter()
            .enumerate()
            .map(|(index, occurrence)| (CodeOccurrenceId::from_index(index), occurrence))
    }

    pub fn occurrence(&self, id: CodeOccurrenceId) -> &CodeOccurrence {
        &self.occurrences[id.index()]
    }

    pub fn normalized_shape(&self) -> &Digest {
        &self.normalized_shape
    }
}

#[derive(Clone, Debug)]
pub struct CodeOccurrence {
    pub start: usize,
    pub end: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CodeSite {
    pub code: CodeId,
    pub occurrence: CodeOccurrenceId,
}

#[derive(Clone, Debug)]
pub enum Relocation {
    Binding {
        site: CodeSite,
        target: BindingId,
        access: ExternalBindingAccess,
    },
    Namespace {
        site: CodeSite,
        target: NamespaceId,
    },
    Package {
        site: CodeSite,
        target: Option<PackageId>,
        operation: PackageOperationIr,
    },
    Resource {
        site: CodeSite,
        target: ResourceId,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PackageOperationIr {
    RequireNamespace { result: bool },
    LoadNamespace,
    GetNamespace,
    AsNamespace,
    PackageVersion { version: String },
}

/// One Linked namespace activation, in the order the Root `.onLoad` wrapper performs them.
#[derive(Clone, Debug)]
pub struct NamespaceActivationIr {
    pub namespace: NamespaceId,
    pub on_load: Option<BindingId>,
    pub exports: ExportTable,
    pub unretained_exports: Vec<String>,
    pub stubs: Vec<String>,
}

/// Root source-package transformation decided at finalization: the generated `DESCRIPTION`, the
/// `NAMESPACE` exports, and the original Root `.onLoad` that the generated wrapper calls last.
#[derive(Clone, Debug, Default)]
pub struct RootArtifactIr {
    pub description: Arc<str>,
    pub exports: ExportTable,
    pub on_load: Option<ClosureId>,
}

#[derive(Clone, Debug)]
pub struct GenericId {
    pub package: Option<PackageId>,
    pub name: String,
}

#[derive(Clone, Debug)]
pub struct S3RegistrationIr {
    pub owner_namespace: NamespaceId,
    pub generic: GenericId,
    pub class: String,
    pub method: BindingId,
}

#[derive(Clone, Debug)]
pub struct NativeComponentIr {
    pub namespace: NamespaceId,
    pub native: crate::package::NativeComponent,
}

#[derive(Clone, Debug)]
pub struct ResourceIr {
    pub package: PackageId,
    pub path: String,
}

/// Selected target-R compatibility contract recorded in the artifact.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TargetContract {
    pub r_version: String,
    pub platform: String,
    pub arch: String,
}

/// Complete immutable semantic construction authority.
#[derive(Debug)]
pub struct ProgramIr {
    target: TargetContract,
    root_package: PackageId,
    packages: BTreeMap<PackageId, PackageIr>,
    namespaces: Vec<Namespace>,
    bindings: Vec<Binding>,
    values: Vec<Value>,
    closures: Vec<Closure>,
    environments: Vec<Environment>,
    codes: Vec<CodeIr>,
    activations: Vec<NamespaceActivationIr>,
    s3_registrations: Vec<S3RegistrationIr>,
    native_components: Vec<NativeComponentIr>,
    resources: Vec<ResourceIr>,
    relocations: Vec<Relocation>,
    root_artifact: RootArtifactIr,
}

impl ProgramIr {
    pub fn builder(target: TargetContract) -> ProgramBuilder {
        ProgramBuilder::new(target)
    }

    pub fn target(&self) -> &TargetContract {
        &self.target
    }

    pub fn root_package(&self) -> PackageId {
        self.root_package
    }

    pub fn packages(&self) -> impl Iterator<Item = (PackageId, &PackageIr)> {
        self.packages.iter().map(|(id, package)| (*id, package))
    }

    pub fn namespaces(&self) -> &[Namespace] {
        &self.namespaces
    }

    pub fn bindings(&self) -> &[Binding] {
        &self.bindings
    }

    pub fn values(&self) -> &[Value] {
        &self.values
    }

    pub fn closures(&self) -> &[Closure] {
        &self.closures
    }

    pub fn environments(&self) -> &[Environment] {
        &self.environments
    }

    pub fn codes(&self) -> &[CodeIr] {
        &self.codes
    }

    pub fn indexed_codes(&self) -> impl Iterator<Item = (CodeId, &CodeIr)> {
        self.codes
            .iter()
            .enumerate()
            .map(|(index, code)| (CodeId::from_index(index), code))
    }

    pub fn activations(&self) -> &[NamespaceActivationIr] {
        &self.activations
    }

    pub fn s3_registrations(&self) -> &[S3RegistrationIr] {
        &self.s3_registrations
    }

    pub fn native_components(&self) -> &[NativeComponentIr] {
        &self.native_components
    }

    pub fn resources(&self) -> &[ResourceIr] {
        &self.resources
    }

    pub fn indexed_resources(&self) -> impl Iterator<Item = (ResourceId, &ResourceIr)> {
        self.resources
            .iter()
            .enumerate()
            .map(|(index, resource)| (ResourceId::from_index(index), resource))
    }

    pub fn relocations(&self) -> &[Relocation] {
        &self.relocations
    }

    pub fn root_artifact(&self) -> &RootArtifactIr {
        &self.root_artifact
    }

    pub fn package(&self, id: PackageId) -> &PackageIr {
        &self.packages[&id]
    }

    pub fn namespace(&self, id: NamespaceId) -> &Namespace {
        &self.namespaces[id.index()]
    }

    pub fn root_namespace(&self) -> &Namespace {
        self.namespaces
            .iter()
            .find(|namespace| matches!(namespace.state, LinkNamespaceState::Root(_)))
            .expect("ProgramIr has one Root namespace")
    }

    pub fn binding(&self, id: BindingId) -> &Binding {
        &self.bindings[id.index()]
    }

    pub fn binding_namespace(&self, id: BindingId) -> NamespaceId {
        match &self.binding(id).state {
            LinkBindingState::Materialized { namespace, .. }
            | LinkBindingState::External { namespace, .. } => *namespace,
        }
    }

    pub fn closure(&self, id: ClosureId) -> &Closure {
        &self.closures[id.index()]
    }

    pub fn environment(&self, id: EnvironmentId) -> &Environment {
        &self.environments[id.index()]
    }

    pub fn code(&self, id: CodeId) -> &CodeIr {
        &self.codes[id.index()]
    }

    pub fn value(&self, id: ValueId) -> &Value {
        &self.values[id.index()]
    }

    pub fn s3_registration(&self, id: S3RegistrationId) -> &S3RegistrationIr {
        &self.s3_registrations[id.index()]
    }

    pub fn native_component(&self, id: NativeComponentId) -> &NativeComponentIr {
        &self.native_components[id.index()]
    }

    pub fn resource(&self, id: ResourceId) -> &ResourceIr {
        &self.resources[id.index()]
    }
}

/// Analysis finalizer; the only constructor for [`ProgramIr`].
pub struct ProgramBuilder {
    target: TargetContract,
    packages: BTreeMap<PackageId, PackageIr>,
    namespaces: Vec<Namespace>,
    bindings: Vec<Binding>,
    values: Vec<Value>,
    closures: Vec<Closure>,
    environments: Vec<Environment>,
    codes: Vec<CodeIr>,
    activations: Vec<NamespaceActivationIr>,
    s3_registrations: Vec<S3RegistrationIr>,
    native_components: Vec<NativeComponentIr>,
    resources: Vec<ResourceIr>,
    relocations: Vec<Relocation>,
    root_artifact: RootArtifactIr,
    root_package: Option<PackageId>,
}

/// IDs allocated while a namespace builder closes its final slot universe.
#[derive(Debug)]
pub struct FinalizedNamespace {
    pub namespace: NamespaceId,
    pub bindings: BTreeMap<String, BindingId>,
}

/// Final source of one materialized namespace slot.
#[derive(Clone, Debug)]
pub struct MaterializedSlot {
    pub name: String,
    pub source: MaterializedSlotSource,
}

/// Exact supported pre-activation contents of a namespace slot.
#[derive(Clone, Debug)]
pub enum MaterializedSlotSource {
    Unbound,
    Closure {
        source: Arc<str>,
        normalized_shape: Digest,
        binding: String,
    },
    Payload {
        binding: String,
    },
}

impl ProgramBuilder {
    fn new(target: TargetContract) -> Self {
        Self {
            target,
            packages: BTreeMap::new(),
            namespaces: Vec::new(),
            bindings: Vec::new(),
            values: Vec::new(),
            closures: Vec::new(),
            environments: Vec::new(),
            codes: Vec::new(),
            activations: Vec::new(),
            s3_registrations: Vec::new(),
            native_components: Vec::new(),
            resources: Vec::new(),
            relocations: Vec::new(),
            root_artifact: RootArtifactIr::default(),
            root_package: None,
        }
    }

    pub fn add_package(&mut self, id: PackageId, package: PackageIr) {
        if package.role() == PackageRole::Root {
            assert!(
                self.root_package.replace(id).is_none(),
                "one Root package per ProgramIr"
            );
        }
        assert!(
            self.packages.insert(id, package).is_none(),
            "one PackageIr per PackageId"
        );
    }

    pub fn add_namespace(&mut self, namespace: Namespace) -> NamespaceId {
        let id = NamespaceId::from_index(self.namespaces.len());
        self.namespaces.push(namespace);
        id
    }

    pub fn finish_materialized_namespace(
        &mut self,
        package: PackageId,
        role: MaterializedRole,
        slots: impl IntoIterator<Item = MaterializedSlot>,
    ) -> FinalizedNamespace {
        let namespace = NamespaceId::from_index(self.namespaces.len());
        let imports_environment = self.add_environment(Environment {
            kind: EnvironmentKind::Imports(namespace),
            parent: EnvironmentParentIr::BaseNamespace,
        });
        let namespace_environment = self.add_environment(Environment {
            kind: EnvironmentKind::Namespace(namespace),
            parent: EnvironmentParentIr::Materialized(imports_environment),
        });
        let mut bindings = BTreeMap::new();
        for slot in slots {
            let initial = match slot.source {
                MaterializedSlotSource::Unbound => InitialBindingState::Unbound,
                MaterializedSlotSource::Payload { binding } => {
                    let value = self.add_value(Value::Payload(PayloadRef { package, binding }));
                    InitialBindingState::Value(value)
                }
                MaterializedSlotSource::Closure {
                    source,
                    normalized_shape,
                    binding,
                } => {
                    let code = self.add_code(CodeIr::new(source, Vec::new(), normalized_shape));
                    let closure = self.add_closure(Closure {
                        code,
                        enclosure: namespace_environment,
                        payload: PayloadRef { package, binding },
                    });
                    let value = self.add_value(Value::Closure(closure));
                    InitialBindingState::Value(value)
                }
            };
            let binding = self.add_binding(Binding {
                name: slot.name.clone(),
                state: LinkBindingState::Materialized { namespace, initial },
            });
            assert!(
                bindings.insert(slot.name, binding).is_none(),
                "duplicate namespace slot"
            );
        }
        let state = MaterializedNamespaceState {
            namespace_environment,
            imports_environment,
        };
        let id = self.add_namespace(Namespace {
            package,
            bindings: bindings.clone(),
            imports: Vec::new(),
            state: match role {
                MaterializedRole::Root => LinkNamespaceState::Root(state),
                MaterializedRole::Linked => LinkNamespaceState::Linked(state),
            },
            s3_registrations: Vec::new(),
            native_components: Vec::new(),
        });
        assert_eq!(id, namespace);
        FinalizedNamespace {
            namespace,
            bindings,
        }
    }

    pub fn finish_external_namespace(
        &mut self,
        package: PackageId,
        bindings: impl IntoIterator<Item = (String, ExternalBindingAccess)>,
    ) -> FinalizedNamespace {
        let namespace = NamespaceId::from_index(self.namespaces.len());
        let mut slots = BTreeMap::new();
        for (name, access) in bindings {
            let binding = self.add_binding(Binding {
                name: name.clone(),
                state: LinkBindingState::External { namespace, access },
            });
            assert!(
                slots.insert(name, binding).is_none(),
                "duplicate external binding"
            );
        }
        let id = self.add_namespace(Namespace {
            package,
            bindings: slots.clone(),
            imports: Vec::new(),
            state: LinkNamespaceState::External { package },
            s3_registrations: Vec::new(),
            native_components: Vec::new(),
        });
        assert_eq!(id, namespace);
        FinalizedNamespace {
            namespace,
            bindings: slots,
        }
    }

    pub fn add_binding(&mut self, binding: Binding) -> BindingId {
        let id = BindingId::from_index(self.bindings.len());
        self.bindings.push(binding);
        id
    }

    pub fn add_environment(&mut self, environment: Environment) -> EnvironmentId {
        let id = EnvironmentId::from_index(self.environments.len());
        self.environments.push(environment);
        id
    }

    pub fn add_value(&mut self, value: Value) -> ValueId {
        let id = ValueId::from_index(self.values.len());
        self.values.push(value);
        id
    }

    pub fn add_code(&mut self, code: CodeIr) -> CodeId {
        let id = CodeId::from_index(self.codes.len());
        self.codes.push(code);
        id
    }

    pub fn add_closure(&mut self, closure: Closure) -> ClosureId {
        let id = ClosureId::from_index(self.closures.len());
        self.closures.push(closure);
        id
    }

    pub fn add_activation(&mut self, activation: NamespaceActivationIr) {
        self.activations.push(activation);
    }

    pub fn add_s3_registration(&mut self, registration: S3RegistrationIr) -> S3RegistrationId {
        let id = S3RegistrationId::from_index(self.s3_registrations.len());
        self.s3_registrations.push(registration);
        id
    }

    pub fn attach_s3_registration(
        &mut self,
        namespace: NamespaceId,
        generic: GenericId,
        class: String,
        method: BindingId,
    ) -> S3RegistrationId {
        let registration = self.add_s3_registration(S3RegistrationIr {
            owner_namespace: namespace,
            generic,
            class,
            method,
        });
        self.namespaces[namespace.index()]
            .s3_registrations
            .push(registration);
        registration
    }

    /// The binding a namespace sees under `name`: its own slot, else an import.
    pub fn visible_binding(&self, namespace: NamespaceId, name: &str) -> Option<BindingId> {
        let namespace = &self.namespaces[namespace.index()];
        namespace.bindings.get(name).copied().or_else(|| {
            namespace
                .imports
                .iter()
                .find(|import| import.local == name)
                .map(|import| import.target)
        })
    }

    pub fn attach_import(&mut self, namespace: NamespaceId, local: String, target: BindingId) {
        self.namespaces[namespace.index()]
            .imports
            .push(ImportBindingIr { local, target });
    }

    pub fn add_native_component(&mut self, component: NativeComponentIr) -> NativeComponentId {
        let id = NativeComponentId::from_index(self.native_components.len());
        self.native_components.push(component);
        id
    }

    pub fn attach_native_component(
        &mut self,
        namespace: NamespaceId,
        native: crate::package::NativeComponent,
    ) -> NativeComponentId {
        let component = self.add_native_component(NativeComponentIr { namespace, native });
        self.namespaces[namespace.index()]
            .native_components
            .push(component);
        component
    }

    pub fn add_resource(&mut self, resource: ResourceIr) -> ResourceId {
        let id = ResourceId::from_index(self.resources.len());
        self.resources.push(resource);
        id
    }

    pub fn add_relocation(&mut self, relocation: Relocation) {
        self.relocations.push(relocation);
    }

    pub fn binding_closure(&self, binding: BindingId) -> Option<ClosureId> {
        let LinkBindingState::Materialized {
            initial: InitialBindingState::Value(value),
            ..
        } = &self.bindings[binding.index()].state
        else {
            return None;
        };
        let Value::Closure(closure) = &self.values[value.index()] else {
            return None;
        };
        Some(*closure)
    }

    pub fn binding_code(&self, binding: BindingId) -> Option<CodeId> {
        self.binding_closure(binding)
            .map(|closure| self.closures[closure.index()].code)
    }

    pub fn binding_name(&self, binding: BindingId) -> &str {
        &self.bindings[binding.index()].name
    }

    pub fn binding_namespace(&self, binding: BindingId) -> NamespaceId {
        match &self.bindings[binding.index()].state {
            LinkBindingState::Materialized { namespace, .. }
            | LinkBindingState::External { namespace, .. } => *namespace,
        }
    }

    pub fn add_code_occurrence(&mut self, code: CodeId, start: usize, end: usize) -> CodeSite {
        let occurrence = CodeOccurrenceId::from_index(self.codes[code.index()].occurrences.len());
        self.codes[code.index()]
            .occurrences
            .push(CodeOccurrence { start, end });
        CodeSite { code, occurrence }
    }

    pub fn set_root_artifact(&mut self, root_artifact: RootArtifactIr) {
        self.root_artifact = root_artifact;
    }

    pub fn finish(self) -> ProgramIr {
        ProgramIr {
            target: self.target,
            root_package: self
                .root_package
                .expect("ProgramIr requires one Root package"),
            packages: self.packages,
            namespaces: self.namespaces,
            bindings: self.bindings,
            values: self.values,
            closures: self.closures,
            environments: self.environments,
            codes: self.codes,
            activations: self.activations,
            s3_registrations: self.s3_registrations,
            native_components: self.native_components,
            resources: self.resources,
            relocations: self.relocations,
            root_artifact: self.root_artifact,
        }
    }
}

/// Successful semantic provenance sidecar; never consumed by materialization.
#[derive(Debug, Default)]
pub struct ProvenanceIr {
    nodes: Vec<Node>,
    edges: Vec<Edge>,
    roots: Vec<NodeId>,
}

impl ProvenanceIr {
    pub(crate) fn from_analysis(graph: Graph, roots: Vec<NodeId>) -> Self {
        Self {
            nodes: graph.nodes,
            edges: graph.edges,
            roots,
        }
    }

    pub fn nodes(&self) -> &[Node] {
        &self.nodes
    }

    pub fn edges(&self) -> &[Edge] {
        &self.edges
    }

    pub fn roots(&self) -> &[NodeId] {
        &self.roots
    }

    pub(crate) fn graph(&self) -> Graph {
        Graph::from_parts(self.nodes.clone(), self.edges.clone())
    }

    pub fn binding(&self, package: &str, name: &str) -> Option<NodeId> {
        self.nodes
            .iter()
            .find(|node| {
                node.package == package
                    && matches!(&node.kind, crate::analysis::NodeKind::Binding { name: candidate } if candidate == name)
            })
            .map(|node| node.id)
    }

    pub fn incoming(&self, node: NodeId) -> impl Iterator<Item = &Edge> {
        self.edges.iter().filter(move |edge| edge.to == node)
    }

    pub fn shortest_path(&self, roots: &[NodeId], target: NodeId) -> Option<Vec<&Edge>> {
        let graph = self.graph();
        let indexes = graph
            .shortest_path(roots, target)?
            .into_iter()
            .map(|edge| {
                self.edges
                    .iter()
                    .position(|candidate| {
                        candidate.from == edge.from
                            && candidate.to == edge.to
                            && candidate.kind == edge.kind
                            && candidate.reason == edge.reason
                            && candidate.span == edge.span
                    })
                    .expect("derived graph edge belongs to provenance")
            })
            .collect::<Vec<_>>();
        Some(
            indexes
                .into_iter()
                .map(|index| &self.edges[index])
                .collect(),
        )
    }

    pub fn missing_packages(&self) -> impl Iterator<Item = NodeId> + '_ {
        self.nodes
            .iter()
            .filter(|node| matches!(node.kind, crate::analysis::NodeKind::MissingPackage))
            .map(|node| node.id)
    }

    pub fn dump(&self) -> String {
        self.graph().dump()
    }
}
