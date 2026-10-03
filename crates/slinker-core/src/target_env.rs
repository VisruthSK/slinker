use crate::Error;
use crate::package::BindingName;
use crate::worker::client::WorkerClient;
use std::collections::BTreeSet;
use std::fmt;
use std::path::{Path, PathBuf};

/// Executable implementing slinker's `__r-worker` protocol.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum WorkerExecutable {
    /// The host dispatches `__r-worker` to `slinker_r_worker::run`.
    #[default]
    CurrentProcess,
    /// A separate slinker executable, for consumers that do not host the worker.
    Standalone(PathBuf),
}

impl WorkerExecutable {
    pub(crate) fn path(&self) -> crate::Result<PathBuf> {
        match self {
            Self::CurrentProcess => std::env::current_exe().map_err(|source| Error::Io {
                path: "<current-executable>".into(),
                source,
            }),
            Self::Standalone(path) => Ok(path.clone()),
        }
    }
}

pub fn r_executable(r_home: &Path) -> Option<PathBuf> {
    [
        r_home.join("bin/x64/R.exe"),
        r_home.join("bin/R.exe"),
        r_home.join("bin/R"),
    ]
    .into_iter()
    .find(|path| path.is_file())
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Target {
    pub r_version: String,
    pub os: String,
    pub arch: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TargetEnvironment {
    pub r_home: PathBuf,
    pub target: Target,
    pub libraries: Vec<PathBuf>,
    pub base_bindings: BTreeSet<BindingName>,
}

#[derive(Debug)]
pub struct PrimedWorker {
    pub(crate) client: WorkerClient,
    pub(crate) target: TargetEnvironment,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TargetEnvironmentRequest {
    pub r_home: PathBuf,
    pub libraries: Vec<PathBuf>,
    pub worker_executable: WorkerExecutable,
}

impl TargetEnvironmentRequest {
    pub fn new(r_home: impl Into<PathBuf>) -> Self {
        Self {
            r_home: r_home.into(),
            libraries: Vec::new(),
            worker_executable: WorkerExecutable::default(),
        }
    }

    pub fn capture(self) -> Result<TargetEnvironment, TargetEnvironmentError> {
        self.capture_primed().map(|(target, _)| target)
    }

    pub fn capture_primed(
        self,
    ) -> Result<(TargetEnvironment, PrimedWorker), TargetEnvironmentError> {
        let (target, client) =
            WorkerClient::capture_target(self.r_home, self.libraries, &self.worker_executable)
                .map_err(TargetEnvironmentError)?;
        let worker = PrimedWorker {
            client,
            target: target.clone(),
        };
        Ok((target, worker))
    }
}

#[derive(Debug)]
pub struct TargetEnvironmentError(Error);

impl fmt::Display for TargetEnvironmentError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "target-environment capture failed: {}", self.0)
    }
}

impl std::error::Error for TargetEnvironmentError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.0)
    }
}
