use crate::package::InstalledPackage;
use crate::r_worker::protocol::{
    PROTOCOL_VERSION, PackageSpec, TargetSpec, WorkerRequest, WorkerResponse,
};
use crate::{Error, Result, Target, TargetEnvironment};
use std::collections::BTreeSet;
use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use tempfile::TempPath;

const RESPONSE_SPIN: std::time::Duration = std::time::Duration::from_millis(2);

pub(crate) struct WorkerClient {
    child: Child,
    input: BufWriter<ChildStdin>,
    output: BufReader<File>,
    protocol_path: TempPath,
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
        let (protocol_file, protocol_path) = protocol_file()?;
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
                worker: next_worker(),
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

    pub(crate) fn serialize_bundle(
        &mut self,
        package: PackageSpec,
        names: Vec<String>,
    ) -> Result<Vec<u8>> {
        let request_id = self.request_id();
        match self.exchange(&WorkerRequest::SerializeBundle {
            request_id,
            package,
            names,
        })? {
            WorkerResponse::Payload {
                request_id: response_id,
                bytes,
            } if response_id == request_id => Ok(bytes),
            response => Err(worker_error("payload serialization", response)),
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

    pub(crate) fn normalize_syntax(&mut self, source: &str) -> Result<String> {
        let request_id = self.request_id();
        match self.exchange(&WorkerRequest::NormalizeSyntax {
            request_id,
            source: source.to_owned(),
        })? {
            WorkerResponse::NormalizedSyntax {
                request_id: response_id,
                source,
            } if response_id == request_id => Ok(source),
            response => Err(worker_error("syntax normalization", response)),
        }
    }

    fn request_id(&mut self) -> u64 {
        let request = self.next_request;
        self.next_request = self.next_request.wrapping_add(1);
        request
    }

    fn exchange(&mut self, request: &WorkerRequest) -> Result<WorkerResponse> {
        let context = request_context(request);
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
        let waiting = std::time::Instant::now();
        loop {
            let bytes = self
                .output
                .read_line(&mut line)
                .map_err(|source| Error::Io {
                    path: self.protocol_path.to_path_buf(),
                    source,
                })?;
            if bytes != 0 && line.ends_with('\n') {
                break;
            }
            if let Some(status) = self.child.try_wait().ok().flatten() {
                return Err(Error::Analysis(format!(
                    "Harp worker terminated while processing {context}; status {status}"
                )));
            }
            if waiting.elapsed() < RESPONSE_SPIN {
                std::thread::yield_now();
            } else {
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        }
        serde_json::from_str(&line).map_err(|error| {
            Error::Analysis(format!(
                "invalid Harp worker response: {error}; payload {line:?}"
            ))
        })
    }
}

fn request_context(request: &WorkerRequest) -> String {
    match request {
        WorkerRequest::Hello { .. } => "target startup".into(),
        WorkerRequest::PackageIndex {
            request_id,
            package,
        } => format!(
            "request {request_id} package index {} {} {}",
            package.name, package.version, package.image_fingerprint
        ),
        WorkerRequest::Binding {
            request_id,
            package,
            name,
        } => format!(
            "request {request_id} binding {}::{name} {} {}",
            package.name, package.version, package.image_fingerprint
        ),
        WorkerRequest::SerializeBundle {
            request_id,
            package,
            names,
        } => format!(
            "request {request_id} payload bundle of {} {} bindings {} {}",
            package.name,
            names.len(),
            package.version,
            package.image_fingerprint
        ),
        WorkerRequest::ValidateSyntax { request_id, .. } => {
            format!("request {request_id} target syntax validation")
        }
        WorkerRequest::NormalizeSyntax { request_id, .. } => {
            format!("request {request_id} target syntax normalization")
        }
        WorkerRequest::Shutdown => "worker shutdown".into(),
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
    }
}

fn next_worker() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

fn protocol_file() -> Result<(File, TempPath)> {
    let file = tempfile::Builder::new()
        .prefix("slinker-r-worker-")
        .suffix(".jsonl")
        .tempfile()
        .map_err(|source| Error::Io {
            path: std::env::temp_dir(),
            source,
        })?;
    Ok(file.into_parts())
}

fn package_spec(package: &InstalledPackage) -> PackageSpec {
    PackageSpec {
        name: package.identity.name.clone(),
        version: package.identity.version.to_string(),
        image_fingerprint: package.identity.image_fingerprint.0.clone(),
        root: package.location.root.clone(),
    }
}

fn worker_error(operation: &str, response: WorkerResponse) -> Error {
    match response {
        WorkerResponse::Error { error } => Error::Analysis(format!(
            "Harp worker {operation} failed ({:?}) for {}{}: {}{}",
            error.code,
            error.package.as_ref().map_or_else(
                || "target".into(),
                |package| format!(
                    "{} {} {}",
                    package.name, package.version, package.image_fingerprint
                )
            ),
            error
                .binding
                .as_ref()
                .map_or_else(String::new, |binding| format!("::{binding}")),
            error.message,
            if error.captured_output.is_empty() {
                String::new()
            } else {
                format!("; R output: {}", error.captured_output.join(" | "))
            }
        )),
        response => Error::Analysis(format!(
            "Harp worker returned unexpected {operation} response: {response:?}"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stale_protocol_files_do_not_block_worker_startup() {
        let stale = (0..64)
            .map(|index| {
                std::env::temp_dir().join(format!(
                    "slinker-r-worker-{}-{index}.jsonl",
                    std::process::id()
                ))
            })
            .filter(|path| std::fs::File::create_new(path).is_ok())
            .collect::<Vec<_>>();
        let created = (0..3).map(|_| protocol_file()).collect::<Vec<_>>();
        for path in stale {
            let _ = std::fs::remove_file(path);
        }
        for result in created {
            result.expect("a fresh protocol file");
        }
    }

    #[test]
    fn worker_crash_context_identifies_exact_binding_request() {
        let request = WorkerRequest::Binding {
            request_id: 41,
            package: PackageSpec {
                name: "fixture".into(),
                version: "1.0.0".into(),
                image_fingerprint: "abc123".into(),
                root: "fixture".into(),
            },
            name: "bad".into(),
        };

        assert_eq!(
            request_context(&request),
            "request 41 binding fixture::bad 1.0.0 abc123"
        );
    }
}
