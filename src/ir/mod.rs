//! Immutable linked-program representation consumed by build preflight and materialization.

use crate::analysis::{Edge, Graph, Node, NodeId};
pub use crate::package::{
    BindingName, ClassName, ComponentName, GenericName, PackageId, PackageIdentity, PackageName,
    PackageRole,
};

use crate::package::Digest;
use crate::syntax::TextRange;
use std::collections::{BTreeMap, BTreeSet};
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
id_type!(ResourceId);
id_type!(PayloadBundleId);

/// Final package runtime contract.
#[derive(Clone, Debug)]
pub enum PackageIr {
    Root {
        build_identity: PackageIdentity,
    },
    Linked {
        build_identity: PackageIdentity,
        namespace_key: PrivateNamespaceKey,
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
            Self::Root { build_identity } | Self::Linked { build_identity, .. } => build_identity,
            Self::External {
                analyzed_identity, ..
            } => analyzed_identity,
        }
    }

    pub fn registered_namespace(&self) -> RegisteredNamespace<'_> {
        match self {
            Self::Root { build_identity } => RegisteredNamespace::Package(&build_identity.name),
            Self::External {
                analyzed_identity, ..
            } => RegisteredNamespace::Package(&analyzed_identity.name),
            Self::Linked { namespace_key, .. } => RegisteredNamespace::Private(namespace_key),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PrivateNamespaceKey(String);

impl PrivateNamespaceKey {
    fn derive(root: &PackageName, linked: &PackageName) -> Self {
        Self(format!("{root}:{linked}"))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RegisteredNamespace<'a> {
    Package(&'a PackageName),
    Private(&'a PrivateNamespaceKey),
}

impl<'a> RegisteredNamespace<'a> {
    pub fn as_str(&self) -> &'a str {
        match self {
            Self::Package(name) => name.as_str(),
            Self::Private(key) => key.as_str(),
        }
    }
}

/// DESCRIPTION-governed runtime requirement for an External package. Platform packages ship with
/// the selected R, so the target contract already satisfies them.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExternalPackageContract {
    pub package: PackageName,
    pub platform: bool,
    pub requirements: Vec<crate::Relation>,
}

/// One stable namespace binding slot.
#[derive(Clone, Debug)]
pub struct Binding {
    pub name: BindingName,
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
    pub payload: Option<PayloadBundleId>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MaterializedRole {
    Root,
    Linked,
}

#[derive(Clone, Debug)]
pub struct Namespace {
    pub package: PackageId,
    pub bindings: BTreeMap<BindingName, BindingId>,
    pub imports: Vec<ImportBindingIr>,
    pub state: LinkNamespaceState,
    pub s3_registrations: Vec<S3RegistrationId>,
}

#[derive(Clone, Debug)]
pub struct ImportBindingIr {
    pub local: BindingName,
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
}

/// Supported persistent runtime value without an Unknown state.
#[derive(Clone, Debug)]
pub enum Value {
    Closure(ClosureId),
    Payload(PayloadBundleId),
}

/// The retained non-source bindings of one Root or Linked namespace, carried by a single R
/// serialization and restored into that namespace. One serialization preserves sharing, cycles,
/// private environments with their parents, closure enclosures, and attributes among these
/// bindings; identity never extends to another bundle.
#[derive(Clone, Debug)]
pub struct PayloadBundleIr {
    namespace: NamespaceId,
    bindings: Vec<BindingId>,
    closure_patches: Vec<PayloadClosurePatch>,
    dependencies: BTreeSet<PayloadDependency>,
}

impl PayloadBundleIr {
    pub fn namespace(&self) -> NamespaceId {
        self.namespace
    }

    pub fn bindings(&self) -> &[BindingId] {
        &self.bindings
    }

    pub fn closure_patches(&self) -> &[PayloadClosurePatch] {
        &self.closure_patches
    }

    /// Foreign namespaces the serialized references resolve to, each activated before restore.
    pub fn dependencies(&self) -> &BTreeSet<PayloadDependency> {
        &self.dependencies
    }
}

/// A namespace a payload bundle refers to. The Root namespace is activated only after every
/// Linked bundle is restored, so it is never a dependency.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum PayloadDependency {
    Linked(NamespaceId),
    External(NamespaceId),
}

impl PayloadDependency {
    pub fn namespace(self) -> NamespaceId {
        match self {
            Self::Linked(namespace) | Self::External(namespace) => namespace,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InvalidPayloadDependency {
    RootNamespace,
}

#[derive(Clone, Debug)]
pub struct CodeIr {
    source: Arc<str>,
    value_start: Option<usize>,
    occurrences: Vec<TextRange>,
    normalized_shape: Digest,
}

impl CodeIr {
    pub fn new(source: Arc<str>, normalized_shape: Digest) -> Self {
        Self {
            value_start: crate::syntax::assigned_value_start(&source),
            source,
            occurrences: Vec::new(),
            normalized_shape,
        }
    }

    pub fn source(&self) -> &str {
        &self.source
    }

    pub fn assigned_value_start(&self) -> Option<usize> {
        self.value_start
    }

    pub fn occurrence(&self, id: CodeOccurrenceId) -> TextRange {
        self.occurrences[id.index()]
    }

    pub fn normalized_shape(&self) -> &Digest {
        &self.normalized_shape
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CodeSite {
    pub code: CodeId,
    pub occurrence: CodeOccurrenceId,
}

#[derive(Clone, Debug)]
pub struct Relocation {
    pub site: CodeSite,
    pub target: RelocationTarget,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RelocationTarget {
    Binding {
        target: BindingId,
        access: ExternalBindingAccess,
    },
    RequireNamespace {
        result: bool,
    },
    Namespace {
        package: PackageId,
        operation: NamespaceOperation,
    },
    PackageVersion {
        version: String,
    },
    Resource {
        target: ResourceId,
    },
    LoadedQuery,
    NamespaceArgument {
        package: PackageId,
    },
    DescriptionArgument {
        description: ResourceId,
    },
    NativeSymbol {
        package: PackageId,
        component: ComponentName,
        symbol: String,
    },
    NativeLibrary {
        package: PackageId,
        component: ComponentName,
    },
    InstalledQuery {
        check: bool,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NamespaceOperation {
    Load,
    Get,
    As,
}

impl NamespaceOperation {
    fn callee(self) -> &'static str {
        match self {
            Self::Load => "loadNamespace(",
            Self::Get => "getNamespace(",
            Self::As => "asNamespace(",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InvalidRelocation {
    OutsideCode,
    Overlap,
    Mismatch(String),
}

impl std::fmt::Display for InvalidRelocation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::OutsideCode => f.write_str("relocation occurrence lies outside its code unit"),
            Self::Overlap => f.write_str("relocation occurrences overlap in one code unit"),
            Self::Mismatch(original) => {
                write!(
                    f,
                    "relocation does not match the syntax it rewrites: `{original}`"
                )
            }
        }
    }
}

/// One Linked namespace activation, in the order the Root `.onLoad` wrapper performs them.
#[derive(Clone, Debug)]
pub struct NamespaceActivationIr {
    pub namespace: NamespaceId,
    pub on_load: Option<BindingId>,
    pub native_components: Vec<crate::package::NativeComponent>,
    pub exports: ExportTable,
    pub unretained: BTreeMap<String, UnretainedName>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UnretainedName {
    Stub,
    ExportedStub,
    ExportedByActivation,
}

impl UnretainedName {
    pub fn is_stub(self) -> bool {
        matches!(self, Self::Stub | Self::ExportedStub)
    }

    pub fn is_exported(self) -> bool {
        matches!(self, Self::ExportedStub | Self::ExportedByActivation)
    }
}

/// Root source-package transformation decided at finalization: the generated `DESCRIPTION`, the
/// `NAMESPACE` exports and native libraries, and the original Root `.onLoad` that the generated
/// wrapper calls last.
#[derive(Clone, Debug, Default)]
pub struct RootArtifactIr {
    pub description: Option<Arc<str>>,
    pub exports: ExportTable,
    pub native_components: Vec<crate::package::NativeComponent>,
    pub on_load: Option<ClosureId>,
    pub activated_s3: Vec<S3RegistrationId>,
}

#[derive(Clone, Debug)]
pub struct GenericId {
    pub home: GenericHome,
    pub name: GenericName,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GenericHome {
    Lexical,
    Program(PackageId),
    Optional(PackageName),
}

#[derive(Clone, Debug)]
pub struct S3RegistrationIr {
    pub owner_namespace: NamespaceId,
    pub generic: GenericId,
    pub class: ClassName,
    pub method: BindingId,
}

#[derive(Clone, Debug)]
pub struct ResourceIr {
    pub package: PackageId,
    pub path: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ObjectStep {
    Environment,
    Parent,
    Binding(BindingName),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ClosureHome {
    Namespace,
    Reached {
        root: BindingName,
        steps: Vec<ObjectStep>,
    },
}

#[derive(Clone, Debug)]
pub struct PayloadClosurePatch {
    pub home: ClosureHome,
    pub binding: BindingName,
    pub code: CodeId,
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
    resources: Vec<ResourceIr>,
    payload_bundles: Vec<PayloadBundleIr>,
    relocations: Vec<Relocation>,
    root_artifact: RootArtifactIr,
}

impl ProgramIr {
    pub fn builder(
        target: TargetContract,
        root: PackageId,
        root_identity: PackageIdentity,
    ) -> ProgramBuilder {
        ProgramBuilder::new(target, root, root_identity)
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

    pub fn resources(&self) -> &[ResourceIr] {
        &self.resources
    }

    pub fn indexed_resources(&self) -> impl Iterator<Item = (ResourceId, &ResourceIr)> {
        self.resources
            .iter()
            .enumerate()
            .map(|(index, resource)| (ResourceId::from_index(index), resource))
    }

    pub fn payload_bundles(&self) -> &[PayloadBundleIr] {
        &self.payload_bundles
    }

    pub fn indexed_payload_bundles(
        &self,
    ) -> impl Iterator<Item = (PayloadBundleId, &PayloadBundleIr)> {
        self.payload_bundles
            .iter()
            .enumerate()
            .map(|(index, bundle)| (PayloadBundleId::from_index(index), bundle))
    }

    pub fn payload_bundle(&self, id: PayloadBundleId) -> &PayloadBundleIr {
        &self.payload_bundles[id.index()]
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
    resources: Vec<ResourceIr>,
    payload_bundles: Vec<PayloadBundleIr>,
    relocations: Vec<Relocation>,
    root_artifact: RootArtifactIr,
    root_package: PackageId,
}

/// IDs allocated while a namespace builder closes its final slot universe.
#[derive(Debug)]
pub struct FinalizedNamespace {
    pub namespace: NamespaceId,
    pub bindings: BTreeMap<BindingName, BindingId>,
}

/// Final source of one materialized namespace slot.
#[derive(Clone, Debug)]
pub struct MaterializedSlot {
    pub name: BindingName,
    pub source: MaterializedSlotSource,
}

/// Exact supported pre-activation contents of a namespace slot.
#[derive(Clone, Debug)]
pub enum MaterializedSlotSource {
    Unbound,
    Closure {
        source: Arc<str>,
        normalized_shape: Digest,
    },
    Payload,
}

impl ProgramBuilder {
    fn new(target: TargetContract, root: PackageId, root_identity: PackageIdentity) -> Self {
        Self {
            target,
            packages: BTreeMap::from([(
                root,
                PackageIr::Root {
                    build_identity: root_identity,
                },
            )]),
            namespaces: Vec::new(),
            bindings: Vec::new(),
            values: Vec::new(),
            closures: Vec::new(),
            environments: Vec::new(),
            codes: Vec::new(),
            activations: Vec::new(),
            s3_registrations: Vec::new(),
            resources: Vec::new(),
            payload_bundles: Vec::new(),
            relocations: Vec::new(),
            root_artifact: RootArtifactIr::default(),
            root_package: root,
        }
    }

    pub fn add_linked_package(&mut self, id: PackageId, build_identity: PackageIdentity) {
        let namespace_key = PrivateNamespaceKey::derive(
            &self.packages[&self.root_package].identity().name,
            &build_identity.name,
        );
        self.insert_package(
            id,
            PackageIr::Linked {
                build_identity,
                namespace_key,
            },
        );
    }

    pub fn add_external_package(
        &mut self,
        id: PackageId,
        analyzed_identity: PackageIdentity,
        contract: ExternalPackageContract,
    ) {
        self.insert_package(
            id,
            PackageIr::External {
                analyzed_identity,
                contract,
            },
        );
    }

    fn insert_package(&mut self, id: PackageId, package: PackageIr) {
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
        let mut payload = None;
        for slot in slots {
            let (initial, bundle) = match slot.source {
                MaterializedSlotSource::Unbound => (InitialBindingState::Unbound, None),
                MaterializedSlotSource::Payload => {
                    let bundle = *payload.get_or_insert_with(|| {
                        let id = PayloadBundleId::from_index(self.payload_bundles.len());
                        self.payload_bundles.push(PayloadBundleIr {
                            namespace,
                            bindings: Vec::new(),
                            closure_patches: Vec::new(),
                            dependencies: BTreeSet::new(),
                        });
                        id
                    });
                    let value = self.add_value(Value::Payload(bundle));
                    (InitialBindingState::Value(value), Some(bundle))
                }
                MaterializedSlotSource::Closure {
                    source,
                    normalized_shape,
                } => {
                    let code = self.add_code(CodeIr::new(source, normalized_shape));
                    let closure = self.add_closure(Closure {
                        code,
                        enclosure: namespace_environment,
                    });
                    let value = self.add_value(Value::Closure(closure));
                    (InitialBindingState::Value(value), None)
                }
            };
            let binding = self.add_binding(Binding {
                name: slot.name.clone(),
                state: LinkBindingState::Materialized { namespace, initial },
            });
            if let Some(bundle) = bundle {
                self.payload_bundles[bundle.index()].bindings.push(binding);
            }
            assert!(
                bindings.insert(slot.name, binding).is_none(),
                "duplicate namespace slot"
            );
        }
        let state = MaterializedNamespaceState {
            namespace_environment,
            imports_environment,
            payload,
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
        bindings: impl IntoIterator<Item = (BindingName, ExternalBindingAccess)>,
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
        class: ClassName,
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

    pub fn linked_generic_registrations(&self, namespace: NamespaceId) -> Vec<S3RegistrationId> {
        let owner = &self.namespaces[namespace.index()];
        owner
            .s3_registrations
            .iter()
            .copied()
            .filter(|&id| {
                let generic = &self.s3_registrations[id.index()].generic;
                match &generic.home {
                    GenericHome::Program(package) => {
                        matches!(self.packages[package], PackageIr::Linked { .. })
                    }
                    GenericHome::Optional(_) => false,
                    GenericHome::Lexical => {
                        !owner.bindings.contains_key(generic.name.as_str())
                            && self.visible_binding(namespace, &generic.name).is_some_and(
                                |binding| {
                                    matches!(
                                        self.namespaces[self.binding_namespace(binding).index()]
                                            .state,
                                        LinkNamespaceState::Linked(_)
                                    )
                                },
                            )
                    }
                }
            })
            .collect()
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

    pub fn attach_import(&mut self, namespace: NamespaceId, local: BindingName, target: BindingId) {
        self.namespaces[namespace.index()]
            .imports
            .push(ImportBindingIr { local, target });
    }

    pub fn payload_bundle(&self, namespace: NamespaceId) -> Option<PayloadBundleId> {
        match &self.namespaces[namespace.index()].state {
            LinkNamespaceState::Root(state) | LinkNamespaceState::Linked(state) => state.payload,
            LinkNamespaceState::External { .. } => None,
        }
    }

    pub fn add_payload_closure(
        &mut self,
        bundle: PayloadBundleId,
        home: ClosureHome,
        binding: BindingName,
        code: CodeIr,
    ) -> CodeId {
        let code = self.add_code(code);
        self.payload_bundles[bundle.index()]
            .closure_patches
            .push(PayloadClosurePatch {
                home,
                binding,
                code,
            });
        code
    }

    /// Record that `bundle` refers to `namespace`; a reference to the owner itself needs no
    /// activation and yields `None`.
    pub fn attach_payload_dependency(
        &mut self,
        bundle: PayloadBundleId,
        namespace: NamespaceId,
    ) -> Result<Option<PayloadDependency>, InvalidPayloadDependency> {
        let bundle = &mut self.payload_bundles[bundle.index()];
        if bundle.namespace == namespace {
            return Ok(None);
        }
        let dependency = match self.namespaces[namespace.index()].state {
            LinkNamespaceState::Root(_) => return Err(InvalidPayloadDependency::RootNamespace),
            LinkNamespaceState::Linked(_) => PayloadDependency::Linked(namespace),
            LinkNamespaceState::External { .. } => PayloadDependency::External(namespace),
        };
        bundle.dependencies.insert(dependency);
        Ok(Some(dependency))
    }

    pub fn add_resource(&mut self, resource: ResourceIr) -> ResourceId {
        let id = ResourceId::from_index(self.resources.len());
        self.resources.push(resource);
        id
    }

    pub fn relocate(
        &mut self,
        code: CodeId,
        range: TextRange,
        target: RelocationTarget,
    ) -> Result<(), InvalidRelocation> {
        let code_ir = &self.codes[code.index()];
        let original = code_ir
            .source
            .get(range.start..range.end)
            .ok_or(InvalidRelocation::OutsideCode)?;
        if code_ir
            .occurrences
            .iter()
            .any(|occurrence| occurrence.overlaps(range))
        {
            return Err(InvalidRelocation::Overlap);
        }
        if !self.relocation_matches(&target, original) {
            return Err(InvalidRelocation::Mismatch(original.to_owned()));
        }
        let occurrence = CodeOccurrenceId::from_index(code_ir.occurrences.len());
        self.codes[code.index()].occurrences.push(range);
        self.relocations.push(Relocation {
            site: CodeSite { code, occurrence },
            target,
        });
        Ok(())
    }

    fn relocation_matches(&self, target: &RelocationTarget, original: &str) -> bool {
        let callee = original
            .trim_start_matches("base::")
            .trim_start_matches("utils::")
            .trim_start_matches("rlang::");
        match target {
            RelocationTarget::Binding { target, .. } => {
                let unqualified = original
                    .rsplit(':')
                    .next()
                    .unwrap_or(original)
                    .trim_matches('`');
                self.bindings[target.index()].name == unqualified
            }
            RelocationTarget::RequireNamespace { .. } => callee.starts_with("requireNamespace("),
            RelocationTarget::Namespace { operation, .. } => callee.starts_with(operation.callee()),
            RelocationTarget::PackageVersion { .. } => callee.starts_with("packageVersion("),
            RelocationTarget::Resource { .. } => callee.starts_with("system.file("),
            RelocationTarget::InstalledQuery { check: true } => {
                callee.starts_with("check_installed(")
            }
            RelocationTarget::InstalledQuery { check: false } => {
                callee.starts_with("is_installed(")
            }
            RelocationTarget::LoadedQuery => {
                callee.starts_with("isNamespaceLoaded(") || original.contains("%in%")
            }
            RelocationTarget::NamespaceArgument { .. }
            | RelocationTarget::DescriptionArgument { .. }
            | RelocationTarget::NativeSymbol { .. }
            | RelocationTarget::NativeLibrary { .. } => original.starts_with(['"', '\'']),
        }
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

    pub fn binding_is_payload(&self, binding: BindingId) -> bool {
        let LinkBindingState::Materialized {
            initial: InitialBindingState::Value(value),
            ..
        } = &self.bindings[binding.index()].state
        else {
            return false;
        };
        matches!(self.values[value.index()], Value::Payload(_))
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

    pub fn set_root_artifact(&mut self, root_artifact: RootArtifactIr) {
        self.root_artifact = root_artifact;
    }

    pub fn finish(self) -> ProgramIr {
        ProgramIr {
            target: self.target,
            root_package: self.root_package,
            packages: self.packages,
            namespaces: self.namespaces,
            bindings: self.bindings,
            values: self.values,
            closures: self.closures,
            environments: self.environments,
            codes: self.codes,
            activations: self.activations,
            s3_registrations: self.s3_registrations,
            resources: self.resources,
            payload_bundles: self.payload_bundles,
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
}
