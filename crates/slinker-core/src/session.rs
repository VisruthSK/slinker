use std::io;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::analysis::{LinkIr, Linker};
use crate::build::incremental::{
    BuildRecord, BuildState, consulted_packages, inputs_digest, is_up_to_date,
};
use crate::build::{BuildContext, PureRStatic, materialize};
use crate::cache::CacheLocation;
use crate::package::{PackageLocator, PackageName, PackageStore, tree_digest};
use crate::source::{SourcePackageSnapshot, StagedRoot, stage_root};
use crate::{Description, PrimedWorker, TargetEnvironment, TargetEnvironmentRequest};

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Source(#[from] crate::source::SourcePackageError),
    #[error(transparent)]
    Staging(#[from] crate::source::StagingError),
    #[error(transparent)]
    Target(#[from] crate::TargetEnvironmentError),
    #[error(transparent)]
    Analysis(#[from] crate::Error),
    #[error(transparent)]
    Preflight(#[from] crate::build::PreflightError),
    #[error(transparent)]
    Materialize(#[from] crate::build::MaterializeError),
    #[error("{0}")]
    InvalidRoot(String),
}

/// Options frozen when a session captures its source and ordered library universe.
#[derive(Clone, Debug)]
pub struct SessionOptions {
    pub libraries: Vec<PathBuf>,
    pub external: Vec<PackageName>,
    pub linked: Vec<PackageName>,
    pub threads: NonZeroUsize,
    pub cache: CacheLocation,
    pub worker_executable: crate::WorkerExecutable,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RootSpec {
    Source(PathBuf),
    Installed(PackageName),
    InstalledDirectory(PathBuf),
}

impl RootSpec {
    pub fn parse(value: &str) -> Result<Self, &'static str> {
        if value.is_empty() {
            return Err("expected a package name or path");
        }
        if !value.contains(['/', '\\']) && value != "." && value != ".." {
            return Ok(Self::Installed(value.into()));
        }
        let path = PathBuf::from(value);
        if path.join("Meta").join("package.rds").is_file() {
            Ok(Self::InstalledDirectory(path))
        } else {
            Ok(Self::Source(path))
        }
    }
}

struct SourceInputs {
    snapshot: SourcePackageSnapshot,
    staged: StagedRoot,
}

pub struct Session {
    r_home: PathBuf,
    target: TargetEnvironment,
    root: PackageName,
    options: SessionOptions,
    root_description: Option<Arc<str>>,
    retained: Option<SourceInputs>,
    primed: Mutex<Option<PrimedWorker>>,
}

pub struct SourceSession {
    session: Session,
    inputs: SourceInputs,
}

impl Session {
    pub fn open(
        root: &RootSpec,
        universe: &SessionOptions,
        r_home: PathBuf,
    ) -> Result<Self, SessionError> {
        match root {
            RootSpec::Installed(name) => {
                let libraries = absolute_libraries(universe)?;
                Self::capture(r_home, name.clone(), None, libraries, universe.clone())
            }
            RootSpec::InstalledDirectory(directory) => {
                let (parent, name) = installed_directory(directory)?;
                let libraries = std::iter::once(parent.clone())
                    .chain(
                        absolute_libraries(universe)?
                            .into_iter()
                            .filter(|library| *library != parent),
                    )
                    .collect();
                Self::capture(r_home, name, None, libraries, universe.clone())
            }
            RootSpec::Source(path) => {
                let SourceSession {
                    mut session,
                    inputs,
                } = SourceSession::open(path, universe, r_home)?;
                session.retained = Some(inputs);
                Ok(session)
            }
        }
    }

    fn capture(
        r_home: PathBuf,
        root: PackageName,
        root_description: Option<Arc<str>>,
        libraries: Vec<PathBuf>,
        options: SessionOptions,
    ) -> Result<Self, SessionError> {
        let mut request = TargetEnvironmentRequest::new(r_home.clone());
        request.libraries = libraries;
        request.worker_executable = options.worker_executable.clone();
        let (target, primed) = request.capture_primed()?;
        Ok(Self {
            target,
            r_home,
            root,
            options,
            root_description,
            retained: None,
            primed: Mutex::new(Some(primed)),
        })
    }

    pub fn root(&self) -> &str {
        &self.root
    }

    pub fn target(&self) -> &TargetEnvironment {
        &self.target
    }

    pub fn analyze(&self, provenance: bool) -> Result<LinkIr, SessionError> {
        let universe = &self.options;
        let store = PackageStore::new(
            self.r_home.clone(),
            self.target.clone(),
            universe.cache.clone(),
        )?
        .with_worker_limit(universe.threads.get())
        .with_worker_executable(universe.worker_executable.clone());
        let store = match self.primed.lock().expect("primed worker").take() {
            Some(worker) => store.with_primed_worker(worker),
            None => store,
        };
        let mut linker = Linker::new(store, universe.threads.get())
            .with_external_packages(universe.external.iter().cloned())
            .with_linked_packages(universe.linked.iter().cloned());
        if !provenance {
            linker = linker.without_provenance();
        }
        if let Some(description) = &self.root_description {
            linker = linker.with_root_source(Arc::clone(description));
        }
        Ok(linker.analyze(&self.root)?)
    }
}

pub struct PreparedSource {
    snapshot: SourcePackageSnapshot,
    r_home: PathBuf,
    target: TargetEnvironment,
    options: SessionOptions,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BuildOutcome {
    Built,
    UpToDate,
}

impl BuildOutcome {
    pub fn status(self) -> &'static str {
        match self {
            Self::Built => "built",
            Self::UpToDate => "up_to_date",
        }
    }
}

impl PreparedSource {
    /// Build this frozen source, reusing an output only after validating its read set and contents.
    pub fn build(self, output: &Path) -> Result<BuildOutcome, SessionError> {
        let output = std::path::absolute(output)?;
        let package = self.snapshot.package().to_owned();
        let state = BuildState::new(&self.options.cache);
        fn sorted(names: &[PackageName]) -> Vec<&str> {
            let mut names = names.iter().map(PackageName::as_str).collect::<Vec<_>>();
            names.sort_unstable();
            names.dedup();
            names
        }
        let inputs = inputs_digest(
            &tree_digest(self.snapshot.files().root())?,
            &self.target,
            &sorted(&self.options.linked),
            &sorted(&self.options.external),
        );
        if let Some(record) = state.load(&output)
            && is_up_to_date(
                &record,
                &inputs,
                &PackageLocator::new(self.target.clone()),
                &package,
            )?
        {
            return Ok(BuildOutcome::UpToDate);
        }
        let session = SourceSession::stage(self)?;
        let ir = session.session().analyze(false)?;
        let consulted = consulted_packages(ir.consulted(), &package);
        let context = session.into_build_context();
        let buildable = PureRStatic::check(&ir, &context)?;
        let generated = materialize(buildable, &output)?;
        state.save(&BuildRecord {
            package,
            output,
            inputs: inputs.as_str().to_owned(),
            consulted,
            output_digest: tree_digest(generated.root())?.as_str().to_owned(),
        })?;
        generated.publish()?;
        Ok(BuildOutcome::Built)
    }

    pub fn snapshot(&self) -> &SourcePackageSnapshot {
        &self.snapshot
    }

    pub fn target(&self) -> &TargetEnvironment {
        &self.target
    }
}

impl SourceSession {
    /// Analyze and run build preflight without publishing a generated package.
    pub fn check(self) -> Result<(), SessionError> {
        let ir = self.session.analyze(false)?;
        let context = self.into_build_context();
        PureRStatic::check(&ir, &context)?;
        Ok(())
    }

    pub fn prepare(
        path: &Path,
        universe: &SessionOptions,
        r_home: PathBuf,
    ) -> Result<PreparedSource, SessionError> {
        let snapshot = SourcePackageSnapshot::capture(path)?;
        let mut request = TargetEnvironmentRequest::new(r_home.clone());
        request.libraries = absolute_libraries(universe)?;
        request.worker_executable = universe.worker_executable.clone();
        Ok(PreparedSource {
            snapshot,
            r_home,
            target: request.capture()?,
            options: universe.clone(),
        })
    }

    pub fn open(
        path: &Path,
        universe: &SessionOptions,
        r_home: PathBuf,
    ) -> Result<Self, SessionError> {
        Self::stage(Self::prepare(path, universe, r_home)?)
    }

    pub fn stage(prepared: PreparedSource) -> Result<Self, SessionError> {
        let PreparedSource {
            snapshot,
            r_home,
            mut target,
            options,
        } = prepared;
        let staged = stage_root(&snapshot, &r_home, &target.libraries)?;
        let staged_library = dunce::canonicalize(staged.library())?;
        target.libraries.insert(0, staged_library);
        let session = Session {
            r_home,
            target,
            root: snapshot.package().into(),
            options,
            root_description: Some(snapshot.description_source().into()),
            retained: None,
            primed: Mutex::new(None),
        };
        Ok(Self {
            session,
            inputs: SourceInputs { snapshot, staged },
        })
    }

    pub fn session(&self) -> &Session {
        &self.session
    }

    pub fn snapshot(&self) -> &SourcePackageSnapshot {
        &self.inputs.snapshot
    }

    fn into_build_context(self) -> BuildContext {
        BuildContext::new(
            self.inputs.snapshot,
            self.inputs.staged,
            self.session.r_home,
            self.session.target,
        )
        .with_worker_executable(self.session.options.worker_executable)
    }
}

fn installed_directory(directory: &Path) -> Result<(PathBuf, PackageName), SessionError> {
    let directory = dunce::canonicalize(directory)?;
    let name = directory
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            SessionError::InvalidRoot(format!(
                "{} has no package directory name",
                directory.display()
            ))
        })?
        .to_owned();
    let parent = directory
        .parent()
        .ok_or_else(|| {
            SessionError::InvalidRoot(format!("{} has no parent library", directory.display()))
        })?
        .to_path_buf();
    let description_path = directory.join("DESCRIPTION");
    let description = Description::parse(&std::fs::read_to_string(&description_path)?);
    match description.package() {
        Some(package) if package.as_str() == name => Ok((parent, name.into())),
        Some(package) => Err(SessionError::InvalidRoot(format!(
            "installed directory {} holds package `{}`, but a library directory must be named for its package",
            directory.display(),
            package.as_str()
        ))),
        None => Err(SessionError::InvalidRoot(format!(
            "{} has no Package field",
            description_path.display()
        ))),
    }
}

fn absolute_libraries(universe: &SessionOptions) -> io::Result<Vec<PathBuf>> {
    universe
        .libraries
        .iter()
        .map(|library| {
            if library.is_absolute() {
                Ok(library.clone())
            } else {
                Ok(std::env::current_dir()?.join(library))
            }
        })
        .collect()
}
