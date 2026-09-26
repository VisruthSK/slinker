use crate::TargetEnvironment;
use crate::analysis::LinkIr;
use crate::ir::{
    LinkBindingState, LinkNamespaceState, PackageOperationIr, ProgramIr, Relocation, ResourceId,
    Value,
};
use crate::package::{BindingName, PackageId};
use crate::r_worker::client::WorkerClient;
use crate::r_worker::protocol::PackageSpec;
use crate::source::{FrozenSourceFiles, SourcePackageSnapshot, StagedRoot};
use crate::syntax::Span;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::fs;
use std::marker::PhantomData;
use std::path::{Path, PathBuf};
use tempfile::TempDir;
use thiserror::Error;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PackageOperation {
    RequireNamespace { result: bool },
    LoadNamespace,
    GetNamespace,
    AsNamespace,
    PackageVersion { version: String },
}

#[derive(Clone, Debug)]
pub(crate) enum PendingRelocation {
    NamespaceAccess {
        source: Span,
        package: PackageId,
        binding: BindingName,
        internal: bool,
    },
    ResourceAccess {
        source: Span,
        package: PackageId,
        resource: String,
    },
    PackageOperation {
        source: Span,
        package: Option<PackageId>,
        operation: PackageOperation,
    },
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
    frozen: Option<FrozenInputs>,
}

/// Exact installed bytes redeemed from the selected images before preflight.
#[derive(Debug)]
struct FrozenInputs {
    bundles: BTreeMap<PackageId, Vec<u8>>,
    resources: BTreeMap<ResourceId, PathBuf>,
    _directory: TempDir,
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
            frozen: None,
        }
    }

    /// Redeem every physical input `ProgramIr` needs, then prove that no selected image changed
    /// since analysis fingerprinted it.
    ///
    /// # Errors
    ///
    /// Fails when payload serialization or resource copying fails, or with
    /// [`BuildContextError::TargetUniverseChanged`] when a selected image no longer matches.
    fn freeze(&mut self, ir: &LinkIr) -> Result<(), BuildContextError> {
        let program = ir.program();
        let sources = ir.package_sources();
        let location = |package| {
            &sources
                .get(package)
                .expect("finalized package has a frozen physical source")
                .1
                .root
        };

        let mut payloads = BTreeMap::<PackageId, BTreeSet<String>>::new();
        for value in program.values() {
            if let Value::Payload(payload) = value {
                payloads
                    .entry(payload.package)
                    .or_default()
                    .insert(payload.binding.clone());
            }
        }
        let mut bundles = BTreeMap::new();
        if !payloads.is_empty() {
            let mut worker = self.target_runtime.worker()?;
            for (package, names) in payloads {
                let identity = program.package(package).identity();
                let spec = PackageSpec {
                    name: identity.name.clone(),
                    version: identity.version.to_string(),
                    image_fingerprint: identity.image_fingerprint.0.clone(),
                    root: location(package).clone(),
                };
                bundles.insert(
                    package,
                    worker.serialize_bundle(spec, names.into_iter().collect())?,
                );
            }
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
            return Err(BuildContextError::TargetUniverseChanged(
                changed.name.clone(),
            ));
        }
        self.frozen = Some(FrozenInputs {
            bundles,
            resources,
            _directory: directory,
        });
        Ok(())
    }

    pub fn source(&self) -> &SourcePackageSnapshot {
        &self.source
    }

    fn materialization(&self) -> MaterializationContext<'_> {
        MaterializationContext {
            source_files: self.source.files(),
            target_runtime: &self.target_runtime,
            frozen: self
                .frozen
                .as_ref()
                .expect("preflight freezes inputs before materialization"),
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

    fn bundle(&self, package: PackageId) -> &[u8] {
        &self.frozen.bundles[&package]
    }

    fn resource(&self, resource: ResourceId) -> &Path {
        &self.frozen.resources[&resource]
    }
}

/// First exact source-package materialization profile.
pub enum PureRStatic {}

/// Opaque capability proving full preflight succeeded for one profile.
pub struct BuildableProgram<'a, Profile> {
    program: &'a ProgramIr,
    materialization: MaterializationContext<'a>,
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
        context: &'a mut BuildContext,
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
        if ir.program().root_artifact().description.is_empty() {
            blockers.insert("Root source-package DESCRIPTION plan is missing".into());
        }
        blockers.extend(relocation_site_issues(ir.program()));
        let program = ir.program();
        for import in &program.root_namespace().imports {
            let target = program.binding(import.target);
            let external = matches!(
                program
                    .namespace(program.binding_namespace(import.target))
                    .state,
                crate::ir::LinkNamespaceState::External { .. }
            );
            if external && target.name != import.local {
                blockers.insert(format!(
                    "Root imports External `{}` as `{}`, which NAMESPACE cannot express",
                    target.name, import.local
                ));
            }
        }
        if !blockers.is_empty() {
            return Err(BuildReport::new(blockers.into_iter().collect()).into());
        }
        context.freeze(ir)?;
        let context: &'a BuildContext = context;
        Ok(BuildableProgram {
            program: ir.program(),
            materialization: context.materialization(),
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
    let temporary = tempfile::Builder::new()
        .prefix(".slinker-materialize-")
        .tempdir_in(parent)?;
    let package_name = &buildable
        .program
        .package(buildable.program.root_package())
        .identity()
        .name;
    let package_root = temporary.path().join(package_name);
    fs::create_dir_all(package_root.join("R"))?;
    fs::create_dir_all(package_root.join("inst/slinker"))?;
    fs::write(
        package_root.join("DESCRIPTION"),
        buildable.program.root_artifact().description.as_bytes(),
    )?;
    fs::write(
        package_root.join("NAMESPACE"),
        render_namespace(buildable.program),
    )?;
    copy_root_resources(
        buildable.materialization.source_files().root(),
        &package_root,
    )?;
    let payload_directory = package_root.join("inst/slinker/payload");
    fs::create_dir_all(&payload_directory)?;
    for namespace in buildable.program.namespaces() {
        if has_payloads(buildable.program, namespace) {
            fs::write(
                payload_directory.join(format!(
                    "{}.rds",
                    buildable.program.package(namespace.package).identity().name
                )),
                buildable.materialization.bundle(namespace.package),
            )?;
        }
    }
    let generated = generate_r_source(buildable.program)?;
    let mut worker = buildable.materialization.target_runtime().worker()?;
    validate_program_code(buildable.program, &mut worker)?;
    validate_r_source(&mut worker, &generated)?;
    fs::write(package_root.join("R/zzz-slinker-generated.R"), generated)?;
    copy_linked_resources(buildable.program, buildable.materialization, &package_root)?;
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

fn generate_r_source(program: &ProgramIr) -> Result<String, MaterializeError> {
    let mut out = String::new();
    out.push_str(
        ".slinker_runtime <- base::new.env(parent = base::baseenv())\nbase::local(envir = .slinker_runtime, {\n",
    );
    out.push_str(GENERATED_RUNTIME);
    out.push('\n');
    writeln!(
        out,
        ".slinker_target <- c(version = {}, platform = {}, arch = {})",
        r_string(&program.target().r_version),
        r_string(&program.target().platform),
        r_string(&program.target().arch)
    )
    .expect("String writes cannot fail");
    writeln!(
        out,
        ".slinker_root_package <- {}",
        r_string(&program.package(program.root_package()).identity().name)
    )
    .expect("String writes cannot fail");
    let root = program.root_namespace();
    let root_on_load = program.root_artifact().on_load;
    let mut root_code = String::new();
    for closure in namespace_closures(program, root) {
        let source = relocated_source(program, program.closure(closure).code)?;
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
            root_code.push_str(&source);
        }
        root_code.push('\n');
    }

    out.push_str("bootstrap <- function(root, libname, pkgname) {\n  .slinker_check_target()\n");
    out.push_str("  linked <- list()\n");
    for activation in program.activations() {
        let package = program
            .package(program.namespace(activation.namespace).package)
            .identity();
        writeln!(
            out,
            "  linked[[{name}]] <- .slinker_new_namespace({name}, {})",
            r_string(package.version.as_ref()),
            name = r_string(&package.name),
        )
        .expect("String writes cannot fail");
    }
    for activation in program.activations() {
        let namespace = program.namespace(activation.namespace);
        let name = &program.package(namespace.package).identity().name;
        writeln!(
            out,
            "  local({{\n    ns <- linked[[{}]]\n    imports <- parent.env(ns)",
            r_string(name)
        )
        .expect("String writes cannot fail");
        for native in &activation.native_components {
            let Some(library) = &native.library else {
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
            writeln!(
                out,
                "    .slinker_load_native(ns, {}, {}, c({symbols}))",
                r_string(name),
                r_string(library)
            )
            .expect("String writes cannot fail");
        }
        for import in &namespace.imports {
            writeln!(
                out,
                "    assign({}, {}, envir = imports)",
                r_string(&import.local),
                binding_reference(program, import.target)
            )
            .expect("String writes cannot fail");
        }
        for closure in namespace_closures(program, namespace) {
            let source = relocated_source(program, program.closure(closure).code)?;
            writeln!(
                out,
                "    eval(parse(text = {}), envir = ns)",
                r_string(&source)
            )
            .expect("String writes cannot fail");
        }
        if has_payloads(program, namespace) {
            writeln!(out, "    .slinker_populate(ns, {})", r_string(name))
                .expect("String writes cannot fail");
        }
        writeln!(
            out,
            "    .slinker_activate(ns, {}, {}, {}, {})\n  }})",
            r_vector(
                activation
                    .exports
                    .bindings()
                    .iter()
                    .map(|binding| program.binding(*binding).name.as_str())
                    .chain(activation.unretained_exports.iter().map(String::as_str))
            ),
            s3_matrix(program, namespace),
            r_vector(activation.stubs.iter().map(String::as_str)),
            if activation.on_load.is_some() {
                "TRUE"
            } else {
                "FALSE"
            }
        )
        .expect("String writes cannot fail");
    }
    for import in &root.imports {
        if matches!(
            program
                .namespace(program.binding_namespace(import.target))
                .state,
            LinkNamespaceState::Linked(_)
        ) {
            writeln!(
                out,
                "  assign({}, {}, envir = parent.env(root))",
                r_string(&import.local),
                binding_reference(program, import.target)
            )
            .expect("String writes cannot fail");
        }
    }
    if has_payloads(program, root) {
        writeln!(
            out,
            "  .slinker_populate(root, {})",
            r_string(&program.package(root.package).identity().name)
        )
        .expect("String writes cannot fail");
    }
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

fn has_payloads(program: &ProgramIr, namespace: &crate::ir::Namespace) -> bool {
    namespace.bindings.values().any(|binding| {
        matches!(
            initial_value(program, *binding).map(|value| program.value(value)),
            Some(Value::Payload(_))
        )
    })
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
    let package = r_string(&program.package(namespace.package).identity().name);
    let name = r_string(&program.binding(binding).name);
    match namespace.state {
        LinkNamespaceState::External { .. } => match program.binding(binding).state {
            LinkBindingState::External {
                access: crate::ir::ExternalBindingAccess::Internal,
                ..
            } => namespace_get(&package, &name),
            _ => format!("base::getExportedValue({package}, {name})"),
        },
        LinkNamespaceState::Root(_) | LinkNamespaceState::Linked(_) => {
            namespace_get(&package, &name)
        }
    }
}

fn namespace_get(package: &str, name: &str) -> String {
    format!("base::get({name}, envir = base::asNamespace({package}), inherits = FALSE)")
}

fn s3_matrix(program: &ProgramIr, namespace: &crate::ir::Namespace) -> String {
    let rows = namespace
        .s3_registrations
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
        .chain(rows.iter().map(|row| {
            row.generic.package.map_or_else(
                || "NA_character_".to_owned(),
                |package| r_string(&program.package(package).identity().name),
            )
        }))
        .collect::<Vec<_>>();
    format!("matrix(as.character(c({})), ncol = 4L)", cells.join(", "))
}

fn r_vector<'a>(values: impl Iterator<Item = &'a str>) -> String {
    format!(
        "as.character(c({}))",
        values.map(r_string).collect::<Vec<_>>().join(", ")
    )
}

fn render_namespace(program: &ProgramIr) -> String {
    let root = program.root_namespace();
    let mut out = String::new();
    for binding in program.root_artifact().exports.bindings() {
        writeln!(out, "export({})", r_string(&program.binding(*binding).name))
            .expect("String writes cannot fail");
    }
    for import in &root.imports {
        let target = program.namespace(program.binding_namespace(import.target));
        if let LinkNamespaceState::External { package } = target.state {
            writeln!(
                out,
                "importFrom({}, {})",
                r_string(&program.package(package).identity().name),
                r_string(&program.binding(import.target).name)
            )
            .expect("String writes cannot fail");
        }
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
        writeln!(
            out,
            "useDynLib({})",
            std::iter::once(r_binding_name(&native.name))
                .chain(registration)
                .chain(symbols)
                .collect::<Vec<_>>()
                .join(", ")
        )
        .expect("String writes cannot fail");
    }
    for registration in &root.s3_registrations {
        let registration = program.s3_registration(*registration);
        let generic = match registration.generic.package {
            Some(package) => format!(
                "{}::{}",
                r_binding_name(&program.package(package).identity().name),
                r_binding_name(&registration.generic.name)
            ),
            None => r_string(&registration.generic.name),
        };
        writeln!(
            out,
            "S3method({generic}, {}, {})",
            r_string(&registration.class),
            r_string(&program.binding(registration.method).name)
        )
        .expect("String writes cannot fail");
    }
    out
}

fn relocation_site(relocation: &Relocation) -> &crate::ir::CodeSite {
    match relocation {
        Relocation::Binding { site, .. }
        | Relocation::Namespace { site, .. }
        | Relocation::Package { site, .. }
        | Relocation::Resource { site, .. } => site,
    }
}

fn relocation_matches(program: &ProgramIr, relocation: &Relocation, original: &str) -> bool {
    let unqualified = original
        .rsplit(':')
        .next()
        .unwrap_or(original)
        .trim_matches('`');
    let callee = original.trim_start_matches("base::");
    match relocation {
        Relocation::Binding { target, .. } => unqualified == program.binding(*target).name,
        Relocation::Namespace { target, .. } => original.contains(
            program
                .package(program.namespace(*target).package)
                .identity()
                .name
                .as_str(),
        ),
        Relocation::Package { operation, .. } => {
            let names: &[&str] = match operation {
                PackageOperationIr::RequireNamespace { .. } => &["requireNamespace("],
                PackageOperationIr::LoadNamespace => &["loadNamespace("],
                PackageOperationIr::GetNamespace => &["getNamespace("],
                PackageOperationIr::AsNamespace => &["asNamespace("],
                PackageOperationIr::PackageVersion { .. } => &["packageVersion("],
            };
            names.iter().any(|name| callee.starts_with(name))
        }
        Relocation::Resource { .. } => callee.starts_with("system.file("),
    }
}

fn relocation_site_issues(program: &ProgramIr) -> Vec<String> {
    let mut sites = BTreeMap::<crate::ir::CodeId, Vec<crate::ir::CodeOccurrenceId>>::new();
    let mut issues = Vec::new();
    for relocation in program.relocations() {
        let site = relocation_site(relocation);
        sites.entry(site.code).or_default().push(site.occurrence);
        let code_ir = program.code(site.code);
        let occurrence = code_ir.occurrence(site.occurrence);
        let original = code_ir
            .source()
            .get(occurrence.start..occurrence.end)
            .unwrap_or_default();
        if !relocation_matches(program, relocation, original) {
            issues.push(format!(
                "relocation does not match the syntax it rewrites: `{original}`"
            ));
        }
    }
    for (code, mut occurrences) in sites {
        let code_ir = program.code(code);
        let source = code_ir.source();
        occurrences.sort_by_key(|occurrence| code_ir.occurrence(*occurrence).start);
        for pair in occurrences.windows(2) {
            if pair[0] == pair[1] {
                issues.push("two relocations rewrite the same code occurrence".to_owned());
            } else if code_ir.occurrence(pair[0]).end > code_ir.occurrence(pair[1]).start {
                issues.push("relocation occurrences overlap in one code unit".to_owned());
            }
        }
        for occurrence in occurrences
            .iter()
            .map(|occurrence| code_ir.occurrence(*occurrence))
        {
            if occurrence.start > occurrence.end
                || occurrence.end > source.len()
                || !source.is_char_boundary(occurrence.start)
                || !source.is_char_boundary(occurrence.end)
            {
                issues.push("relocation occurrence lies outside its code unit".to_owned());
            }
        }
    }
    issues
}

fn relocated_source(
    program: &ProgramIr,
    code: crate::ir::CodeId,
) -> Result<String, MaterializeError> {
    let code_ir = program.code(code);
    let mut source = code_ir.source().to_owned();
    let mut relocations = program
        .relocations()
        .iter()
        .filter_map(|relocation| {
            let site = relocation_site(relocation);
            (site.code == code).then_some((site, relocation))
        })
        .collect::<Vec<_>>();
    relocations
        .sort_by_key(|(site, _)| std::cmp::Reverse(code_ir.occurrence(site.occurrence).start));
    for (site, relocation) in relocations {
        let occurrence = code_ir.occurrence(site.occurrence);
        let replacement = match relocation {
            Relocation::Binding { target, access, .. } => match access {
                crate::ir::ExternalBindingAccess::Exported => binding_reference(program, *target),
                crate::ir::ExternalBindingAccess::Internal => {
                    let namespace = program.namespace(program.binding_namespace(*target));
                    namespace_get(
                        &r_string(&program.package(namespace.package).identity().name),
                        &r_string(&program.binding(*target).name),
                    )
                }
            },
            Relocation::Namespace { target, .. } => {
                let package = program
                    .package(program.namespace(*target).package)
                    .identity();
                format!("base::asNamespace({})", r_string(&package.name))
            }
            Relocation::Package {
                target, operation, ..
            } => match operation {
                PackageOperationIr::RequireNamespace { result } => {
                    if *result { "TRUE" } else { "FALSE" }.into()
                }
                PackageOperationIr::LoadNamespace
                | PackageOperationIr::GetNamespace
                | PackageOperationIr::AsNamespace => {
                    let package = program
                        .package(target.expect("resolved package operation"))
                        .identity();
                    format!("base::asNamespace({})", r_string(&package.name))
                }
                PackageOperationIr::PackageVersion { version } => {
                    format!("base::package_version({})", r_string(version))
                }
            },
            Relocation::Resource { target, .. } => {
                let resource = program.resource(*target);
                let package = program.package(resource.package).identity();
                format!(
                    "base::system.file(\"slinker\", \"resources\", {}, {}, package = {})",
                    r_string(&package.name),
                    r_string(&resource.path),
                    r_string(&program.package(program.root_package()).identity().name)
                )
            }
        };
        source.replace_range(occurrence.start..occurrence.end, &replacement);
    }
    Ok(source)
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

fn validate_program_code(
    program: &ProgramIr,
    worker: &mut WorkerClient,
) -> Result<(), MaterializeError> {
    for (code_id, code) in program.indexed_codes() {
        let emitted = relocated_source(program, code_id)?;
        let normalized = worker.normalize_syntax(&emitted)?;
        let normalized_again = worker.normalize_syntax(&normalized)?;
        if normalized != normalized_again {
            return Err(MaterializeError::InvalidR(format!(
                "CodeIr {code_id:?} is not stable across target-R emission round trip"
            )));
        }
        let relocated = program
            .relocations()
            .iter()
            .any(|relocation| match relocation {
                Relocation::Binding { site, .. }
                | Relocation::Namespace { site, .. }
                | Relocation::Package { site, .. }
                | Relocation::Resource { site, .. } => site.code == code_id,
            });
        if !relocated {
            let digest = crate::package::Digest::of(&normalized);
            if &digest != code.normalized_shape() {
                return Err(MaterializeError::InvalidR(format!(
                    "CodeIr {code_id:?} changed normalized shape before emission: expected {}, got {}; normalized source {normalized:?}",
                    code.normalized_shape().0,
                    digest.0
                )));
            }
        }
    }
    Ok(())
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
            .join(&package.name)
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
.slinker_new_namespace <- function(name, version) {
  if (isNamespaceLoaded(name)) stop(sprintf("LinkedNamespaceCollision(%s)", name), call. = FALSE)
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
  setNamespaceInfo(namespace, "dynlibs", NULL)
  setNamespaceInfo(namespace, "S3methods", matrix(NA_character_, 0L, 4L))
  namespace$.__S3MethodsTable__. <- new.env(hash = TRUE, parent = baseenv())
  .Internal(registerNamespace(name, namespace))
  namespace
}
.slinker_load_native <- function(namespace, package, library, symbols) {
  path <- system.file("slinker", "resources", package, library, package = .slinker_root_package, mustWork = TRUE)
  dll <- dyn.load(path, local = TRUE)
  for (binding in names(symbols)) {
    assign(binding, getNativeSymbolInfo(symbols[[binding]], dll), envir = namespace)
  }
}
.slinker_populate <- function(namespace, package) {
  bundle <- system.file("slinker", "payload", paste0(package, ".rds"), package = .slinker_root_package, mustWork = TRUE)
  invisible(list2env(readRDS(bundle), envir = namespace))
}
.slinker_stub <- function(namespace, package, binding) {
  makeActiveBinding(binding, function(value) {
    stop(sprintf("`%s::%s` was removed by slinker because the build never reached it", package, binding), call. = FALSE)
  }, namespace)
}
.slinker_activate <- function(namespace, exports, s3, removed, on_load) {
  name <- unname(getNamespaceName(namespace))
  if (nrow(s3)) registerS3methods(s3, name, namespace)
  if (on_load) get(".onLoad", envir = namespace, inherits = FALSE)("", name)
  for (binding in removed[!vapply(removed, exists, logical(1), envir = namespace, inherits = FALSE)]) {
    .slinker_stub(namespace, name, binding)
  }
  exports <- exports[vapply(exports, exists, logical(1), envir = namespace)]
  if (length(exports)) namespaceExport(namespace, exports)
  lockEnvironment(namespace, TRUE)
  lockEnvironment(parent.env(namespace), TRUE)
  invisible(namespace)
}
"#;
