//! Frozen source-package ingestion and target-R staging.

mod package;
mod staging;

pub use package::{FrozenSourceFiles, SourcePackageError, SourcePackageSnapshot};
pub use staging::{StagedRoot, StagingError, stage_root};
