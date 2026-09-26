//! Frozen source-package ingestion and target-R staging.

mod description;
mod package;
mod staging;

pub use description::generated_description;
pub use package::{FrozenSourceFiles, SourcePackageError, SourcePackageSnapshot};
pub use staging::{StagedRoot, StagingError, stage_root};
