use crate::TargetEnvironment;
use crate::analysis::LinkIr;
use crate::ir::{
    BindingName, ClosureHome, GenericHome, ImportSlotIr, LinkBindingState, LinkNamespaceState,
    NamespaceId, ObjectStep, PayloadBundleId, PayloadBundleIr, PayloadDependency, ProgramIr,
    RegisteredNamespace, RemovedImportIr, ResourceId, Value,
};
use crate::package::PackageId;
use crate::r_worker::client::WorkerClient;
use crate::r_worker::protocol::{
    ClosurePatchSpec, NamespaceImageSpec, ObjectStepSpec, PackageSpec, PayloadSerialization,
    PayloadSite, PayloadSpec, SerializedPayload,
};
use crate::source::{FrozenSourceFiles, SourcePackageSnapshot, StagedRoot};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::marker::PhantomData;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

mod relocated;

use relocated::RelocatedCode;
use thiserror::Error;

macro_rules! emit {
    ($out:expr, $($argument:tt)*) => {{
        $out.push_str(&format!($($argument)*));
        $out.push('\n');
    }};
}

/// Selected target-R physical handle available to staging and materialization.
#[derive(Debug)]
pub struct TargetRuntimeHandle {
    r_home: PathBuf,
    target: TargetEnvironment,
}

impl TargetRuntimeHandle {
    pub fn new(r_home: PathBuf, target: TargetEnvironment) -> Self {
        Self { r_home, target }
    }

    pub fn r_home(&self) -> &Path {
        &self.r_home
    }

    pub fn target(&self) -> &TargetEnvironment {
        &self.target
    }

    pub(crate) fn worker(&self) -> crate::Result<WorkerClient> {
        WorkerClient::spawn(self.r_home.clone(), &self.target)
    }
}

/// Frozen physical inputs used to orchestrate preflight, never passed wholesale to materialization.
#[derive(Debug)]
pub struct BuildContext {
    source: SourcePackageSnapshot,
    _staged_root: StagedRoot,
    target_runtime: TargetRuntimeHandle,
}

/// Exact installed bytes redeemed from the selected images before preflight.
#[derive(Debug)]
struct FrozenInputs {
    bundles: Vec<CheckedPayloadBundle>,
    resources: BTreeMap<ResourceId, PathBuf>,
    code: RelocatedCode,
    _directory: TempDir,
}

/// One payload bundle serialized by the target R whose namespace references match its IR
/// dependencies and whose reference objects are reached from no other bundle.
#[derive(Debug)]
struct CheckedPayloadBundle {
    bundle: PayloadBundleId,
    bytes: Vec<u8>,
}

impl BuildContext {
    pub fn new(
        source: SourcePackageSnapshot,
        staged_root: StagedRoot,
        r_home: PathBuf,
        target: TargetEnvironment,
    ) -> Self {
        Self {
            source,
            _staged_root: staged_root,
            target_runtime: TargetRuntimeHandle::new(r_home, target),
        }
    }

    /// Redeem every physical input `ProgramIr` needs, then prove that no selected image changed
    /// since analysis fingerprinted it.
    ///
    /// # Errors
    ///
    /// Blocks when a serialized payload bundle diverges from its IR entity. Fails when payload
    /// serialization or resource copying fails, or with
    /// [`BuildContextError::TargetUniverseChanged`] when a selected image no longer matches.
    fn freeze(&self, ir: &LinkIr) -> Result<FrozenInputs, PreflightError> {
        let program = ir.program();
        let sources = ir.package_sources();
        let location = |package| {
            &sources
                .get(package)
                .expect("finalized package has a frozen physical source")
                .1
                .root
        };

        let spec = |package| {
            let identity = program.package(package).identity();
            PackageSpec {
                name: identity.name.to_string(),
                version: identity.version.to_string(),
                image_fingerprint: identity.image_fingerprint.0.clone(),
                root: location(package).clone(),
            }
        };
        let mut worker = self.target_runtime.worker()?;
        let code = RelocatedCode::verify(program, &mut worker)?;
        let mut bundles = Vec::new();
        if !program.payload_bundles().is_empty() {
            let namespaces = program
                .packages()
                .filter(|(_, package)| package.role() != crate::ir::PackageRole::External)
                .map(|(id, package)| NamespaceImageSpec {
                    package: spec(id),
                    registered_name: package.registered_namespace().as_str().to_owned(),
                })
                .collect();
            let specs = program
                .payload_bundles()
                .iter()
                .map(|bundle| PayloadSpec {
                    package: spec(program.namespace(bundle.namespace()).package),
                    names: bundle
                        .bindings()
                        .iter()
                        .map(|binding| program.binding(*binding).name.to_string())
                        .collect(),
                    patches: closure_patches(program, bundle, &code),
                })
                .collect();
            let serialized = worker.serialize_payloads(namespaces, specs)?;
            bundles = check_payload_bundles(program, serialized)?;
        }

        let directory = tempfile::Builder::new()
            .prefix("slinker-frozen-")
            .tempdir()?;
        let mut resources = BTreeMap::new();
        for (ordinal, (id, resource)) in program.indexed_resources().enumerate() {
            let frozen = directory.path().join(ordinal.to_string());
            copy_entry(&location(resource.package).join(&resource.path), &frozen)?;
            resources.insert(id, frozen);
        }

        if let Some(changed) = sources.changed()? {
            return Err(BuildContextError::TargetUniverseChanged(changed.name.to_string()).into());
        }
        Ok(FrozenInputs {
            bundles,
            code,
            resources,
            _directory: directory,
        })
    }

    pub fn source(&self) -> &SourcePackageSnapshot {
        &self.source
    }

    fn materialization<'a>(&'a self, frozen: &'a FrozenInputs) -> MaterializationContext<'a> {
        MaterializationContext {
            source_files: self.source.files(),
            target_runtime: &self.target_runtime,
            frozen,
        }
    }
}

#[derive(Debug, Error)]
pub enum BuildContextError {
    #[error("failed to freeze package input: {0}")]
    Package(#[from] crate::Error),
    #[error("failed to freeze package input: {0}")]
    Io(#[from] std::io::Error),
    #[error("selected package image changed during the invocation: {0}")]
    TargetUniverseChanged(String),
    #[error("emitted code failed target-R verification: {0}")]
    InvalidCode(String),
}

/// Narrow physical view available only after successful preflight.
#[derive(Clone, Copy)]
pub struct MaterializationContext<'a> {
    source_files: &'a FrozenSourceFiles,
    target_runtime: &'a TargetRuntimeHandle,
    frozen: &'a FrozenInputs,
}

impl MaterializationContext<'_> {
    fn source_files(&self) -> &FrozenSourceFiles {
        self.source_files
    }

    fn target_runtime(&self) -> &TargetRuntimeHandle {
        self.target_runtime
    }

    fn bundles(&self) -> &[CheckedPayloadBundle] {
        &self.frozen.bundles
    }

    fn resource(&self, resource: ResourceId) -> &Path {
        &self.frozen.resources[&resource]
    }

    fn code(&self) -> &RelocatedCode {
        &self.frozen.code
    }
}

/// First exact source-package materialization profile.
pub enum PureRStatic {}

/// Opaque capability proving full preflight succeeded for one profile.
pub struct BuildableProgram<'a, Profile> {
    program: &'a ProgramIr,
    description: &'a str,
    context: &'a BuildContext,
    frozen: FrozenInputs,
    _profile: PhantomData<Profile>,
}

impl PureRStatic {
    /// Check every analysis blocker and profile capability, then freeze the physical inputs of an
    /// eligible program.
    ///
    /// # Errors
    ///
    /// Returns one deterministic report of every blocker, or the failure to freeze inputs.
    pub fn check<'a>(
        ir: &'a LinkIr,
        context: &'a BuildContext,
    ) -> Result<BuildableProgram<'a, PureRStatic>, PreflightError> {
        let mut blockers = ir
            .blockers()
            .iter()
            .map(|blocker| match &blocker.binding {
                Some(binding) => format!(
                    "{:?} in {}::{binding}: {}",
                    blocker.code, blocker.package, blocker.message
                ),
                None => format!(
                    "{:?} in {}: {}",
                    blocker.code, blocker.package, blocker.message
                ),
            })
            .collect::<BTreeSet<_>>();
        let description = ir.program().root_artifact().description.as_deref();
        if description.is_none() {
            blockers.insert("Root source-package DESCRIPTION plan is missing".into());
        }
        let program = ir.program();
        for import in program.root_artifact().load.before_bootstrap().imports() {
            let target = program.binding(import.target);
            if target.name != import.local {
                blockers.insert(format!(
                    "Root imports External `{}` as `{}`, which NAMESPACE cannot express",
                    target.name, import.local
                ));
            }
        }
        let Some(description) = description.filter(|_| blockers.is_empty()) else {
            return Err(BuildReport::new(blockers.into_iter().collect()).into());
        };
        let frozen = context.freeze(ir)?;
        Ok(BuildableProgram {
            program: ir.program(),
            description,
            context,
            frozen,
            _profile: PhantomData,
        })
    }
}

#[derive(Debug, Error)]
pub enum PreflightError {
    #[error(transparent)]
    Blocked(#[from] BuildReport),
    #[error(transparent)]
    Freeze(#[from] BuildContextError),
}

impl From<crate::Error> for PreflightError {
    fn from(error: crate::Error) -> Self {
        Self::Freeze(error.into())
    }
}

impl From<std::io::Error> for PreflightError {
    fn from(error: std::io::Error) -> Self {
        Self::Freeze(error.into())
    }
}

/// Deterministic complete build-preflight failure report.
#[derive(Clone, Debug, Error)]
#[error("build preflight failed:\n{rendered}")]
pub struct BuildReport {
    blockers: Vec<String>,
    rendered: String,
}

impl BuildReport {
    fn new(blockers: Vec<String>) -> Self {
        let rendered = blockers
            .iter()
            .map(|blocker| format!("  - {blocker}"))
            .collect::<Vec<_>>()
            .join("\n");
        Self { blockers, rendered }
    }

    pub fn blockers(&self) -> &[String] {
        &self.blockers
    }
}

/// Completed generated source-package artifact.
#[derive(Debug)]
pub struct GeneratedPackage {
    path: PathBuf,
}

impl GeneratedPackage {
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Materialize a preflight-approved ProgramIr into a generated R source package.
#[expect(
    clippy::needless_pass_by_value,
    reason = "consuming the preflight capability makes each approved program materialize once"
)]
pub fn materialize(
    buildable: BuildableProgram<'_, PureRStatic>,
    output: &Path,
) -> Result<GeneratedPackage, MaterializeError> {
    if output.exists() {
        return Err(MaterializeError::OutputExists(output.into()));
    }
    let parent = output
        .parent()
        .ok_or_else(|| MaterializeError::InvalidOutput(output.into()))?;
    fs::create_dir_all(parent)?;
    let materialization = buildable.context.materialization(&buildable.frozen);
    let temporary = tempfile::Builder::new()
        .prefix(".slinker-materialize-")
        .tempdir_in(parent)?;
    let package_name = &buildable
        .program
        .package(buildable.program.root_package())
        .identity()
        .name;
    let package_root = temporary.path().join(package_name.as_str());
    fs::create_dir_all(package_root.join("R"))?;
    fs::create_dir_all(package_root.join("inst/slinker"))?;
    fs::write(
        package_root.join("DESCRIPTION"),
        buildable.description.as_bytes(),
    )?;
    fs::write(
        package_root.join("NAMESPACE"),
        render_namespace(buildable.program),
    )?;
    copy_root_resources(materialization.source_files().root(), &package_root)?;
    let payload_directory = package_root.join("inst/slinker/payload");
    fs::create_dir_all(&payload_directory)?;
    for checked in materialization.bundles() {
        let bundle = buildable.program.payload_bundle(checked.bundle);
        let package = buildable.program.namespace(bundle.namespace()).package;
        fs::write(
            payload_directory.join(format!(
                "{}.rds",
                buildable.program.package(package).identity().name
            )),
            &checked.bytes,
        )?;
    }
    let generated = generate_r_source(buildable.program, materialization.code())?;
    let mut worker = materialization.target_runtime().worker()?;
    validate_r_source(&mut worker, &generated)?;
    fs::write(package_root.join("R/zzz-slinker-generated.R"), generated)?;
    copy_linked_resources(buildable.program, materialization, &package_root)?;
    fs::rename(&package_root, output)?;
    Ok(GeneratedPackage {
        path: output.to_path_buf(),
    })
}

#[derive(Debug, Error)]
pub enum MaterializeError {
    #[error("generated output already exists: {0}")]
    OutputExists(PathBuf),
    #[error("invalid generated output path: {0}")]
    InvalidOutput(PathBuf),
    #[error("generated R source failed target-R validation: {0}")]
    InvalidR(String),
    #[error("target-R worker failed during materialization: {0}")]
    Worker(#[from] crate::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

fn generate_r_source(
    program: &ProgramIr,
    code: &RelocatedCode,
) -> Result<String, MaterializeError> {
    let mut out = String::new();
    out.push_str(
        ".slinker_runtime <- base::new.env(parent = base::baseenv())\nbase::local(envir = .slinker_runtime, {\n",
    );
    out.push_str(GENERATED_RUNTIME);
    out.push('\n');
    emit!(
        out,
        ".slinker_target <- c(version = {}, platform = {}, arch = {})",
        r_string(&program.target().r_version),
        r_string(&program.target().platform),
        r_string(&program.target().arch)
    );
    emit!(
        out,
        ".slinker_root_package <- {}",
        r_string(&program.package(program.root_package()).identity().name)
    );
    let root = program.root_namespace();
    let root_on_load = program.root_artifact().on_load;
    let mut root_code = String::new();
    for closure in namespace_closures(program, root) {
        let source = code.source(program.closure(closure).code);
        if Some(closure) == root_on_load {
            let value_start = program
                .code(program.closure(closure).code)
                .assigned_value_start()
                .ok_or_else(|| {
                    MaterializeError::InvalidR("Root .onLoad code is not an assignment".into())
                })?;
            root_code.push_str(".slinker_original_on_load <- ");
            root_code.push_str(&source[value_start..]);
        } else {
            root_code.push_str(source);
        }
        root_code.push('\n');
    }

    out.push_str("bootstrap <- function(root, libname, pkgname) {\n  .slinker_check_target()\n");
    out.push_str("  on.exit(.slinker_unregister(), add = TRUE)\n");
    for (local, removed) in program.root_artifact().load.removed_imports() {
        emit!(
            out,
            "  .slinker_stub(parent.env(root), {})",
            removed_import(local, removed)
        );
    }
    for activation in program.activations() {
        let package = program.package(program.namespace(activation.namespace).package);
        let identity = package.identity();
        emit!(
            out,
            "  namespaces[[{key}]] <- .slinker_new_namespace({key}, {}, {})",
            r_string(&identity.name),
            r_string(identity.version.as_ref()),
            key = r_string(package.registered_namespace().as_str()),
        );
    }
    for activation in program.activations() {
        let namespace = program.namespace(activation.namespace);
        let package = program.package(namespace.package);
        let name = &package.identity().name;
        emit!(
            out,
            "  local({{\n    ns <- namespaces[[{}]]\n    imports <- parent.env(ns)",
            r_string(package.registered_namespace().as_str())
        );
        for native in &activation.native_components {
            let Some(library) = native.library.path() else {
                continue;
            };
            let symbols = native
                .bindings()
                .map(|symbol| {
                    format!(
                        "{} = {}",
                        r_string(&symbol.binding),
                        r_string(&symbol.symbol)
                    )
                })
                .collect::<Vec<_>>()
                .join(", ");
            emit!(
                out,
                "    .slinker_load_native(ns, {}, {}, {}, {}, c({symbols}))",
                r_string(name),
                r_string(&native.name),
                r_string(&native.alias),
                r_string(library)
            );
        }
        emit!(
            out,
            "    setNamespaceInfo(ns, \"imports\", {})",
            imports_info(namespace)
        );
        for (local, slot) in &namespace.imports {
            match slot {
                ImportSlotIr::Bound(target) => emit!(
                    out,
                    "    assign({}, {}, envir = imports)",
                    r_string(local),
                    binding_reference(program, *target)
                ),
                ImportSlotIr::Removed(removed) => emit!(
                    out,
                    "    .slinker_stub(imports, {})",
                    removed_import(local, removed)
                ),
            }
        }
        for closure in namespace_closures(program, namespace) {
            let source = code.source(program.closure(closure).code);
            emit!(
                out,
                "    eval(parse(text = {}), envir = ns)",
                r_string(source)
            );
        }
        if let Some(bundle) = payload_bundle(program, namespace) {
            emit!(
                out,
                "    .slinker_populate(ns, {}, {})",
                r_string(name),
                external_payload_dependencies(program, bundle)
            );
        }
        emit!(
            out,
            "    .slinker_activate(ns, {}, {}, {}, {}, {})\n  }})",
            r_vector(activation.exports.names().iter().map(BindingName::as_str)),
            s3_matrix(program, &namespace.s3_registrations, S3Column4::Registry),
            s3_matrix(program, &namespace.s3_registrations, S3Column4::Original),
            r_vector(activation.removed_bindings.iter().map(BindingName::as_str)),
            if activation.on_load.is_some() {
                "TRUE"
            } else {
                "FALSE"
            }
        );
    }
    let after_activation = program.root_artifact().load.after_activation();
    for import in after_activation.imports() {
        emit!(
            out,
            "  assign({}, {}, envir = parent.env(root))",
            r_string(&import.local),
            binding_reference(program, import.target)
        );
    }
    if let Some(bundle) = payload_bundle(program, root) {
        emit!(
            out,
            "  .slinker_populate(root, {}, {})",
            r_string(&program.package(root.package).identity().name),
            external_payload_dependencies(program, bundle)
        );
    }
    let activated_s3 = after_activation.s3_registrations();
    if !activated_s3.is_empty() {
        emit!(
            out,
            "  .slinker_register_s3(root, {}, {})",
            s3_matrix(program, activated_s3, S3Column4::Registry),
            s3_matrix(program, activated_s3, S3Column4::Original)
        );
    }
    out.push_str("  .slinker_unregister()\n");
    if root_on_load.is_some() {
        out.push_str(
            "  get(\".slinker_original_on_load\", envir = root, inherits = FALSE)(libname, pkgname)\n",
        );
    }
    out.push_str("}\n})\n");
    out.push_str(&root_code);
    out.push_str(
        ".onLoad <- function(libname, pkgname) {\n  .slinker_runtime[[\"bootstrap\"]](base::asNamespace(pkgname), libname, pkgname)\n}\n",
    );
    Ok(out)
}

fn namespace_closures(
    program: &ProgramIr,
    namespace: &crate::ir::Namespace,
) -> impl Iterator<Item = crate::ir::ClosureId> {
    namespace.bindings.values().filter_map(|binding| {
        match initial_value(program, *binding).map(|value| program.value(value)) {
            Some(Value::Closure(closure)) => Some(*closure),
            _ => None,
        }
    })
}

fn payload_bundle<'a>(
    program: &'a ProgramIr,
    namespace: &crate::ir::Namespace,
) -> Option<&'a PayloadBundleIr> {
    match &namespace.state {
        LinkNamespaceState::Root(state) | LinkNamespaceState::Linked(state) => {
            state.payload.map(|bundle| program.payload_bundle(bundle))
        }
        LinkNamespaceState::External { .. } => None,
    }
}

fn external_payload_dependencies(program: &ProgramIr, bundle: &PayloadBundleIr) -> String {
    r_vector(bundle.dependencies().iter().filter_map(|dependency| {
        match dependency {
            PayloadDependency::External(namespace) => Some(
                program
                    .package(program.namespace(*namespace).package)
                    .identity()
                    .name
                    .as_str(),
            ),
            PayloadDependency::Linked(_) => None,
        }
    }))
}

fn registered_name(program: &ProgramIr, namespace: NamespaceId) -> &str {
    program
        .package(program.namespace(namespace).package)
        .registered_namespace()
        .as_str()
}

/// Accept the target-R serialization of every IR payload bundle only when no reference object
/// is shared between bundles and each bundle resolves exactly the namespaces its IR entity
/// depends on.
fn check_payload_bundles(
    program: &ProgramIr,
    serialization: PayloadSerialization,
) -> Result<Vec<CheckedPayloadBundle>, BuildReport> {
    let bundles = program.payload_bundles();
    let package_name = |bundle: &PayloadBundleIr| {
        &program
            .package(program.namespace(bundle.namespace()).package)
            .identity()
            .name
    };
    let site =
        |site: &PayloadSite| format!("{}::{}", package_name(&bundles[site.payload]), site.binding);
    let serialized = match serialization {
        PayloadSerialization::SharedIdentity { first, second } => {
            return Err(BuildReport::new(vec![format!(
                "payload `{}` and payload `{}` reach one environment or reference object, which separate namespace bundles would split into two",
                site(&first),
                site(&second)
            )]));
        }
        PayloadSerialization::Serialized { bundles } => bundles,
    };
    let mut blockers = Vec::new();
    let mut checked = Vec::new();
    for ((id, bundle), SerializedPayload { bytes, namespaces }) in
        program.indexed_payload_bundles().zip(serialized)
    {
        let owner = registered_name(program, bundle.namespace());
        let package = package_name(bundle);
        let established = bundle
            .dependencies()
            .iter()
            .map(|dependency| registered_name(program, dependency.namespace()))
            .collect::<BTreeSet<_>>();
        let observed = namespaces
            .iter()
            .map(String::as_str)
            .filter(|namespace| *namespace != owner)
            .collect::<BTreeSet<_>>();
        blockers.extend(observed.difference(&established).map(|namespace| {
            format!(
                "payload bundle of `{package}` refers to namespace `{namespace}`, which analysis did not establish as a dependency"
            )
        }));
        blockers.extend(established.difference(&observed).map(|namespace| {
            format!(
                "payload bundle of `{package}` does not refer to namespace `{namespace}`, which analysis established as a dependency"
            )
        }));
        checked.push(CheckedPayloadBundle { bundle: id, bytes });
    }
    if blockers.is_empty() {
        Ok(checked)
    } else {
        Err(BuildReport::new(blockers))
    }
}

fn initial_value(program: &ProgramIr, binding: crate::ir::BindingId) -> Option<crate::ir::ValueId> {
    match &program.binding(binding).state {
        LinkBindingState::Materialized {
            initial: crate::ir::InitialBindingState::Value(value),
            ..
        } => Some(*value),
        LinkBindingState::Materialized { .. } | LinkBindingState::External { .. } => None,
    }
}

fn binding_reference(program: &ProgramIr, binding: crate::ir::BindingId) -> String {
    let namespace = program.namespace(program.binding_namespace(binding));
    let package = program.package(namespace.package);
    let exported_external = matches!(
        program.binding(binding).state,
        LinkBindingState::External {
            access: crate::ir::ExternalBindingAccess::Exported,
            ..
        }
    );
    if exported_external {
        format!(
            "base::getExportedValue({}, {})",
            r_string(&package.identity().name),
            r_string(&program.binding(binding).name)
        )
    } else {
        namespace_get(program, binding)
    }
}

fn namespace_get(program: &ProgramIr, binding: crate::ir::BindingId) -> String {
    format!(
        "base::get({}, envir = {}, inherits = FALSE)",
        r_string(&program.binding(binding).name),
        namespace_expression(
            program,
            program
                .namespace(program.binding_namespace(binding))
                .package
        )
    )
}

fn namespace_expression(program: &ProgramIr, package: PackageId) -> String {
    let root = program.package(program.root_package());
    match program.package(package).registered_namespace() {
        RegisteredNamespace::Package(name) => format!("base::asNamespace({})", r_string(name)),
        RegisteredNamespace::Private(key) => format!(
            "base::asNamespace({})[[\".slinker_runtime\"]][[\"namespaces\"]][[{}]]",
            r_string(&root.identity().name),
            r_string(key.as_str())
        ),
    }
}

#[derive(Clone, Copy)]
enum S3Column4 {
    Registry,
    Original,
}

fn s3_matrix(
    program: &ProgramIr,
    registrations: &[crate::ir::S3RegistrationId],
    column4: S3Column4,
) -> String {
    let rows = registrations
        .iter()
        .map(|registration| program.s3_registration(*registration))
        .collect::<Vec<_>>();
    let cells = rows
        .iter()
        .map(|row| r_string(&row.generic.name))
        .chain(rows.iter().map(|row| r_string(&row.class)))
        .chain(
            rows.iter()
                .map(|row| r_string(&program.binding(row.method).name)),
        )
        .chain(rows.iter().map(|row| match &row.generic.home {
            GenericHome::Lexical => "NA_character_".to_owned(),
            GenericHome::Program(package) => match column4 {
                S3Column4::Registry => {
                    r_string(program.package(*package).registered_namespace().as_str())
                }
                S3Column4::Original => r_string(&program.package(*package).identity().name),
            },
            GenericHome::Optional(package) => r_string(package),
        }))
        .collect::<Vec<_>>();
    format!("matrix(as.character(c({})), ncol = 4L)", cells.join(", "))
}

fn imports_info(namespace: &crate::ir::Namespace) -> String {
    let records = namespace.import_records.iter().map(|record| {
        let locals = record
            .names
            .iter()
            .map(|(local, _)| local)
            .map(|local| r_string(local))
            .collect::<Vec<_>>()
            .join(", ");
        let remotes = record
            .names
            .iter()
            .map(|(_, remote)| remote)
            .map(|remote| r_string(remote))
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            "{} = structure(as.character(c({remotes})), names = as.character(c({locals})))",
            r_string(&record.package)
        )
    });
    let entries = std::iter::once("base = TRUE".to_owned())
        .chain(records)
        .collect::<Vec<_>>();
    format!("list({})", entries.join(", "))
}

fn r_vector<'a>(values: impl Iterator<Item = &'a str>) -> String {
    format!(
        "as.character(c({}))",
        values.map(r_string).collect::<Vec<_>>().join(", ")
    )
}

fn render_namespace(program: &ProgramIr) -> String {
    let before_bootstrap = program.root_artifact().load.before_bootstrap();
    let mut out = String::new();
    for name in program.root_artifact().exports.names() {
        emit!(out, "export({})", r_string(name));
    }
    for import in before_bootstrap.imports() {
        let package = program
            .namespace(program.binding_namespace(import.target))
            .package;
        emit!(
            out,
            "importFrom({}, {})",
            r_string(&program.package(package).identity().name),
            r_string(&program.binding(import.target).name)
        );
    }
    for native in &program.root_artifact().native_components {
        let registration = native.registration.iter().map(|fixes| {
            format!(
                ".registration = TRUE, .fixes = c({}, {})",
                r_string(&fixes.prefix),
                r_string(&fixes.suffix)
            )
        });
        let symbols = native.symbols.iter().map(|symbol| {
            format!(
                "{} = {}",
                r_binding_name(&symbol.binding),
                r_string(&symbol.symbol)
            )
        });
        emit!(
            out,
            "useDynLib({})",
            std::iter::once(r_binding_name(&native.name))
                .chain(registration)
                .chain(symbols)
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    for registration in before_bootstrap.s3_registrations() {
        let registration = program.s3_registration(*registration);
        let generic = match &registration.generic.home {
            GenericHome::Program(package) => format!(
                "{}::{}",
                r_binding_name(&program.package(*package).identity().name),
                r_binding_name(&registration.generic.name)
            ),
            GenericHome::Optional(package) => format!(
                "{}::{}",
                r_binding_name(package),
                r_binding_name(&registration.generic.name)
            ),
            GenericHome::Lexical => r_string(&registration.generic.name),
        };
        emit!(
            out,
            "S3method({generic}, {}, {})",
            r_string(&registration.class),
            r_string(&program.binding(registration.method).name)
        );
    }
    out
}

fn closure_patches(
    program: &ProgramIr,
    bundle: &PayloadBundleIr,
    relocated: &RelocatedCode,
) -> Vec<ClosurePatchSpec> {
    bundle
        .closure_patches()
        .iter()
        .map(|closure| {
            let code = program.code(closure.code);
            let source = relocated.source(closure.code);
            let (root, steps) = match &closure.home {
                ClosureHome::Namespace => (None, Vec::new()),
                ClosureHome::Reached { root, steps } => (
                    Some(root.to_string()),
                    steps
                        .iter()
                        .map(|step| match step {
                            ObjectStep::Environment => ObjectStepSpec::Environment,
                            ObjectStep::Parent => ObjectStepSpec::Parent,
                            ObjectStep::Binding(name) => ObjectStepSpec::Binding(name.to_string()),
                        })
                        .collect(),
                ),
            };
            ClosurePatchSpec {
                root,
                steps,
                binding: closure.binding.to_string(),
                expected_shape: code.normalized_shape().0.clone(),
                source: code
                    .assigned_value_start()
                    .map_or(source, |start| &source[start..])
                    .to_owned(),
            }
        })
        .collect()
}

fn native_library(program: &ProgramIr, package: PackageId, component: &str) -> String {
    format!(
        "base::getNamespaceInfo({}, \"DLLs\")[[{}]]",
        namespace_expression(program, package),
        r_string(component)
    )
}

fn validate_r_source(worker: &mut WorkerClient, source: &str) -> Result<(), MaterializeError> {
    let normalized = worker.normalize_syntax(source)?;
    let normalized_again = worker.normalize_syntax(&normalized)?;
    if normalized == normalized_again {
        Ok(())
    } else {
        Err(MaterializeError::InvalidR(
            "target-R parse/deparse normalization is not stable".into(),
        ))
    }
}

fn copy_root_resources(source: &Path, output: &Path) -> Result<(), std::io::Error> {
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let name = entry.file_name();
        if matches!(
            name.to_str(),
            Some("DESCRIPTION" | "NAMESPACE" | "MD5" | "R" | "target" | ".git")
        ) {
            continue;
        }
        copy_entry(&entry.path(), &output.join(name))?;
    }
    Ok(())
}

fn copy_linked_resources(
    program: &ProgramIr,
    context: MaterializationContext<'_>,
    output: &Path,
) -> Result<(), std::io::Error> {
    for (id, resource) in program.indexed_resources() {
        let package = program.package(resource.package).identity();
        let source = context.resource(id);
        let target = output
            .join("inst/slinker/resources")
            .join(package.name.as_str())
            .join(&resource.path);
        copy_entry(source, &target)?;
    }
    Ok(())
}

fn copy_entry(source: &Path, target: &Path) -> Result<(), std::io::Error> {
    if source.is_dir() {
        fs::create_dir_all(target)?;
        for entry in fs::read_dir(source)? {
            let entry = entry?;
            copy_entry(&entry.path(), &target.join(entry.file_name()))?;
        }
    } else {
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(source, target)?;
    }
    Ok(())
}

fn removed_import(local: &str, removed: &RemovedImportIr) -> String {
    format!(
        "{}, {}, {}",
        r_string(local),
        r_string(&removed.package),
        r_string(&removed.binding)
    )
}

fn r_string(value: &str) -> String {
    format!(
        "\"{}\"",
        value
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('\n', "\\n")
            .replace('\r', "\\r")
    )
}

fn r_binding_name(value: &str) -> String {
    let simple = !value.is_empty()
        && value.bytes().enumerate().all(|(index, byte)| {
            if index == 0 {
                byte.is_ascii_alphabetic() || byte == b'.'
            } else {
                byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_')
            }
        })
        && !(value.starts_with('.') && value.as_bytes().get(1).is_some_and(u8::is_ascii_digit));
    if simple {
        value.into()
    } else {
        format!("`{}`", value.replace('`', "\\`"))
    }
}

const GENERATED_RUNTIME: &str = r#"
namespaces <- new.env(hash = TRUE, parent = emptyenv())
.slinker_unregister <- function() {
  for (key in names(namespaces)) {
    if (identical(.Internal(getRegisteredNamespace(key)), namespaces[[key]])) .Internal(unregisterNamespace(key))
  }
}
.slinker_check_target <- function() {
  actual <- c(
    version = paste0(R.version$major, ".", R.version$minor),
    platform = R.version$os,
    arch = R.version$arch
  )
  if (!identical(unname(actual), unname(.slinker_target))) {
    stop(sprintf("slinker target mismatch: expected %s, got %s", paste(.slinker_target, collapse = "/"), paste(actual, collapse = "/")), call. = FALSE)
  }
}
.slinker_new_namespace <- function(key, name, version) {
  imports <- new.env(parent = .BaseNamespaceEnv, hash = TRUE)
  attr(imports, "name") <- paste0("imports:", name)
  namespace <- new.env(parent = imports, hash = TRUE)
  info <- new.env(hash = TRUE, parent = baseenv())
  namespace$.__NAMESPACE__. <- info
  namespace$.packageName <- name
  info$spec <- c(name = name, version = version)
  setNamespaceInfo(namespace, "exports", new.env(hash = TRUE, parent = baseenv()))
  setNamespaceInfo(namespace, "imports", list(base = TRUE))
  setNamespaceInfo(namespace, "path", "")
  setNamespaceInfo(namespace, "dynlibs", character())
  setNamespaceInfo(namespace, "DLLs", list())
  setNamespaceInfo(namespace, "S3methods", matrix(NA_character_, 0L, 4L))
  namespace$.__S3MethodsTable__. <- new.env(hash = TRUE, parent = baseenv())
  .Internal(registerNamespace(key, namespace))
  namespace
}
.slinker_load_native <- function(namespace, package, component, alias, library, symbols) {
  path <- system.file("slinker", "resources", package, library, package = .slinker_root_package, mustWork = TRUE)
  dll <- dyn.load(path, local = TRUE)
  dlls <- getNamespaceInfo(namespace, "DLLs")
  dlls[[component]] <- dll
  setNamespaceInfo(namespace, "DLLs", dlls)
  setNamespaceInfo(namespace, "dynlibs", c(getNamespaceInfo(namespace, "dynlibs"), structure(component, names = alias)))
  for (binding in names(symbols)) {
    assign(binding, getNativeSymbolInfo(symbols[[binding]], dll), envir = namespace)
  }
}
.slinker_populate <- function(namespace, package, external) {
  for (dependency in external) loadNamespace(dependency)
  bundle <- system.file("slinker", "payload", paste0(package, ".rds"), package = .slinker_root_package, mustWork = TRUE)
  invisible(list2env(readRDS(bundle), envir = namespace))
}
.slinker_stub <- function(envir, name, package, binding) {
  makeActiveBinding(name, function(value) {
    stop(sprintf("`%s::%s` was removed by slinker because the build never reached it", package, binding), call. = FALSE)
  }, envir)
}
.slinker_register_s3 <- function(namespace, s3, s3_info) {
  previous <- getNamespaceInfo(namespace, "S3methods")
  registerS3methods(s3, unname(getNamespaceName(namespace)), namespace)
  setNamespaceInfo(namespace, "S3methods", rbind(s3_info, previous))
}
.slinker_activate <- function(namespace, exports, s3, s3_info, removed, on_load) {
  name <- unname(getNamespaceName(namespace))
  if (nrow(s3)) .slinker_register_s3(namespace, s3, s3_info)
  if (on_load) get(".onLoad", envir = namespace, inherits = FALSE)("", name)
  for (binding in removed[!vapply(removed, exists, logical(1), envir = namespace, inherits = FALSE)]) {
    .slinker_stub(namespace, binding, name, binding)
  }
  if (length(exports)) namespaceExport(namespace, exports)
  lockEnvironment(namespace, TRUE)
  lockEnvironment(parent.env(namespace), TRUE)
  invisible(namespace)
}
"#;
