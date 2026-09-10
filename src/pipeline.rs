use std::fmt;

use crate::{
    air::{scan_configured, PackageScanError, SourceFileFacts},
    InspectError, RToolchain, SemanticSnapshot, StageError, StageRequest, StagedPackage,
};

#[derive(Clone, Debug)]
pub struct PreparedPackage {
    pub staged: StagedPackage,
    pub source: Vec<SourceFileFacts>,
    pub semantic: SemanticSnapshot,
}

#[derive(Debug)]
pub enum PrepareError {
    Stage(StageError),
    Scan(PackageScanError),
    Inspect(InspectError),
}

impl RToolchain {
    /// Run the three evidence-producing front-end phases in order:
    /// target-toolchain staging, Air analysis of the configured source view,
    /// and isolated pre-activation inspection of installed semantic state.
    pub fn prepare_package(&self, request: &StageRequest) -> Result<PreparedPackage, PrepareError> {
        let staged = self.stage(request).map_err(PrepareError::Stage)?;
        let source = scan_configured(&staged.configured).map_err(PrepareError::Scan)?;
        let semantic = self
            .inspect_staged_snapshot(&staged)
            .map_err(PrepareError::Inspect)?;
        Ok(PreparedPackage { staged, source, semantic })
    }
}

impl fmt::Display for PrepareError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Stage(error) => write!(f, "package staging failed: {error}"),
            Self::Scan(error) => write!(f, "configured-source analysis failed: {error}"),
            Self::Inspect(error) => write!(f, "installed-state inspection failed: {error}"),
        }
    }
}

impl std::error::Error for PrepareError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Stage(error) => Some(error),
            Self::Scan(error) => Some(error),
            Self::Inspect(error) => Some(error),
        }
    }
}
