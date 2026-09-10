use std::ffi::OsStr;
use std::io::{BufRead, BufReader, Read, Write};
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Output, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};
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
    Runtime {
        program: PathBuf,
        message: String,
    },
    Timeout {
        program: PathBuf,
        seconds: u64,
        context: String,
    },
}

pub(crate) struct RRuntimeServer {
    program: PathBuf,
    child: Child,
    stdin: Option<ChildStdin>,
    responses: Receiver<String>,
}

impl RRuntimeServer {
    pub(crate) fn request(
        &mut self,
        command: &str,
        manifest: &Path,
        seconds: u64,
        context: impl Into<String>,
    ) -> Result<(), ToolchainError> {
        let context = context.into();
        let stdin = self.stdin.as_mut().ok_or_else(|| ToolchainError::Runtime {
            program: self.program.clone(),
            message: "persistent R runtime stdin is closed".into(),
        })?;
        let path = manifest.to_string_lossy();
        writeln!(stdin, "{command}\t{}", encode_hex(path.as_bytes())).map_err(ToolchainError::Io)?;
        stdin.flush().map_err(ToolchainError::Io)?;

        match self.responses.recv_timeout(Duration::from_secs(seconds)) {
            Ok(response) if response == "OK" => Ok(()),
            Ok(response) if response.starts_with("ERROR\t") => {
                let encoded = &response[6..];
                let message = decode_hex(encoded).unwrap_or_else(|| format!("invalid encoded R runtime error: {encoded}"));
                Err(ToolchainError::Runtime { program: self.program.clone(), message })
            }
            Ok(response) => Err(ToolchainError::Runtime {
                program: self.program.clone(),
                message: format!("invalid persistent R runtime response: {response:?}"),
            }),
            Err(RecvTimeoutError::Timeout) => {
                let _ = self.child.kill();
                let _ = self.child.wait();
                self.stdin.take();
                Err(ToolchainError::Timeout { program: self.program.clone(), seconds, context })
            }
            Err(RecvTimeoutError::Disconnected) => {
                let status = self.child.try_wait().ok().flatten().and_then(|status| status.code());
                self.stdin.take();
                Err(ToolchainError::Runtime {
                    program: self.program.clone(),
                    message: format!("persistent R runtime terminated unexpectedly with status {status:?}"),
                })
            }
        }
    }
}

impl Drop for RRuntimeServer {
    fn drop(&mut self) {
        if let Some(mut stdin) = self.stdin.take() {
            let _ = writeln!(stdin, "STOP");
            let _ = stdin.flush();
        }
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            match self.child.try_wait() {
                Ok(Some(_)) => return,
                Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(20)),
                _ => break,
            }
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
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
        // of slinker already relies on file-backed R helpers. Keep probing in
        // the same sanitized Rscript environment as those helpers.
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let probe_dir = std::env::temp_dir().join(format!(
            "slinker-probe-{}-{nonce}",
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

    pub(crate) fn run_runtime_timeout<I, S>(
        &self,
        work_dir: &Path,
        command_name: &str,
        args: I,
        host_environment: bool,
        seconds: u64,
        context: impl Into<String>,
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
        checked_output_timeout(&mut command, seconds, context.into())
    }

    pub(crate) fn spawn_runtime_server(
        &self,
        work_dir: &Path,
        jobs: usize,
    ) -> Result<RRuntimeServer, ToolchainError> {
        let runtime = r_runtime::prepare(work_dir).map_err(ToolchainError::Io)?;
        let mut command = self.rscript_command();
        command
            .arg(&runtime.runner)
            .arg(&runtime.package_root)
            .arg("serve")
            .arg(jobs.max(1).to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());

        let program = PathBuf::from(command.get_program());
        let mut child = command.spawn().map_err(ToolchainError::Io)?;
        let stdin = child.stdin.take().ok_or_else(|| ToolchainError::Runtime {
            program: program.clone(),
            message: "failed to open persistent R runtime stdin".into(),
        })?;
        let stdout = child.stdout.take().ok_or_else(|| ToolchainError::Runtime {
            program: program.clone(),
            message: "failed to open persistent R runtime stdout".into(),
        })?;
        let (sender, responses) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if sender.send(line).is_err() {
                    break;
                }
            }
        });

        Ok(RRuntimeServer { program, child, stdin: Some(stdin), responses })
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

pub(crate) fn checked_output_timeout(
    command: &mut Command,
    seconds: u64,
    context: String,
) -> Result<Output, ToolchainError> {
    let program = PathBuf::from(command.get_program());
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = command.spawn().map_err(ToolchainError::Io)?;
    let mut stdout = child.stdout.take().expect("piped stdout");
    let mut stderr = child.stderr.take().expect("piped stderr");
    let stdout_reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = stdout.read_to_end(&mut bytes);
        bytes
    });
    let stderr_reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = stderr.read_to_end(&mut bytes);
        bytes
    });
    let deadline = Instant::now() + Duration::from_secs(seconds);
    let status = loop {
        if let Some(status) = child.try_wait().map_err(ToolchainError::Io)? {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            let _ = stdout_reader.join();
            let _ = stderr_reader.join();
            return Err(ToolchainError::Timeout { program, seconds, context });
        }
        thread::sleep(Duration::from_millis(25));
    };
    let stdout = stdout_reader.join().unwrap_or_default();
    let stderr = stderr_reader.join().unwrap_or_default();
    let output = Output { status, stdout, stderr };
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


fn encode_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

fn decode_hex(value: &str) -> Option<String> {
    if value.len() % 2 != 0 {
        return None;
    }
    let mut bytes = Vec::with_capacity(value.len() / 2);
    for pair in value.as_bytes().chunks_exact(2) {
        let high = hex_nibble(pair[0])?;
        let low = hex_nibble(pair[1])?;
        bytes.push((high << 4) | low);
    }
    String::from_utf8(bytes).ok()
}

fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
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
            Self::Runtime { program, message } => write!(f, "{}: {message}", program.display()),
            Self::Timeout { program, seconds, context } => write!(f, "{} timed out after {seconds}s ({context})", program.display()),
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
