//! Demand-driven linker for installed R package images.
//!
//! `slinker` analyzes the exact installed package images selected by a target R
//! library universe. Air supplies syntax; slinker owns semantic reachability,
//! provenance, optional-package specialization, and rewrite planning.

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

mod metadata;
mod r_runtime;
mod target_env;
mod toolchain;

#[cfg(feature = "air")]
pub mod analysis;
#[cfg(feature = "air")]
pub mod build;
#[cfg(feature = "air")]
pub mod package;
#[cfg(feature = "air")]
pub mod syntax;

pub use metadata::{Dependency, Description, MetadataError};
pub use target_env::{Target, TargetEnvironment, TargetEnvironmentError, TargetEnvironmentRequest};
pub use toolchain::{RToolchain, ToolchainError};
