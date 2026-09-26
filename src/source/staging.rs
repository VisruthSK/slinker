use crate::source::SourcePackageSnapshot;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use tempfile::TempDir;
use thiserror::Error;

/// Privately installed root image analyzed as the Root package.
#[derive(Debug)]
pub struct StagedRoot {
    library: PathBuf,
    package_root: PathBuf,
    _owner: TempDir,
}

impl StagedRoot {
    /// Private staging library.
    pub fn library(&self) -> &Path {
        &self.library
    }

    /// Exact installed root package directory.
    pub fn package_root(&self) -> &Path {
        &self.package_root
    }
}

/// Stage-install a frozen root source package with the selected target R.
pub fn stage_root(
    snapshot: &SourcePackageSnapshot,
    r_home: &Path,
    libraries: &[PathBuf],
) -> Result<StagedRoot, StagingError> {
    let owner = tempfile::Builder::new()
        .prefix("slinker-stage-")
        .tempdir()?;
    let library = owner.path().join("library");
    std::fs::create_dir(&library)?;
    let executable = r_executable(r_home).ok_or_else(|| StagingError::MissingR(r_home.into()))?;
    let mut command = Command::new(executable);
    command
        .args(["CMD", "INSTALL", "--no-test-load"])
        .arg(format!("--library={}", library.display()))
        .arg(snapshot.files().root())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env(
            "R_ENVIRON_USER",
            r_home.join("etc/__slinker_no_user_Renviron__"),
        )
        .env_remove("R_HOME")
        .env_remove("R_PROFILE_USER")
        .env_remove("R_LIBS_USER")
        .env_remove("R_LIBS_SITE");
    let ordered = std::iter::once(library.as_path())
        .chain(libraries.iter().map(PathBuf::as_path))
        .map(|path| path.to_string_lossy())
        .collect::<Vec<_>>()
        .join(if cfg!(windows) { ";" } else { ":" });
    command.env("R_LIBS", ordered);
    let output = command.output()?;
    if !output.status.success() {
        return Err(StagingError::Install {
            status: output.status.to_string(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        });
    }
    let package_root = library.join(snapshot.package());
    if !package_root.join("DESCRIPTION").is_file() {
        return Err(StagingError::MissingInstalledRoot(package_root));
    }
    Ok(StagedRoot {
        library,
        package_root,
        _owner: owner,
    })
}

#[derive(Debug, Error)]
pub enum StagingError {
    #[error("selected R installation has no executable under {0}")]
    MissingR(PathBuf),
    #[error("target-R staging install failed ({status})\nstdout:\n{stdout}\nstderr:\n{stderr}")]
    Install {
        status: String,
        stdout: String,
        stderr: String,
    },
    #[error("target-R staging did not create installed root at {0}")]
    MissingInstalledRoot(PathBuf),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

fn r_executable(r_home: &Path) -> Option<PathBuf> {
    [
        r_home.join("bin/x64/R.exe"),
        r_home.join("bin/R.exe"),
        r_home.join("bin/R"),
    ]
    .into_iter()
    .find(|path| path.is_file())
}
