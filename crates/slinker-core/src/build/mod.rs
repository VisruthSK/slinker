mod copy;
mod emit;
pub mod incremental;
mod payload;
mod relocated;
mod report;

pub use report::{Blocker, BlockerGroup, BuildReport};

use crate::TargetEnvironment;
use crate::analysis::LinkIr;
use crate::ir::{ProgramIr, ResourceId};
use crate::package::{CanonicalSyntax, PackageId};
use crate::source::{FrozenSourceFiles, SourcePackageSnapshot, StagedRoot};
use crate::worker::client::WorkerClient;
use crate::worker::protocol::{DataLibraryFiles, NamespaceImageSpec, PackageSpec, PayloadSpec};
use copy::{copy_dataset_libraries, copy_entry, copy_linked_resources, copy_root_resources};
use emit::{generate_r_source, render_namespace};
use payload::{CheckedPayloadBundle, check_payload_bundles, closure_patches};
use relocated::RelocatedCode;
use std::collections::BTreeMap;

use crate::ir::PackageRole;
use std::fs;
use std::marker::PhantomData;
use std::ops::{Deref, DerefMut};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tempfile::TempDir;
use thiserror::Error;

#[derive(Debug)]
pub struct TargetRuntimeHandle {
    r_home: PathBuf,
    target: TargetEnvironment,
    spare: Mutex<Option<WorkerClient>>,
}

pub(crate) struct BorrowedWorker<'a> {
    client: Option<WorkerClient>,
    spare: &'a Mutex<Option<WorkerClient>>,
}

impl Deref for BorrowedWorker<'_> {
    type Target = WorkerClient;

    fn deref(&self) -> &WorkerClient {
        self.client
            .as_ref()
            .expect("a borrowed worker holds its client until it is dropped")
    }
}

impl DerefMut for BorrowedWorker<'_> {
    fn deref_mut(&mut self) -> &mut WorkerClient {
        self.client
            .as_mut()
            .expect("a borrowed worker holds its client until it is dropped")
    }
}

impl Drop for BorrowedWorker<'_> {
    fn drop(&mut self) {
        if let Ok(mut spare) = self.spare.lock() {
            *spare = self.client.take();
        }
    }
}

impl TargetRuntimeHandle {
    pub fn new(r_home: PathBuf, target: TargetEnvironment) -> Self {
        Self {
            r_home,
            target,
            spare: Mutex::new(None),
        }
    }

    pub fn r_home(&self) -> &Path {
        &self.r_home
    }

    pub fn target(&self) -> &TargetEnvironment {
        &self.target
    }

    pub(crate) fn worker(&self) -> crate::Result<BorrowedWorker<'_>> {
        let reused = self.spare.lock().ok().and_then(|mut spare| spare.take());
        let client = match reused {
            Some(client) => client,
            None => WorkerClient::spawn(self.r_home.clone(), &self.target, 0)?,
        };
        Ok(BorrowedWorker {
            client: Some(client),
            spare: &self.spare,
        })
    }
}

#[derive(Debug)]
pub struct BuildContext {
    source: SourcePackageSnapshot,
    _staged_root: StagedRoot,
    target_runtime: TargetRuntimeHandle,
}

#[derive(Debug)]
struct FrozenInputs {
    bundles: Vec<CheckedPayloadBundle>,
    resources: BTreeMap<ResourceId, PathBuf>,
    datasets: BTreeMap<PackageId, DataLibraryFiles>,
    code: RelocatedCode,
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
        }
    }

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
                name: identity.name.clone(),
                version: identity.version.to_string(),
                image_fingerprint: identity.image_fingerprint.clone(),
                root: location(package).clone(),
            }
        };
        let mut worker = self.target_runtime.worker()?;
        let code = RelocatedCode::verify(program, &mut worker)?;
        let mut bundles = Vec::new();
        if !program.payload_bundles().is_empty() {
            let namespaces = program
                .packages()
                .filter(|(_, package)| package.role() != PackageRole::External)
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
                        .map(|binding| program.binding(*binding).name.clone())
                        .collect(),
                    patches: closure_patches(program, bundle, &code),
                })
                .collect();
            let serialized = worker.serialize_payloads(namespaces, specs)?;
            bundles = check_payload_bundles(program, serialized)?;
        }

        let mut datasets = BTreeMap::new();
        for (package, library) in program.dataset_libraries() {
            let sets = library
                .sets()
                .iter()
                .map(|(set, objects)| (set.clone(), objects.clone()))
                .collect();
            let objects = library.objects().iter().cloned().collect();
            datasets.insert(package, worker.data_library(spec(package), objects, sets)?);
        }

        let directory = tempfile::Builder::new()
            .prefix("slinker-frozen-")
            .tempdir()?;
        let mut resources = BTreeMap::new();
        for (ordinal, (id, resource)) in program.indexed_resources().enumerate() {
            let frozen = directory.path().join(ordinal.to_string());
            copy_entry(
                &location(resource.package).join(resource.path.as_str()),
                &frozen,
            )?;
            resources.insert(id, frozen);
        }

        if let Some(changed) = sources.changed()? {
            return Err(BuildContextError::TargetUniverseChanged(changed.name.to_string()).into());
        }
        Ok(FrozenInputs {
            bundles,
            code,
            resources,
            datasets,
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

    fn dataset_library(&self, package: PackageId) -> &DataLibraryFiles {
        &self.frozen.datasets[&package]
    }

    fn code(&self) -> &RelocatedCode {
        &self.frozen.code
    }
}

pub enum PureRStatic {}

pub struct BuildableProgram<'a, Profile> {
    program: &'a ProgramIr,
    description: &'a str,
    context: &'a BuildContext,
    frozen: FrozenInputs,
    _profile: PhantomData<Profile>,
}

impl PureRStatic {
    pub fn check<'a>(
        ir: &'a LinkIr,
        context: &'a BuildContext,
    ) -> Result<BuildableProgram<'a, PureRStatic>, PreflightError> {
        let mut blockers = BuildReport::analysis_blockers(ir);
        let description = ir.program().root_artifact().description.as_deref();
        if description.is_none() {
            blockers.push(Blocker::preflight(
                "Root source-package DESCRIPTION plan is missing".into(),
            ));
        }
        let program = ir.program();
        for import in program.root_artifact().load.before_bootstrap().imports() {
            let target = program.binding(import.target);
            if target.name != import.local {
                blockers.push(Blocker::preflight(format!(
                    "Root imports External `{}` as `{}`, which NAMESPACE cannot express",
                    target.name, import.local
                )));
            }
        }
        let Some(description) = description.filter(|_| blockers.is_empty()) else {
            return Err(BuildReport::new(blockers).into());
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
    #[error("build preflight failed:\n{0}")]
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

#[derive(Debug)]
pub struct GeneratedPackage {
    path: PathBuf,
}

impl GeneratedPackage {
    pub fn path(&self) -> &Path {
        &self.path
    }
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "consuming the preflight capability makes each approved program materialize once"
)]
pub fn materialize(
    buildable: BuildableProgram<'_, PureRStatic>,
    output: &Path,
) -> Result<GeneratedPackage, MaterializeError> {
    if output.exists() && !is_generated_package(output) {
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
    copy_dataset_libraries(buildable.program, materialization, &package_root)?;
    publish(&package_root, output)?;
    Ok(GeneratedPackage {
        path: output.to_path_buf(),
    })
}

fn is_generated_package(directory: &Path) -> bool {
    directory.join("R/zzz-slinker-generated.R").is_file() && directory.join("inst/slinker").is_dir()
}

fn publish(package_root: &Path, output: &Path) -> std::io::Result<()> {
    if !output.exists() {
        return fs::rename(package_root, output);
    }
    let unchanged = preserve_unchanged_files(output, package_root)?;
    if unchanged {
        return Ok(());
    }
    let parent = output.parent().unwrap_or(Path::new("."));
    let retired = tempfile::Builder::new()
        .prefix(".slinker-replaced-")
        .tempdir_in(parent)?;
    let backup = retired.path().join("previous");
    fs::rename(output, &backup)?;
    if let Err(error) = fs::rename(package_root, output) {
        fs::rename(&backup, output)?;
        return Err(error);
    }
    Ok(())
}

fn preserve_unchanged_files(old: &Path, new: &Path) -> std::io::Result<bool> {
    let mut identical = true;
    let mut present = std::collections::BTreeSet::new();
    for entry in fs::read_dir(new)? {
        let entry = entry?;
        let name = entry.file_name();
        present.insert(name.clone());
        let (old_path, new_path) = (old.join(&name), entry.path());
        if entry.file_type()?.is_dir() {
            identical &= old_path.is_dir() && preserve_unchanged_files(&old_path, &new_path)?;
        } else if old_path.is_file() && fs::read(&old_path)? == fs::read(&new_path)? {
            let modified = fs::metadata(&old_path)?.modified()?;
            fs::OpenOptions::new()
                .write(true)
                .open(&new_path)?
                .set_modified(modified)?;
        } else {
            identical = false;
        }
    }
    for entry in fs::read_dir(old)? {
        identical &= present.contains(&entry?.file_name());
    }
    Ok(identical)
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
fn validate_r_source(worker: &mut WorkerClient, source: &str) -> Result<(), MaterializeError> {
    match worker.canonical_syntax(source)? {
        CanonicalSyntax::Stable(_) => Ok(()),
        CanonicalSyntax::Unstable => Err(MaterializeError::InvalidR(
            "target-R parse/deparse normalization is not stable".into(),
        )),
    }
}
