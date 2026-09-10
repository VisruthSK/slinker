//! Installed-image analysis and materialization primitives for heRmetic.
//!
//! `hrm` links the exact package images selected by an R library universe. Air
//! analyzes installed closure bodies; heRmetic owns reachability, rewrite planning,
//! and construction of canonical synthetic package environments.

#[derive(Debug)]
pub enum Error {
    Io { path: std::path::PathBuf, source: std::io::Error },
    Metadata { path: std::path::PathBuf, source: MetadataError },
    Parse { path: String, message: String },
    Analysis(String),
}

pub type Result<T> = std::result::Result<T, Error>;

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io { path, source } => write!(f, "I/O error at {}: {source}", path.display()),
            Self::Metadata { path, source } => write!(f, "metadata error at {}: {source}", path.display()),
            Self::Parse { path, message } => write!(f, "parse error in {path}: {message}"),
            Self::Analysis(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Metadata { source, .. } => Some(source),
            Self::Parse { .. } | Self::Analysis(_) => None,
        }
    }
}

mod inspection;
mod materialize;
mod metadata;
mod target_env;
mod toolchain;


#[cfg(feature = "air")]
pub mod analysis;


pub use inspection::{
    BindingOrigin, ClosureEnvironment, ExportedBinding, ImportDirective, ImportedBinding, InspectError,
    ObjectIssue, ObjectState, S3Registration, SemanticSnapshot, SemanticState,
};
pub use materialize::{
    MaterializationRequest, MaterializeError, MaterializedArtifact, PackageMaterialization,
};
pub use metadata::{Dependency, Description, MetadataError};
pub use target_env::{
    InstalledPackage, Target, TargetEnvironment, TargetEnvironmentError, TargetEnvironmentRequest,
};
pub use toolchain::{RToolchain, ToolchainError};
