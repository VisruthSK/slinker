use crate::{Description, Version};
use regex::{Regex, RegexBuilder};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tempfile::TempDir;
use thiserror::Error;

#[derive(Debug)]
pub struct FrozenSourceFiles {
    root: PathBuf,
    _owner: Arc<TempDir>,
}

impl FrozenSourceFiles {
    pub fn root(&self) -> &Path {
        &self.root
    }
}

#[derive(Debug)]
pub struct SourcePackageSnapshot {
    original_root: PathBuf,
    package: String,
    version: Version,
    description: Description,
    description_source: Arc<str>,
    namespace: Arc<str>,
    files: FrozenSourceFiles,
}

impl SourcePackageSnapshot {
    pub fn capture(path: impl AsRef<Path>) -> Result<Self, SourcePackageError> {
        let source = dunce::canonicalize(path.as_ref()).map_err(|source_error| {
            SourcePackageError::Canonicalize {
                path: path.as_ref().to_path_buf(),
                source: source_error,
            }
        })?;
        for required in ["DESCRIPTION", "NAMESPACE", "R"] {
            let candidate = source.join(required);
            let valid = if required == "R" {
                candidate.is_dir()
            } else {
                candidate.is_file()
            };
            if !valid {
                return Err(SourcePackageError::MissingInput(candidate));
            }
        }

        let owner = Arc::new(
            tempfile::Builder::new()
                .prefix("slinker-source-")
                .tempdir()?,
        );
        let frozen = owner.path().join("package");
        copy_tree(&source, &frozen)?;
        let description_path = frozen.join("DESCRIPTION");
        let description_text =
            fs::read_to_string(&description_path).map_err(|source| SourcePackageError::Read {
                path: description_path.clone(),
                source,
            })?;
        let description = Description::parse(&description_text);
        let package = description
            .package()
            .ok_or(SourcePackageError::MissingDescriptionField("Package"))?
            .as_str()
            .to_owned();
        let version = description
            .version_parsed()
            .ok_or(SourcePackageError::MissingDescriptionField("Version"))?
            .map_err(|error| SourcePackageError::InvalidVersion(error.to_string()))?;
        let namespace_path = frozen.join("NAMESPACE");
        let namespace =
            fs::read_to_string(&namespace_path).map_err(|source| SourcePackageError::Read {
                path: namespace_path,
                source,
            })?;

        Ok(Self {
            original_root: source,
            package,
            version,
            description,
            description_source: description_text.into(),
            namespace: namespace.into(),
            files: FrozenSourceFiles {
                root: frozen,
                _owner: owner,
            },
        })
    }

    pub fn original_root(&self) -> &Path {
        &self.original_root
    }

    pub fn package(&self) -> &str {
        &self.package
    }

    pub fn version(&self) -> &Version {
        &self.version
    }

    pub fn description(&self) -> &Description {
        &self.description
    }

    pub fn description_source(&self) -> &str {
        &self.description_source
    }

    pub fn namespace(&self) -> &str {
        &self.namespace
    }

    pub fn files(&self) -> &FrozenSourceFiles {
        &self.files
    }
}

#[derive(Debug, Error)]
pub enum SourcePackageError {
    #[error("failed to canonicalize source package path {path}: {source}")]
    Canonicalize {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("source package is missing required input {0}")]
    MissingInput(PathBuf),
    #[error("source package contains {0}, which is neither a regular file nor a directory")]
    UnsupportedEntry(PathBuf),
    #[error("source package .Rbuildignore pattern `{pattern}` is not supported: {message}")]
    InvalidBuildIgnore { pattern: String, message: String },
    #[error("source package DESCRIPTION is missing {0}")]
    MissingDescriptionField(&'static str),
    #[error("source package DESCRIPTION has invalid Version: {0}")]
    InvalidVersion(String),
    #[error("failed to read {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

const EXCLUDED_ROOT_ENTRIES: [&str; 3] = [".git", "target", "renv"];

struct BuildIgnore(Vec<Regex>);

impl BuildIgnore {
    fn load(root: &Path) -> Result<Self, SourcePackageError> {
        let path = root.join(".Rbuildignore");
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(source) => return Err(SourcePackageError::Read { path, source }),
        };
        text.lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(|pattern| {
                RegexBuilder::new(pattern)
                    .case_insensitive(true)
                    .build()
                    .map_err(|error| SourcePackageError::InvalidBuildIgnore {
                        pattern: pattern.to_owned(),
                        message: error.to_string(),
                    })
            })
            .collect::<Result<_, _>>()
            .map(Self)
    }

    fn excludes(&self, relative: &str) -> bool {
        self.0.iter().any(|pattern| pattern.is_match(relative))
    }
}

fn copy_tree(source: &Path, target: &Path) -> Result<(), SourcePackageError> {
    let ignore = BuildIgnore::load(source)?;
    copy_directory(source, target, "", &ignore)
}

fn copy_directory(
    source: &Path,
    target: &Path,
    relative: &str,
    ignore: &BuildIgnore,
) -> Result<(), SourcePackageError> {
    fs::create_dir_all(target)?;
    let mut entries = fs::read_dir(source)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        let name = entry.file_name();
        let name_text = name.to_string_lossy();
        let entry_relative = if relative.is_empty() {
            name_text.to_string()
        } else {
            format!("{relative}/{name_text}")
        };
        let excluded_at_root =
            relative.is_empty() && EXCLUDED_ROOT_ENTRIES.contains(&name_text.as_ref());
        if excluded_at_root || ignore.excludes(&entry_relative) {
            continue;
        }
        let path = entry.path();
        let destination = target.join(&name);
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            copy_directory(&path, &destination, &entry_relative, ignore)?;
        } else if file_type.is_file() {
            fs::copy(path, destination)?;
        } else {
            return Err(SourcePackageError::UnsupportedEntry(path));
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "../../tests/unit/source/package.rs"]
mod tests;
