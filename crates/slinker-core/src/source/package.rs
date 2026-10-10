use crate::{Description, Version};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use tempfile::TempDir;
use thiserror::Error;

#[derive(Debug)]
pub struct SourcePackageSnapshot {
    original_root: PathBuf,
    package: String,
    version: Version,
    description: Description,
    description_source: Arc<str>,
    namespace: Arc<str>,
    root: PathBuf,
    _owner: TempDir,
    fingerprint: crate::package::Digest,
}

impl SourcePackageSnapshot {
    pub fn capture(path: impl AsRef<Path>, r_home: &Path) -> Result<Self, SourcePackageError> {
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

        let owner = tempfile::Builder::new()
            .prefix("slinker-source-")
            .tempdir()?;
        reject_unsupported_entries(&source)?;
        let executable = crate::r_executable(r_home).ok_or_else(|| {
            SourcePackageError::Build(format!(
                "selected R has no executable under {}",
                r_home.display()
            ))
        })?;
        let original_description = fs::read_to_string(source.join("DESCRIPTION"))?;
        let description = Description::parse(&original_description);
        let package = description
            .package()
            .ok_or(SourcePackageError::MissingDescriptionField("Package"))?
            .as_str()
            .to_owned();
        let version = description
            .version_parsed()
            .ok_or(SourcePackageError::MissingDescriptionField("Version"))?
            .map_err(|error| SourcePackageError::InvalidVersion(error.to_string()))?;
        let build = Command::new(&executable)
            .args([
                "CMD",
                "build",
                "--no-build-vignettes",
                "--no-manual",
                "--no-resave-data",
                "--no-clean",
            ])
            .arg(&source)
            .current_dir(owner.path())
            .env_remove("R_HOME")
            .output()?;
        if !build.status.success() {
            return Err(SourcePackageError::Build(format!(
                "{}\n{}",
                String::from_utf8_lossy(&build.stdout),
                String::from_utf8_lossy(&build.stderr)
            )));
        }
        let archive = fs::read_dir(owner.path())?
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .map(|entry| entry.path())
            .find(|path| path.extension().is_some_and(|extension| extension == "gz"))
            .ok_or_else(|| {
                SourcePackageError::Build("R CMD build produced no source archive".into())
            })?;
        let unpack = owner.path().join("unpack.R");
        fs::write(
            &unpack,
            "args <- commandArgs(TRUE)\nutils::untar(args[[1L]], exdir = args[[2L]], tar = 'internal')\n",
        )?;
        let destination = owner.path().join("source");
        fs::create_dir(&destination)?;
        let extract = Command::new(executable)
            .args(["--vanilla", "--slave", "-f"])
            .arg(unpack)
            .arg("--args")
            .arg(&archive)
            .arg(&destination)
            .env_remove("R_HOME")
            .output()?;
        if !extract.status.success() {
            return Err(SourcePackageError::Build(
                String::from_utf8_lossy(&extract.stderr).into_owned(),
            ));
        }
        let frozen = destination.join(&package);
        // R owns source selection; keep the author's DESCRIPTION rather than build timestamps.
        fs::write(frozen.join("DESCRIPTION"), &original_description)?;
        let namespace_path = frozen.join("NAMESPACE");
        let namespace =
            fs::read_to_string(&namespace_path).map_err(|source| SourcePackageError::Read {
                path: namespace_path,
                source,
            })?;

        let fingerprint = crate::package::tree_digest(&frozen)
            .map_err(|error| SourcePackageError::Build(error.to_string()))?;
        Ok(Self {
            original_root: source,
            package,
            version,
            description,
            description_source: original_description.into(),
            namespace: namespace.into(),
            fingerprint,
            root: frozen,
            _owner: owner,
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

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn fingerprint(&self) -> &crate::package::Digest {
        &self.fingerprint
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
    #[error("target-R source build failed: {0}")]
    Build(String),
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

fn reject_unsupported_entries(source: &Path) -> Result<(), SourcePackageError> {
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            reject_unsupported_entries(&path)?;
        } else if !file_type.is_file() {
            return Err(SourcePackageError::UnsupportedEntry(path));
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "../../tests/unit/source/package.rs"]
mod tests;
