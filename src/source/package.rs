use crate::package::Digest;
use crate::{Description, Version};
use sha2::{Digest as Sha2Digest, Sha256};
use std::fs;
use std::io::{BufReader, Read};
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
    source_digest: Digest,
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
        let source_digest = digest_tree(&frozen)?;

        Ok(Self {
            original_root: source,
            package,
            version,
            source_digest,
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

    pub fn source_digest(&self) -> &Digest {
        &self.source_digest
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

fn copy_tree(source: &Path, target: &Path) -> Result<(), SourcePackageError> {
    fs::create_dir_all(target)?;
    let mut entries = fs::read_dir(source)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        let path = entry.path();
        let destination = target.join(entry.file_name());
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            copy_tree(&path, &destination)?;
        } else if file_type.is_file() {
            fs::copy(path, destination)?;
        } else {
            return Err(SourcePackageError::MissingInput(path));
        }
    }
    Ok(())
}

fn digest_tree(root: &Path) -> Result<Digest, SourcePackageError> {
    let mut files = Vec::new();
    collect_files(root, root, &mut files)?;
    files.sort_by(|left, right| left.0.cmp(&right.0));
    let mut hash = Sha256::new();
    hash.update(b"slinker-source-package-v1\0");
    for (relative, path) in files {
        hash.update(relative.as_bytes());
        hash.update([0]);
        let mut reader = BufReader::new(fs::File::open(path)?);
        let mut buffer = [0_u8; 128 * 1024];
        loop {
            let read = reader.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            hash.update(&buffer[..read]);
        }
        hash.update([0xff]);
    }
    Ok(Digest::finish(hash))
}

fn collect_files(
    root: &Path,
    directory: &Path,
    files: &mut Vec<(String, PathBuf)>,
) -> Result<(), std::io::Error> {
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            collect_files(root, &path, files)?;
        } else if entry.file_type()?.is_file() {
            files.push((
                path.strip_prefix(root)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .replace('\\', "/"),
                path,
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_is_immutable_after_source_changes() {
        let source = tempfile::tempdir().expect("source tempdir");
        fs::create_dir(source.path().join("R")).expect("R directory");
        fs::write(
            source.path().join("DESCRIPTION"),
            "Package: fixture\nVersion: 1.0.0\n",
        )
        .expect("DESCRIPTION");
        fs::write(source.path().join("NAMESPACE"), "export(f)\n").expect("NAMESPACE");
        fs::write(source.path().join("R/f.R"), "f <- function() 1L\n").expect("R source");
        let snapshot = SourcePackageSnapshot::capture(source.path()).expect("snapshot");

        fs::write(source.path().join("R/f.R"), "f <- function() 2L\n").expect("mutate source");

        assert_eq!(
            fs::read_to_string(snapshot.files().root().join("R/f.R")).expect("frozen source"),
            "f <- function() 1L\n"
        );
    }
}
