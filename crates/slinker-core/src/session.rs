use std::io;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::analysis::{LinkIr, Linker};
use crate::build::{BuildContext, OutputLease, PureRStatic, materialize};
use crate::cache::CacheLocation;
use crate::package::store::NativeSummaryManifest;
use crate::package::{PackageName, PackageStore};
use crate::source::{SourcePackageSnapshot, StagedRoot, stage_root};
use crate::worker::client::WorkerClient;
use crate::worker::service::WorkerService;
use crate::{Description, TargetEnvironment, TargetEnvironmentRequest};

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
    pub native_summaries: Option<PathBuf>,
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
    workers: Arc<WorkerService>,
    root: PackageName,
    options: SessionOptions,
    root_description: Option<Arc<str>>,
    retained: Option<SourceInputs>,
    native_summaries: Arc<NativeSummaryManifest>,
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
                Self::capture(r_home, name.clone(), libraries, universe.clone())
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
                Self::capture(r_home, name, libraries, universe.clone())
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
        libraries: Vec<PathBuf>,
        options: SessionOptions,
    ) -> Result<Self, SessionError> {
        let mut request = TargetEnvironmentRequest::new(r_home);
        request.libraries = libraries;
        request.worker_executable = options.worker_executable.clone();
        let native_summaries = Arc::new(NativeSummaryManifest::load(
            options.native_summaries.as_deref(),
        )?);
        let (_, client) = request.capture_worker()?;
        Ok(Self {
            workers: WorkerService::primed(options.threads.get(), client),
            root,
            options,
            root_description: None,
            retained: None,
            native_summaries,
        })
    }

    pub fn root(&self) -> &str {
        &self.root
    }

    pub fn target(&self) -> &TargetEnvironment {
        self.workers.target()
    }

    pub fn analyze(&self, provenance: bool) -> Result<LinkIr, SessionError> {
        let universe = &self.options;
        let store = PackageStore::for_session(
            &self.workers,
            universe.cache.clone(),
            Arc::clone(&self.native_summaries),
        )?;
        let mut linker = Linker::new(store, universe.threads.get())
            .with_external_packages(universe.external.iter().cloned())
            .with_linked_packages(universe.linked.iter().cloned());
        if !provenance {
            linker = linker.without_provenance();
        }
        if let Some(description) = &self.root_description {
            linker = linker.with_root_source(Arc::clone(description));
        }
        let ir = linker.analyze(&self.root)?;
        self.workers.check()?;
        Ok(ir)
    }
}

pub struct PreparedSource {
    snapshot: SourcePackageSnapshot,
    options: SessionOptions,
    client: WorkerClient,
    native_summaries: Arc<NativeSummaryManifest>,
}

impl PreparedSource {
    /// Stage the frozen source, analyze it, and publish a checked package.
    pub fn build(self, output: &Path) -> Result<(), SessionError> {
        let output = OutputLease::acquire(output)?;
        if let Ok(relative) = output.path().strip_prefix(self.snapshot.original_root())
            && self.snapshot.root().join(relative).exists()
        {
            return Err(SessionError::InvalidRoot("output is included in the frozen source; exclude it with .Rbuildignore or choose an output outside the source".into()));
        }
        let session = SourceSession::stage(self)?;
        let ir = session.session().analyze(false)?;
        let context = session.into_build_context();
        let buildable = PureRStatic::check(&ir, &context)?;
        materialize(buildable, output)?.publish()?;
        Ok(())
    }

    pub fn snapshot(&self) -> &SourcePackageSnapshot {
        &self.snapshot
    }

    pub fn target(&self) -> &TargetEnvironment {
        self.client.target()
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
        let native_summaries = Arc::new(NativeSummaryManifest::load(
            universe.native_summaries.as_deref(),
        )?);
        let mut request = TargetEnvironmentRequest::new(r_home);
        request.libraries = absolute_libraries(universe)?;
        request.worker_executable = universe.worker_executable.clone();
        let (target, client) = request.capture_worker()?;
        let snapshot = SourcePackageSnapshot::capture(path, &target.r_home)?;
        Ok(PreparedSource {
            snapshot,
            options: universe.clone(),
            client,
            native_summaries,
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
            options,
            mut client,
            native_summaries,
        } = prepared;
        let mut target = client.target().clone();
        let staged = stage_root(&snapshot, &target.r_home, &target.libraries)?;
        let staged_library = dunce::canonicalize(staged.library())?;
        target.libraries.insert(0, staged_library);
        let actual = client.configure_libraries(target.libraries.clone())?;
        if actual != target {
            return Err(crate::Error::Analysis(
                "target changed while freezing the staged library universe".into(),
            )
            .into());
        }
        let root = client
            .packages()
            .locate(snapshot.package())?
            .ok_or_else(|| {
                SessionError::InvalidRoot("staged Root is absent from its library universe".into())
            })?;
        let native_summaries = Arc::new(native_summaries.bind_root(
            root.identity,
            snapshot.fingerprint().clone(),
            target.target.clone(),
        )?);
        let session = Session {
            workers: WorkerService::primed(options.threads.get(), client),
            root: snapshot.package().into(),
            options,
            root_description: Some(
                std::fs::read_to_string(staged.package_root().join("DESCRIPTION"))?.into(),
            ),
            retained: None,
            native_summaries,
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
        BuildContext::new(self.inputs.staged, self.session.workers)
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
