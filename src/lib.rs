#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("I/O error at {}: {source}", path.display())]
    Io {
        path: std::path::PathBuf,
        source: std::io::Error,
    },
    #[error("metadata error at {}: {message}", path.display())]
    Metadata {
        path: std::path::PathBuf,
        message: String,
    },
    #[error("{0}")]
    Analysis(String),
}

pub type Result<T> = std::result::Result<T, Error>;

mod metadata;
pub mod r_worker;
mod target_env;

#[cfg(feature = "air")]
pub mod analysis;
#[cfg(feature = "air")]
pub mod build;
pub mod cache;
#[cfg(feature = "air")]
pub mod ir;
#[cfg(feature = "air")]
pub mod package;
#[cfg(feature = "air")]
pub mod source;
#[cfg(feature = "air")]
pub mod syntax;

pub use metadata::{Description, Relation, RelationField, Version};
pub use target_env::{
    Target, TargetEnvironment, TargetEnvironmentError, TargetEnvironmentRequest, r_executable,
};
