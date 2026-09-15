use crate::package::InstalledPackage;
use crate::r_worker::protocol::{
    PROTOCOL_VERSION, PackageSpec, TargetSpec, WorkerRequest, WorkerResponse,
};
use crate::{Error, Result, Target, TargetEnvironment};
use std::collections::BTreeSet;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

pub(crate) struct WorkerClient {
    child: Child,
    input: BufWriter<ChildStdin>,
    output: BufReader<File>,
    protocol_path: PathBuf,
    next_request: u64,
}

impl WorkerClient {
    pub(crate) fn spawn(r_home: std::path::PathBuf, target: &TargetEnvironment) -> Result<Self> {
        let (client, actual) = Self::connect(r_home, target.libraries.clone())?;
        if actual != *target {
            return Err(Error::Analysis(
                "Harp worker target changed between discovery and analysis".into(),
            ));
        }
        Ok(client)
    }

    pub(crate) fn capture_target(
        r_home: std::path::PathBuf,
        libraries: Vec<std::path::PathBuf>,
    ) -> Result<TargetEnvironment> {
        let (_, target) = Self::connect(r_home, libraries)?;
        Ok(target)
    }

    fn connect(
        r_home: std::path::PathBuf,
        libraries: Vec<std::path::PathBuf>,
    ) -> Result<(Self, TargetEnvironment)> {
        let executable = std::env::current_exe().map_err(|source| Error::Io {
            path: "<current-executable>".into(),
            source,
        })?;
        let protocol_path = protocol_path();
        let protocol_file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&protocol_path)
            .map_err(|source| Error::Io {
                path: protocol_path.clone(),
                source,
            })?;
        let mut command = Command::new(&executable);
        command
            .arg("__r-worker")
            .arg(&protocol_path)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit());
        for variable in [
            "R_ENVIRON_USER",
            "R_PROFILE_USER",
            "R_LIBS",
            "R_LIBS_USER",
            "R_LIBS_SITE",
        ] {
            command.env_remove(variable);
        }
        let mut child = command.spawn().map_err(|source| Error::Io {
            path: executable,
            source,
        })?;
        let input = child
            .stdin
            .take()
            .ok_or_else(|| Error::Analysis("failed to open Harp worker request stream".into()))?;
        let mut client = Self {
            child,
            input: BufWriter::new(input),
            output: BufReader::new(protocol_file),
            protocol_path,
            next_request: 1,
        };
        let response = client.exchange(&WorkerRequest::Hello {
            protocol: PROTOCOL_VERSION,
            target: TargetSpec {
                r_home,
                arch: worker_arch().into(),
                libraries,
            },
        })?;
        match response {
            WorkerResponse::Hello {
                protocol: PROTOCOL_VERSION,
                harp_worker: true,
                target,
            } => Ok((
                client,
                TargetEnvironment {
                    r_home: target.r_home,
                    target: Target {
                        r_version: target.r_version,
                        os: target.os,
                        arch: target.arch,
                    },
                    libraries: target.libraries,
                    base_bindings: target.base_bindings.into_iter().collect::<BTreeSet<_>>(),
                },
            )),
            response => Err(worker_error("worker startup", response)),
        }
    }

    pub(crate) fn package_index(
        &mut self,
        package: &InstalledPackage,
    ) -> Result<crate::r_worker::protocol::WorkerPackageIndex> {
        let request_id = self.request_id();
        match self.exchange(&WorkerRequest::PackageIndex {
            request_id,
            package: package_spec(package),
        })? {
            WorkerResponse::PackageIndex {
                request_id: response_id,
                index,
            } if response_id == request_id => Ok(index),
            response => Err(worker_error("package index", response)),
        }
    }

    pub(crate) fn binding(
        &mut self,
        package: &InstalledPackage,
        name: &str,
    ) -> Result<crate::r_worker::protocol::WorkerBinding> {
        let request_id = self.request_id();
        match self.exchange(&WorkerRequest::Binding {
            request_id,
            package: package_spec(package),
            name: name.to_owned(),
        })? {
            WorkerResponse::Binding {
                request_id: response_id,
                binding,
            } if response_id == request_id && binding.binding.name == name => Ok(binding),
            response => Err(worker_error("binding", response)),
        }
    }

    pub(crate) fn validate_syntax(
        &mut self,
        source: &str,
    ) -> Result<crate::package::SyntaxValidation> {
        let request_id = self.request_id();
        match self.exchange(&WorkerRequest::ValidateSyntax {
            request_id,
            source: source.to_owned(),
        })? {
            WorkerResponse::SyntaxValidation {
                request_id: response_id,
                accepted,
                message: _,
            } if response_id == request_id && accepted => {
                Ok(crate::package::SyntaxValidation::Accepted)
            }
            WorkerResponse::SyntaxValidation {
                request_id: response_id,
                accepted: false,
                message,
            } if response_id == request_id => Ok(crate::package::SyntaxValidation::Rejected(
                message.unwrap_or_else(|| "target R rejected syntax".into()),
            )),
            response => Err(worker_error("syntax validation", response)),
        }
    }

    fn request_id(&mut self) -> u64 {
        let request = self.next_request;
        self.next_request = self.next_request.wrapping_add(1);
        request
    }

    fn exchange(&mut self, request: &WorkerRequest) -> Result<WorkerResponse> {
        serde_json::to_writer(&mut self.input, request).map_err(|error| {
            Error::Analysis(format!("failed to serialize Harp worker request: {error}"))
        })?;
        self.input.write_all(b"\n").map_err(|source| Error::Io {
            path: "<r-worker-stdin>".into(),
            source,
        })?;
        self.input.flush().map_err(|source| Error::Io {
            path: "<r-worker-stdin>".into(),
            source,
        })?;
        let mut line = String::new();
        loop {
            let bytes = self
                .output
                .read_line(&mut line)
                .map_err(|source| Error::Io {
                    path: self.protocol_path.clone(),
                    source,
                })?;
            if bytes != 0 && line.ends_with('\n') {
                break;
            }
            if let Some(status) = self.child.try_wait().ok().flatten() {
                return Err(Error::Analysis(format!(
                    "Harp worker terminated before responding; status {status}"
                )));
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        serde_json::from_str(&line).map_err(|error| {
            Error::Analysis(format!(
                "invalid Harp worker response: {error}; payload {line:?}"
            ))
        })
    }
}

fn worker_arch() -> &'static str {
    match std::env::consts::ARCH {
        "x86" => "i386",
        arch => arch,
    }
}

impl Drop for WorkerClient {
    fn drop(&mut self) {
        let _ = serde_json::to_writer(&mut self.input, &WorkerRequest::Shutdown);
        let _ = self.input.write_all(b"\n");
        let _ = self.input.flush();
        let _ = self.child.wait();
        let _ = std::fs::remove_file(&self.protocol_path);
    }
}

fn protocol_path() -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    std::env::temp_dir().join(format!(
        "slinker-r-worker-{}-{}.jsonl",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ))
}

fn package_spec(package: &InstalledPackage) -> PackageSpec {
    PackageSpec {
        name: package.id.name.clone(),
        version: package.id.version.to_string(),
        image_fingerprint: package.id.image_fingerprint.0.clone(),
        root: package.id.root.clone(),
    }
}

fn worker_error(operation: &str, response: WorkerResponse) -> Error {
    match response {
        WorkerResponse::Error { code, message, .. } => Error::Analysis(format!(
            "Harp worker {operation} failed ({code:?}): {message}"
        )),
        response => Error::Analysis(format!(
            "Harp worker returned unexpected {operation} response: {response:?}"
        )),
    }
}
