use std::ffi::OsStr;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::r_runtime;
use crate::target_env::Target;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RToolchain {
    rscript: PathBuf,
}

#[derive(Debug)]
pub enum ToolchainError {
    Io(std::io::Error),
    Failed {
        program: PathBuf,
        status: Option<i32>,
        stdout: String,
        stderr: String,
    },
    InvalidProbe(String),
}

impl RToolchain {
    pub fn from_r(r: impl Into<PathBuf>) -> Self {
        let r = r.into();
        let name = if cfg!(windows) { "Rscript.exe" } else { "Rscript" };
        let rscript = r
            .parent()
            .map(|parent| parent.join(name))
            .unwrap_or_else(|| PathBuf::from(name));
        Self { rscript }
    }

    pub fn rscript(&self) -> &Path {
        &self.rscript
    }

    pub fn probe(&self) -> Result<Target, ToolchainError> {
        const PROBE: &str = r#"
cat(paste0(R.version$major, ".", R.version$minor), "\n", sep = "")
cat(R.version$os, "\n", sep = "")
cat(R.version$arch, "\n", sep = "")
"#;

        // Avoid `-e` here. On Windows, the command-line expression path can be
        // materially less robust than executing a script file, while the rest
        // of heRmetic already relies on file-backed R helpers. Keep probing in
        // the same sanitized Rscript environment as those helpers.
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let probe_dir = std::env::temp_dir().join(format!(
            "hrm-probe-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(&probe_dir).map_err(ToolchainError::Io)?;
        let probe_path = probe_dir.join("probe.R");
        fs::write(&probe_path, PROBE).map_err(ToolchainError::Io)?;

        let result = self.run_rscript([probe_path.as_os_str()]);
        let _ = fs::remove_dir_all(&probe_dir);
        let output = result?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        let mut lines = stdout.lines();
        let r_version = lines.next().unwrap_or_default().trim().to_owned();
        let os = lines.next().unwrap_or_default().trim().to_owned();
        let arch = lines.next().unwrap_or_default().trim().to_owned();
        if r_version.is_empty() || os.is_empty() || arch.is_empty() {
            return Err(ToolchainError::InvalidProbe(stdout.into_owned()));
        }
        Ok(Target { r_version, os, arch })
    }

    pub(crate) fn rscript_command(&self) -> Command {
        let mut command = Command::new(&self.rscript);
        sanitize_r_environment(&mut command);
        command.arg("--vanilla");
        command
    }

    /// Run an R helper in the host library environment while still disabling
    /// user startup files. This is reserved for target discovery, where the installed library
    /// universe is itself the input being inspected.
    pub(crate) fn host_rscript_command(&self) -> Command {
        let mut command = Command::new(&self.rscript);
        sanitize_r_startup(&mut command);
        command.arg("--vanilla");
        command
    }

    pub(crate) fn run_rscript<I, S>(&self, args: I) -> Result<Output, ToolchainError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let mut command = self.rscript_command();
        command.args(args);
        checked_output(&mut command)
    }

    pub(crate) fn run_rscript_host<I, S>(&self, args: I) -> Result<Output, ToolchainError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let mut command = self.host_rscript_command();
        command.args(args);
        checked_output(&mut command)
    }

    pub(crate) fn run_runtime<I, S>(
        &self,
        work_dir: &Path,
        command_name: &str,
        args: I,
        host_environment: bool,
    ) -> Result<Output, ToolchainError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let runtime = r_runtime::prepare(work_dir).map_err(ToolchainError::Io)?;
        let mut command = if host_environment {
            self.host_rscript_command()
        } else {
            self.rscript_command()
        };
        command
            .arg(&runtime.runner)
            .arg(&runtime.package_root)
            .arg(command_name)
            .args(args);
        checked_output(&mut command)
    }
}

pub(crate) fn sanitize_r_startup(command: &mut Command) {
    command
        .env("R_ENVIRON_USER", "")
        .env("R_PROFILE_USER", "")
        .env("R_DEFAULT_PACKAGES", "NULL");
}

pub(crate) fn sanitize_r_environment(command: &mut Command) {
    sanitize_r_startup(command);
    command
        .env("R_LIBS", "")
        .env("R_LIBS_SITE", "")
        .env("R_LIBS_USER", "");
}

pub(crate) fn checked_output(command: &mut Command) -> Result<Output, ToolchainError> {
    let program = PathBuf::from(command.get_program());
    let output = command.output().map_err(ToolchainError::Io)?;
    if output.status.success() {
        Ok(output)
    } else {
        Err(ToolchainError::Failed {
            program,
            status: output.status.code(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }
}

impl fmt::Display for ToolchainError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "failed to run R toolchain: {error}"),
            Self::Failed { program, status, stdout, stderr } => {
                let stdout = stdout.trim();
                let stderr = stderr.trim();
                if stdout.is_empty() {
                    write!(f, "{} exited with {:?}: {}", program.display(), status, stderr)
                } else if stderr.is_empty() {
                    write!(f, "{} exited with {:?}: {}", program.display(), status, stdout)
                } else {
                    write!(f, "{} exited with {:?}: {}\n{}", program.display(), status, stderr, stdout)
                }
            }
            Self::InvalidProbe(stdout) => write!(f, "invalid R toolchain probe output: {stdout:?}"),
        }
    }
}

impl std::error::Error for ToolchainError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}
