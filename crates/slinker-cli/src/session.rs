use std::error::Error;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use slinker_core::analysis::{LinkIr, Linker};
use slinker_core::build::BuildContext;
use slinker_core::cache::CacheLocation;
use slinker_core::package::PackageStore;
use slinker_core::source::{SourcePackageSnapshot, StagedRoot, stage_root};
use slinker_core::{Description, PrimedWorker, TargetEnvironment, TargetEnvironmentRequest};

use crate::UniverseArgs;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RootSpec {
    Source(PathBuf),
    Installed(String),
    InstalledDirectory(PathBuf),
}

impl RootSpec {
    pub fn parse(value: &str) -> Result<Self, &'static str> {
        if value.is_empty() {
            return Err("expected a package name or path");
        }
        if !value.contains(['/', '\\']) && value != "." && value != ".." {
            return Ok(Self::Installed(value.to_owned()));
        }
        let path = PathBuf::from(value);
        if path.join("Meta").join("package.rds").is_file() {
            Ok(Self::InstalledDirectory(path))
        } else {
            Ok(Self::Source(path))
        }
    }
}

pub struct SourceInputs {
    snapshot: SourcePackageSnapshot,
    staged: StagedRoot,
}

pub struct Session {
    r_home: PathBuf,
    target: TargetEnvironment,
    root: String,
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
        universe: &UniverseArgs,
        r_home: PathBuf,
    ) -> Result<Self, Box<dyn Error>> {
        match root {
            RootSpec::Installed(name) => {
                let libraries = absolute_libraries(universe)?;
                Self::capture(r_home, name.clone(), None, libraries)
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
                Self::capture(r_home, name, None, libraries)
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
        root: String,
        root_description: Option<Arc<str>>,
        libraries: Vec<PathBuf>,
    ) -> Result<Self, Box<dyn Error>> {
        let mut request = TargetEnvironmentRequest::new(r_home.clone());
        request.libraries = libraries;
        let (target, primed) = request.capture_primed()?;
        Ok(Self {
            target,
            r_home,
            root,
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

    pub fn analyze(
        &self,
        universe: &UniverseArgs,
        provenance: bool,
    ) -> Result<LinkIr, Box<dyn Error>> {
        let store = PackageStore::new(self.r_home.clone(), self.target.clone(), cache_location())?
            .with_worker_limit(universe.threads.get());
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
}

impl PreparedSource {
    pub fn snapshot(&self) -> &SourcePackageSnapshot {
        &self.snapshot
    }

    pub fn target(&self) -> &TargetEnvironment {
        &self.target
    }
}

impl SourceSession {
    pub fn prepare(
        path: &Path,
        universe: &UniverseArgs,
        r_home: PathBuf,
    ) -> Result<PreparedSource, Box<dyn Error>> {
        let snapshot = SourcePackageSnapshot::capture(path)?;
        let mut request = TargetEnvironmentRequest::new(r_home.clone());
        request.libraries = absolute_libraries(universe)?;
        Ok(PreparedSource {
            snapshot,
            r_home,
            target: request.capture()?,
        })
    }

    pub fn open(
        path: &Path,
        universe: &UniverseArgs,
        r_home: PathBuf,
    ) -> Result<Self, Box<dyn Error>> {
        Self::stage(Self::prepare(path, universe, r_home)?)
    }

    pub fn stage(prepared: PreparedSource) -> Result<Self, Box<dyn Error>> {
        let PreparedSource {
            snapshot,
            r_home,
            mut target,
        } = prepared;
        let staged = stage_root(&snapshot, &r_home, &target.libraries)?;
        let staged_library = dunce::canonicalize(staged.library())?;
        target.libraries.insert(0, staged_library);
        let session = Session {
            r_home,
            target,
            root: snapshot.package().to_owned(),
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

    pub fn into_build_context(self) -> BuildContext {
        BuildContext::new(
            self.inputs.snapshot,
            self.inputs.staged,
            self.session.r_home,
            self.session.target,
        )
    }
}

fn installed_directory(directory: &Path) -> Result<(PathBuf, String), Box<dyn Error>> {
    let directory = dunce::canonicalize(directory)?;
    let name = directory
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| format!("{} has no package directory name", directory.display()))?
        .to_owned();
    let parent = directory
        .parent()
        .ok_or_else(|| format!("{} has no parent library", directory.display()))?
        .to_path_buf();
    let description_path = directory.join("DESCRIPTION");
    let description = Description::parse(&std::fs::read_to_string(&description_path)?);
    match description.package() {
        Some(package) if package.as_str() == name => Ok((parent, name)),
        Some(package) => Err(format!(
            "installed directory {} holds package `{}`, but a library directory must be named for its package",
            directory.display(),
            package.as_str()
        )
        .into()),
        None => Err(format!("{} has no Package field", description_path.display()).into()),
    }
}

pub fn absolute_libraries(universe: &UniverseArgs) -> io::Result<Vec<PathBuf>> {
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

pub(crate) fn cache_location() -> CacheLocation {
    std::env::var_os("SLINKER_CACHE_DIR").map_or(CacheLocation::Default, |root| {
        CacheLocation::Directory(PathBuf::from(root))
    })
}
