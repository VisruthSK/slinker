//! Immutable linked-program representation consumed by build preflight and materialization.

use crate::analysis::{Diagnostic, Edge, Graph, Node, NodeId};
use crate::package::Digest;
use crate::{Version, syntax::Span};
use std::collections::BTreeMap;
use std::marker::PhantomData;
use std::path::PathBuf;
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

id_type!(PackageId);
id_type!(NamespaceId);
id_type!(BindingId);
id_type!(EnvironmentBindingId);
id_type!(ValueId);
id_type!(ClosureId);
id_type!(EnvironmentId);
id_type!(CodeId);
id_type!(CodeOccurrenceId);
id_type!(NamespaceActivationId);
id_type!(S3RegistrationId);
id_type!(NativeComponentId);
id_type!(ResourceId);

mod sealed {
    pub trait Sealed {}
}

/// Marker for immutable installed-image runtime facts.
#[derive(Debug)]
pub enum ImagePhase {}

/// Marker for immutable finalized linked-program facts.
#[derive(Debug)]
pub enum LinkPhase {}

impl sealed::Sealed for ImagePhase {}
impl sealed::Sealed for LinkPhase {}

/// Shared ID vocabulary for persistent image/link runtime entities.
pub trait RuntimePhase: sealed::Sealed {
    type BindingId: Copy + Eq + std::hash::Hash;
    type EnvironmentBindingId: Copy + Eq + std::hash::Hash;
    type ValueId: Copy + Eq + std::hash::Hash;
    type ClosureId: Copy + Eq + std::hash::Hash;
    type EnvironmentId: Copy + Eq + std::hash::Hash;
    type NamespaceId: Copy + Eq + std::hash::Hash;
    type CodeId: Copy + Eq + std::hash::Hash;
    type BindingState;
    type NamespaceState;
}

impl RuntimePhase for LinkPhase {
    type BindingId = BindingId;
    type EnvironmentBindingId = EnvironmentBindingId;
    type ValueId = ValueId;
    type ClosureId = ClosureId;
    type EnvironmentId = EnvironmentId;
    type NamespaceId = NamespaceId;
    type CodeId = CodeId;
    type BindingState = LinkBindingState;
    type NamespaceState = LinkNamespaceState;
}

/// Exact build-time semantic identity of a package image.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackageIdentity {
    pub name: String,
    pub version: Version,
    pub image_fingerprint: Digest,
}

/// Build-time physical location, intentionally absent from [`ProgramIr`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackageLocation {
    pub library: PathBuf,
    pub root: PathBuf,
}

/// How a package participates in the generated artifact.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PackageRole {
    Root,
    Linked,
    External,
}

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

/// DESCRIPTION-governed runtime requirement for an External package.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExternalPackageContract {
    pub package: String,
    pub requirements: Vec<String>,
}

/// One stable namespace binding slot.
#[derive(Clone, Debug)]
pub struct Binding<P: RuntimePhase> {
    pub name: String,
    pub state: P::BindingState,
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
    pub exports: ExportTable,
    pub activation: Option<NamespaceActivationId>,
}

#[derive(Clone, Debug)]
pub struct Namespace<P: RuntimePhase> {
    pub package: PackageId,
    pub bindings: BTreeMap<String, P::BindingId>,
    pub imports: Vec<ImportBindingIr>,
    pub state: P::NamespaceState,
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
    Private,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EnvironmentParentIr {
    Materialized(EnvironmentId),
    ExternalNamespace(NamespaceId),
    BaseNamespace,
    Empty,
}

#[derive(Clone, Debug)]
pub struct Environment<P: RuntimePhase> {
    pub kind: EnvironmentKind,
    pub parent: EnvironmentParentIr,
    pub bindings: BTreeMap<String, P::EnvironmentBindingId>,
}

#[derive(Clone, Debug)]
pub struct EnvironmentBinding<P: RuntimePhase> {
    pub environment: P::EnvironmentId,
    pub name: String,
    pub initial: InitialBindingState,
}

#[derive(Clone, Debug)]
pub struct Closure<P: RuntimePhase> {
    pub code: P::CodeId,
    pub enclosure: P::EnvironmentId,
    pub payload: PayloadRef,
}

/// Supported persistent runtime value without an Unknown state.
#[derive(Clone, Debug)]
pub enum Value<P: RuntimePhase> {
    Null,
    Logical(Vec<Option<bool>>),
    Integer(Vec<Option<i32>>),
    Double(Vec<f64>),
    Character(Vec<Option<String>>),
    Raw(Vec<u8>),
    List(Vec<P::ValueId>),
    Closure(P::ClosureId),
    Environment(P::EnvironmentId),
    Payload(PayloadRef),
}

/// Durable locator relative to one exact installed package binding.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct InstalledObjectLocator {
    pub root: String,
    pub path: Vec<ObjectPathStep>,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum ObjectPathStep {
    ListElement(usize),
    PairlistElement(usize),
    Attribute(String),
    EnvironmentBinding(String),
    ClosureEnclosure,
}

/// Physical payload source selected only after linked identity is fixed.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct PayloadRef {
    pub package: PackageId,
    pub locator: InstalledObjectLocator,
}

#[derive(Clone, Debug)]
pub struct CodeIr {
    source: Arc<str>,
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
            source,
            occurrences,
            normalized_shape,
        }
    }

    pub fn source(&self) -> &str {
        &self.source
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
    FindPackage,
}

#[derive(Clone, Debug)]
pub struct NamespaceActivationIr {
    pub dependencies: Vec<NamespaceId>,
    pub on_load: Option<OnLoadIr>,
}

#[derive(Clone, Debug)]
pub struct OnLoadIr {
    pub closure: ClosureId,
    pub package_name: String,
    pub libname: LinkedLibnameUse,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LinkedLibnameUse {
    SemanticallyUnused,
    LoweredToResources,
}

#[derive(Clone, Debug)]
pub struct ExternalImportIr {
    pub namespace: NamespaceId,
    pub bindings: Vec<BindingId>,
}

#[derive(Clone, Debug)]
pub struct LinkedImportIr {
    pub namespace: NamespaceId,
    pub bindings: Vec<BindingId>,
}

#[derive(Clone, Debug, Default)]
pub struct RootArtifactIr {
    pub description: Arc<str>,
    pub namespace: Arc<str>,
    pub external_description_requirements: Vec<ExternalPackageContract>,
    pub external_namespace_imports: Vec<ExternalImportIr>,
    pub linked_imports: Vec<LinkedImportIr>,
    pub bootstrap_namespaces: Vec<NamespaceId>,
    pub original_on_load: Option<ClosureId>,
    pub retained_resources: Vec<ResourceId>,
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
    pub name: String,
}

#[derive(Clone, Debug)]
pub struct ResourceIr {
    pub package: PackageId,
    pub path: String,
}

#[derive(Clone, Debug)]
pub enum Root {
    RootNamespace(NamespaceId),
    ExportedBinding(BindingId),
    Lifecycle(NamespaceActivationId),
}

/// Deferred finite runtime behavior. The first profile rejects every value.
#[derive(Clone, Debug)]
pub enum ResidualCapability {}

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
    packages: Vec<PackageIr>,
    namespaces: Vec<Namespace<LinkPhase>>,
    bindings: Vec<Binding<LinkPhase>>,
    environment_bindings: Vec<EnvironmentBinding<LinkPhase>>,
    values: Vec<Value<LinkPhase>>,
    closures: Vec<Closure<LinkPhase>>,
    environments: Vec<Environment<LinkPhase>>,
    codes: Vec<CodeIr>,
    activations: Vec<NamespaceActivationIr>,
    s3_registrations: Vec<S3RegistrationIr>,
    native_components: Vec<NativeComponentIr>,
    resources: Vec<ResourceIr>,
    relocations: Vec<Relocation>,
    residuals: Vec<ResidualCapability>,
    roots: Vec<Root>,
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

    pub fn packages(&self) -> &[PackageIr] {
        &self.packages
    }

    pub fn package_ids(&self) -> impl Iterator<Item = PackageId> + '_ {
        (0..self.packages.len()).map(PackageId::from_index)
    }

    pub fn namespaces(&self) -> &[Namespace<LinkPhase>] {
        &self.namespaces
    }

    pub fn bindings(&self) -> &[Binding<LinkPhase>] {
        &self.bindings
    }

    pub fn values(&self) -> &[Value<LinkPhase>] {
        &self.values
    }

    pub fn closures(&self) -> &[Closure<LinkPhase>] {
        &self.closures
    }

    pub fn environment_bindings(&self) -> &[EnvironmentBinding<LinkPhase>] {
        &self.environment_bindings
    }

    pub fn environments(&self) -> &[Environment<LinkPhase>] {
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

    pub fn roots(&self) -> &[Root] {
        &self.roots
    }

    pub fn relocations(&self) -> &[Relocation] {
        &self.relocations
    }

    pub fn residuals(&self) -> &[ResidualCapability] {
        &self.residuals
    }

    pub fn root_artifact(&self) -> &RootArtifactIr {
        &self.root_artifact
    }

    pub fn package(&self, id: PackageId) -> &PackageIr {
        &self.packages[id.index()]
    }

    pub fn namespace(&self, id: NamespaceId) -> &Namespace<LinkPhase> {
        &self.namespaces[id.index()]
    }

    pub fn binding(&self, id: BindingId) -> &Binding<LinkPhase> {
        &self.bindings[id.index()]
    }

    pub fn binding_namespace(&self, id: BindingId) -> NamespaceId {
        match &self.binding(id).state {
            LinkBindingState::Materialized { namespace, .. }
            | LinkBindingState::External { namespace, .. } => *namespace,
        }
    }

    pub fn closure(&self, id: ClosureId) -> &Closure<LinkPhase> {
        &self.closures[id.index()]
    }

    pub fn environment(&self, id: EnvironmentId) -> &Environment<LinkPhase> {
        &self.environments[id.index()]
    }

    pub fn code(&self, id: CodeId) -> &CodeIr {
        &self.codes[id.index()]
    }

    pub fn value(&self, id: ValueId) -> &Value<LinkPhase> {
        &self.values[id.index()]
    }

    pub fn environment_binding(&self, id: EnvironmentBindingId) -> &EnvironmentBinding<LinkPhase> {
        &self.environment_bindings[id.index()]
    }

    pub fn activation(&self, id: NamespaceActivationId) -> &NamespaceActivationIr {
        &self.activations[id.index()]
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
    packages: Vec<PackageIr>,
    namespaces: Vec<Namespace<LinkPhase>>,
    bindings: Vec<Binding<LinkPhase>>,
    environment_bindings: Vec<EnvironmentBinding<LinkPhase>>,
    values: Vec<Value<LinkPhase>>,
    closures: Vec<Closure<LinkPhase>>,
    environments: Vec<Environment<LinkPhase>>,
    codes: Vec<CodeIr>,
    activations: Vec<NamespaceActivationIr>,
    s3_registrations: Vec<S3RegistrationIr>,
    native_components: Vec<NativeComponentIr>,
    resources: Vec<ResourceIr>,
    relocations: Vec<Relocation>,
    roots: Vec<Root>,
    root_artifact: RootArtifactIr,
    root_package: Option<PackageId>,
    _phase: PhantomData<LinkPhase>,
}

/// IDs allocated while a namespace builder closes its final slot universe.
#[derive(Debug)]
pub struct FinalizedNamespace {
    pub namespace: NamespaceId,
    pub namespace_environment: Option<EnvironmentId>,
    pub imports_environment: Option<EnvironmentId>,
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
        locator: InstalledObjectLocator,
    },
    Payload {
        locator: InstalledObjectLocator,
    },
}

impl ProgramBuilder {
    fn new(target: TargetContract) -> Self {
        Self {
            target,
            packages: Vec::new(),
            namespaces: Vec::new(),
            bindings: Vec::new(),
            environment_bindings: Vec::new(),
            values: Vec::new(),
            closures: Vec::new(),
            environments: Vec::new(),
            codes: Vec::new(),
            activations: Vec::new(),
            s3_registrations: Vec::new(),
            native_components: Vec::new(),
            resources: Vec::new(),
            relocations: Vec::new(),
            roots: Vec::new(),
            root_artifact: RootArtifactIr::default(),
            root_package: None,
            _phase: PhantomData,
        }
    }

    pub fn add_package(&mut self, package: PackageIr) -> PackageId {
        let id = PackageId::from_index(self.packages.len());
        if package.role() == PackageRole::Root {
            assert!(
                self.root_package.replace(id).is_none(),
                "one Root package per ProgramIr"
            );
        }
        self.packages.push(package);
        id
    }

    pub fn add_namespace(&mut self, namespace: Namespace<LinkPhase>) -> NamespaceId {
        let id = NamespaceId::from_index(self.namespaces.len());
        self.namespaces.push(namespace);
        id
    }

    pub fn finish_materialized_namespace(
        &mut self,
        package: PackageId,
        role: PackageRole,
        slots: impl IntoIterator<Item = MaterializedSlot>,
        exports: impl IntoIterator<Item = String>,
        on_load: Option<String>,
    ) -> FinalizedNamespace {
        assert!(matches!(role, PackageRole::Root | PackageRole::Linked));
        let namespace = NamespaceId::from_index(self.namespaces.len());
        let imports_environment = self.add_environment(Environment {
            kind: EnvironmentKind::Imports(namespace),
            parent: EnvironmentParentIr::BaseNamespace,
            bindings: BTreeMap::new(),
        });
        let namespace_environment = self.add_environment(Environment {
            kind: EnvironmentKind::Namespace(namespace),
            parent: EnvironmentParentIr::Materialized(imports_environment),
            bindings: BTreeMap::new(),
        });
        let mut bindings = BTreeMap::new();
        for slot in slots {
            let initial = match slot.source {
                MaterializedSlotSource::Unbound => InitialBindingState::Unbound,
                MaterializedSlotSource::Payload { locator } => {
                    let value = self.add_value(Value::Payload(PayloadRef { package, locator }));
                    InitialBindingState::Value(value)
                }
                MaterializedSlotSource::Closure {
                    source,
                    normalized_shape,
                    locator,
                } => {
                    let code = self.add_code(CodeIr::new(source, Vec::new(), normalized_shape));
                    let closure = self.add_closure(Closure {
                        code,
                        enclosure: namespace_environment,
                        payload: PayloadRef { package, locator },
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
        let exports = ExportTable::new(exports.into_iter().map(|name| bindings[&name]).collect());
        let activation = on_load.and_then(|name| {
            let binding = bindings.get(&name)?;
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
            Some(self.add_activation(NamespaceActivationIr {
                dependencies: Vec::new(),
                on_load: Some(OnLoadIr {
                    closure: *closure,
                    package_name: self.packages[package.index()].identity().name.clone(),
                    libname: LinkedLibnameUse::SemanticallyUnused,
                }),
            }))
        });
        let state = MaterializedNamespaceState {
            namespace_environment,
            imports_environment,
            exports,
            activation,
        };
        let id = self.add_namespace(Namespace {
            package,
            bindings: bindings.clone(),
            imports: Vec::new(),
            state: match role {
                PackageRole::Root => LinkNamespaceState::Root(state),
                PackageRole::Linked => LinkNamespaceState::Linked(state),
                PackageRole::External => unreachable!(),
            },
            s3_registrations: Vec::new(),
            native_components: Vec::new(),
        });
        assert_eq!(id, namespace);
        FinalizedNamespace {
            namespace,
            namespace_environment: Some(namespace_environment),
            imports_environment: Some(imports_environment),
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
            namespace_environment: None,
            imports_environment: None,
            bindings: slots,
        }
    }

    pub fn add_binding(&mut self, binding: Binding<LinkPhase>) -> BindingId {
        let id = BindingId::from_index(self.bindings.len());
        self.bindings.push(binding);
        id
    }

    pub fn add_environment(&mut self, environment: Environment<LinkPhase>) -> EnvironmentId {
        let id = EnvironmentId::from_index(self.environments.len());
        self.environments.push(environment);
        id
    }

    pub fn add_environment_binding(
        &mut self,
        binding: EnvironmentBinding<LinkPhase>,
    ) -> EnvironmentBindingId {
        let id = EnvironmentBindingId::from_index(self.environment_bindings.len());
        self.environment_bindings.push(binding);
        id
    }

    pub fn add_value(&mut self, value: Value<LinkPhase>) -> ValueId {
        let id = ValueId::from_index(self.values.len());
        self.values.push(value);
        id
    }

    pub fn add_code(&mut self, code: CodeIr) -> CodeId {
        let id = CodeId::from_index(self.codes.len());
        self.codes.push(code);
        id
    }

    pub fn add_closure(&mut self, closure: Closure<LinkPhase>) -> ClosureId {
        let id = ClosureId::from_index(self.closures.len());
        self.closures.push(closure);
        id
    }

    pub fn add_activation(&mut self, activation: NamespaceActivationIr) -> NamespaceActivationId {
        let id = NamespaceActivationId::from_index(self.activations.len());
        self.activations.push(activation);
        id
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
        name: String,
    ) -> NativeComponentId {
        let component = self.add_native_component(NativeComponentIr { namespace, name });
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

    pub fn binding_code(&self, binding: BindingId) -> Option<CodeId> {
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
        Some(self.closures[closure.index()].code)
    }

    pub fn binding_namespace(&self, binding: BindingId) -> NamespaceId {
        match &self.bindings[binding.index()].state {
            LinkBindingState::Materialized { namespace, .. }
            | LinkBindingState::External { namespace, .. } => *namespace,
        }
    }

    pub fn set_activation_dependencies(
        &mut self,
        namespace: NamespaceId,
        mut dependencies: Vec<NamespaceId>,
    ) {
        dependencies.sort();
        dependencies.dedup();
        let state = &self.namespaces[namespace.index()].state;
        let activation = match state {
            LinkNamespaceState::Root(state) | LinkNamespaceState::Linked(state) => state.activation,
            LinkNamespaceState::External { .. } => None,
        };
        if let Some(activation) = activation {
            self.activations[activation.index()].dependencies = dependencies;
        }
    }

    pub fn add_code_occurrence(&mut self, code: CodeId, start: usize, end: usize) -> CodeSite {
        let occurrence = CodeOccurrenceId::from_index(self.codes[code.index()].occurrences.len());
        self.codes[code.index()]
            .occurrences
            .push(CodeOccurrence { start, end });
        CodeSite { code, occurrence }
    }

    pub fn add_root(&mut self, root: Root) {
        self.roots.push(root);
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
            environment_bindings: self.environment_bindings,
            values: self.values,
            closures: self.closures,
            environments: self.environments,
            codes: self.codes,
            activations: self.activations,
            s3_registrations: self.s3_registrations,
            native_components: self.native_components,
            resources: self.resources,
            relocations: self.relocations,
            residuals: Vec::new(),
            roots: self.roots,
            root_artifact: self.root_artifact,
        }
    }
}

/// Typed analysis blocker retained outside construction authority.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AnalysisBlocker {
    OpenNamespaceShape { site: Option<Span> },
    OpenEnvironmentShape { site: Option<Span> },
    MutableEnvironmentParent { site: Option<Span> },
    MutableClosureEnclosure { site: Option<Span> },
    OpenCallable { site: Option<Span> },
    OpenReflection { site: Option<Span> },
    RuntimeRepresentationIntrospection { site: Option<Span> },
    UnsupportedActiveBinding { binding: String },
    UnsupportedAltrep { binding: String },
    UnsupportedNestedPromise { binding: String },
    UnsupportedObjectSystem { site: Option<Span> },
    UnsupportedNative { component: String },
    UnsupportedLinkedLibname { package: String },
    UnsupportedCodeRepresentation { binding: String },
    UnsupportedRootTransformation { detail: String },
}

#[derive(Clone, Debug, Default)]
pub struct AnalysisBlockerSet {
    blockers: Vec<AnalysisBlocker>,
}

impl AnalysisBlockerSet {
    pub fn as_slice(&self) -> &[AnalysisBlocker] {
        &self.blockers
    }

    pub fn push(&mut self, blocker: AnalysisBlocker) {
        if !self.blockers.contains(&blocker) {
            self.blockers.push(blocker);
        }
    }
}

/// Successful semantic provenance sidecar; never consumed by materialization.
#[derive(Debug, Default)]
pub struct ProvenanceIr {
    nodes: Vec<Node>,
    edges: Vec<Edge>,
    roots: Vec<NodeId>,
    diagnostics: Vec<Diagnostic>,
    records: Vec<ProvenanceRecord>,
}

impl ProvenanceIr {
    pub(crate) fn from_analysis(
        graph: Graph,
        roots: Vec<NodeId>,
        diagnostics: Vec<Diagnostic>,
    ) -> Self {
        let records = graph
            .edges
            .iter()
            .map(|edge| ProvenanceRecord {
                from: format!("{:?}", graph.nodes[edge.from.0].kind),
                to: format!("{:?}", graph.nodes[edge.to.0].kind),
                reason: edge.reason.clone(),
                source: edge.span.clone(),
            })
            .collect();
        Self {
            nodes: graph.nodes,
            edges: graph.edges,
            roots,
            diagnostics,
            records,
        }
    }

    pub fn records(&self) -> &[ProvenanceRecord] {
        &self.records
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

    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
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

#[derive(Clone, Debug)]
pub struct ProvenanceRecord {
    pub from: String,
    pub to: String,
    pub reason: String,
    pub source: Option<Span>,
}
