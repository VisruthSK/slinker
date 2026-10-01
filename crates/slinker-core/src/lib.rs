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
mod target_env;
pub mod worker;

pub mod analysis;
pub mod build;
pub mod cache;
pub mod ir;
pub mod package;
pub mod source;
pub mod syntax;

pub use metadata::{Description, Relation, RelationField, Version};
pub use target_env::{
    Target, TargetEnvironment, TargetEnvironmentError, TargetEnvironmentRequest, r_executable,
};
