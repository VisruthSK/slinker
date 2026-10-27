mod copy;
mod emit;
mod payload;
mod relocated;
mod report;

pub use report::{Blocker, BlockerGroup, BuildReport};

use crate::analysis::LinkIr;
use crate::filesystem::copy_entry;
use crate::ir::{ProgramIr, ResourceId};
use crate::package::{PackageId, SyntaxValidation};
use crate::source::StagedRoot;
use crate::worker::protocol::{DataLibraryFiles, NamespaceImageSpec, PackageSpec, PayloadSpec};
use crate::worker::service::WorkerService;
use copy::{
    copy_dataset_libraries, copy_linked_resources, copy_root_resources, freeze_inst_resources,
};
use emit::{generate_r_source, render_namespace};
use payload::{CheckedPayloadBundle, check_payload_bundles, closure_patches};
use relocated::RelocatedCode;
use std::collections::BTreeMap;

use crate::ir::PackageRole;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tempfile::TempDir;
use thiserror::Error;

#[derive(Debug)]
pub(crate) struct BuildContext {
    staged_root: StagedRoot,
    workers: Arc<WorkerService>,
}

#[derive(Debug)]
struct FrozenInputs {
    bundles: Vec<CheckedPayloadBundle>,
    resources: BTreeMap<ResourceId, PathBuf>,
    datasets: BTreeMap<PackageId, DataLibraryFiles>,
    root_resources: PathBuf,
    generated_r: String,
    namespace: String,
    _directory: TempDir,
}

impl BuildContext {
    pub(crate) fn new(staged_root: StagedRoot, workers: Arc<WorkerService>) -> Self {
        Self {
            staged_root,
            workers,
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
        let mut worker = self.workers.preparation()?;
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
        let root_resources = directory.path().join("root-resources");
        fs::create_dir(&root_resources)?;
        copy_root_resources(self.staged_root.source_root(), &root_resources)?;
        let native = if program.root_artifact().native_components.is_empty() {
            None
        } else {
            let native = dunce::canonicalize(self.staged_root.package_root().join("libs"))?;
            copy_entry(&native, &root_resources.join("inst/libs"))?;
            Some(native)
        };
        let inst = self.staged_root.source_root().join("inst");
        if inst.is_dir() {
            freeze_inst_resources(
                &inst,
                self.staged_root.package_root(),
                &root_resources.join("inst"),
                native.as_deref(),
            )?;
        }
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
        let generated_r = generate_r_source(program, &code)?;
        let namespace = render_namespace(program);
        for source in [&generated_r, &namespace] {
            if let SyntaxValidation::Rejected(message) = worker.validate_syntax(source)? {
                return Err(BuildContextError::InvalidCode(message).into());
            }
        }
        self.workers.check()?;
        Ok(FrozenInputs {
            bundles,
            resources,
            datasets,
            root_resources,
            generated_r,
            namespace,
            _directory: directory,
        })
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

pub(crate) enum PureRStatic {}

pub(crate) struct BuildableProgram<'a> {
    program: &'a ProgramIr,
    description: &'a str,
    frozen: FrozenInputs,
}

impl PureRStatic {
    pub(crate) fn check<'a>(
        ir: &'a LinkIr,
        context: &BuildContext,
    ) -> Result<BuildableProgram<'a>, PreflightError> {
        let mut blockers = BuildReport::analysis_blockers(ir);
        let description = ir.program().root_artifact().description.as_deref();
        if description.is_none() {
            blockers.push(Blocker::preflight(
                "Root source-package DESCRIPTION plan is missing".into(),
            ));
        }
        let program = ir.program();
        let reserved = context.staged_root.source_root().join("inst/slinker");
        if reserved.exists()
            && (!reserved.is_dir() || fs::read_dir(&reserved)?.next().transpose()?.is_some())
        {
            blockers.push(Blocker::preflight(
                "Root resources occupy reserved generated path inst/slinker".into(),
            ));
        }
        for path in ["R", "Meta", "DESCRIPTION", "NAMESPACE"] {
            if context
                .staged_root
                .source_root()
                .join("inst")
                .join(path)
                .exists()
            {
                blockers.push(Blocker::preflight(format!(
                    "Root inst/{path} overlaps package metadata or generated code"
                )));
            }
        }
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
            frozen,
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
pub(crate) struct PendingPackage {
    root: PathBuf,
    output: OutputLease,
    _directory: TempDir,
}

impl PendingPackage {
    pub(crate) fn publish(self) -> Result<(), MaterializeError> {
        publish(&self.root, &self.output.path)?;
        Ok(())
    }
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "consuming the preflight capability makes each approved program materialize once"
)]
pub(crate) fn materialize(
    buildable: BuildableProgram<'_>,
    lease: OutputLease,
) -> Result<PendingPackage, MaterializeError> {
    let output = &lease.path;
    if output.exists() && !is_generated_package(output) {
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
    let package_root = temporary.path().join(package_name.as_str());
    fs::create_dir_all(package_root.join("R"))?;
    fs::create_dir_all(package_root.join("inst/slinker"))?;
    fs::write(
        package_root.join("DESCRIPTION"),
        buildable.description.as_bytes(),
    )?;
    fs::write(package_root.join("NAMESPACE"), &buildable.frozen.namespace)?;
    copy_entry(&buildable.frozen.root_resources, &package_root)?;
    let payload_directory = package_root.join("inst/slinker/payload");
    fs::create_dir_all(&payload_directory)?;
    for checked in &buildable.frozen.bundles {
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
    fs::write(
        package_root.join("R/zzz-slinker-generated.R"),
        &buildable.frozen.generated_r,
    )?;
    copy_linked_resources(buildable.program, &buildable.frozen, &package_root)?;
    copy_dataset_libraries(buildable.program, &buildable.frozen, &package_root)?;
    Ok(PendingPackage {
        root: package_root,
        output: lease,
        _directory: temporary,
    })
}

#[derive(Debug)]
pub(crate) struct OutputLease {
    path: PathBuf,
    _lock: fs::File,
}

impl OutputLease {
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
    pub(crate) fn acquire(output: &Path) -> std::io::Result<Self> {
        let absolute = std::path::absolute(output)?;
        let parent = absolute
            .parent()
            .ok_or_else(|| std::io::Error::other("output has no parent"))?;
        fs::create_dir_all(parent)?;
        let name = absolute
            .file_name()
            .ok_or_else(|| std::io::Error::other("output has no filename"))?;
        let parent = dunce::canonicalize(parent)?;
        let path = parent.join(name);
        if fs::symlink_metadata(&path).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
            return Err(std::io::Error::other(
                "generated output cannot be a symlink",
            ));
        }
        let mut lock_name = std::ffi::OsString::from(".#");
        lock_name.push(name);
        lock_name.push(".slinker-lock");
        let lock = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(parent.join(lock_name))?;
        lock.try_lock().map_err(|error| {
            std::io::Error::other(format!(
                "output {} is already owned by another build: {error}",
                path.display()
            ))
        })?;
        Ok(Self { path, _lock: lock })
    }
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
        if let Err(restore) = fs::rename(&backup, output) {
            let retained = retired.keep();
            return Err(std::io::Error::other(format!(
                "publication failed: {error}; restore failed: {restore}; previous package preserved at {}",
                retained.join("previous").display()
            )));
        }
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
    #[error(transparent)]
    Io(#[from] std::io::Error),
}
