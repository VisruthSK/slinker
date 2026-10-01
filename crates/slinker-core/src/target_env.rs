use crate::Error;
use crate::worker::client::WorkerClient;
use std::collections::BTreeSet;
use std::fmt;
use std::path::{Path, PathBuf};

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
    pub base_bindings: BTreeSet<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TargetEnvironmentRequest {
    pub r_home: PathBuf,
    pub libraries: Vec<PathBuf>,
}

impl TargetEnvironmentRequest {
    pub fn new(r_home: impl Into<PathBuf>) -> Self {
        Self {
            r_home: r_home.into(),
            libraries: Vec::new(),
        }
    }

    pub fn capture(self) -> Result<TargetEnvironment, TargetEnvironmentError> {
        WorkerClient::capture_target(self.r_home, self.libraries).map_err(TargetEnvironmentError)
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

#[cfg(test)]
mod tests {
    use super::TargetEnvironmentRequest;
    use std::path::PathBuf;

    #[test]
    fn request_preserves_explicit_r_home_and_library_order() {
        let mut request = TargetEnvironmentRequest::new("/opt/R");
        request.libraries = vec![PathBuf::from("/first"), PathBuf::from("/second")];

        assert_eq!(request.r_home, PathBuf::from("/opt/R"));
        assert_eq!(
            request.libraries,
            vec![PathBuf::from("/first"), PathBuf::from("/second")]
        );
    }
}
