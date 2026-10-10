use crate::package::SyntaxValidation;
use crate::package::{
    CanonicalSyntax, DataSetId, DatasetName, FrozenPackages, InstalledPackage, Normalization,
};
use crate::profile::{self, Counter, Probe};
use crate::worker::protocol::WorkerPackageIndex;
use crate::worker::protocol::{
    DataLibraryFiles, NamespaceImageSpec, PROTOCOL_VERSION, PackageSpec, PayloadSerialization,
    PayloadSite, PayloadSpec, RelocationSiteSpec, TargetSpec, WorkerRequest, WorkerResponse,
};
use crate::worker::protocol::{RESPONSE_READY, WorkerBinding};
use crate::{Error, Result, Target, TargetEnvironment, WorkerExecutable};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::Arc;
use tempfile::TempPath;

#[derive(Debug)]
pub(crate) struct WorkerClient {
    connection: WorkerConnection,
    packages: Arc<FrozenPackages>,
    images: BTreeMap<crate::package::PackageName, PackageSpec>,
    executable: WorkerExecutable,
    lane: u64,
}

#[derive(Debug)]
struct WorkerConnection {
    child: Child,
    input: BufWriter<ChildStdin>,
    output: BufReader<File>,
    ready: BufReader<ChildStdout>,
    protocol_path: TempPath,
    next_request: u64,
}

impl WorkerClient {
    pub(crate) fn spawn(
        packages: Arc<FrozenPackages>,
        lane: u64,
        executable: &WorkerExecutable,
    ) -> Result<Self> {
        let target = packages.target();
        let (mut client, actual) = Self::connect(
            target.r_home.clone(),
            target.libraries.clone(),
            lane,
            executable,
        )?;
        if actual != *target {
            return Err(Error::Analysis(
                "Harp worker target changed between discovery and analysis".into(),
            ));
        }
        client.packages = packages;
        Ok(client)
    }

    pub(crate) fn capture_target(
        r_home: std::path::PathBuf,
        libraries: Vec<std::path::PathBuf>,
        executable: &WorkerExecutable,
    ) -> Result<(TargetEnvironment, Self)> {
        let (client, target) = Self::connect(r_home, libraries, 0, executable)?;
        Ok((target, client))
    }

    fn connect(
        r_home: std::path::PathBuf,
        libraries: Vec<std::path::PathBuf>,
        lane: u64,
        executable: &WorkerExecutable,
    ) -> Result<(Self, TargetEnvironment)> {
        let executable_path = executable.path()?;
        let (protocol_file, protocol_path) = protocol_file()?;
        let mut command = Command::new(&executable_path);
        command
            .arg("__r-worker")
            .arg(&protocol_path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
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
        command.env("R_DEFAULT_PACKAGES", "methods");
        #[cfg(all(unix, not(target_os = "macos")))]
        command.env("LD_LIBRARY_PATH", target_library_path(&r_home)?);
        #[cfg(unix)]
        command.envs(target_resource_directories(&r_home)?);
        profile::count(Counter::RWorkerStartups);
        let mut child = command.spawn().map_err(|source| Error::Io {
            path: executable_path.clone(),
            source,
        })?;
        let input = child
            .stdin
            .take()
            .ok_or_else(|| Error::Analysis("failed to open Harp worker request stream".into()))?;
        let ready = child
            .stdout
            .take()
            .ok_or_else(|| Error::Analysis("failed to open Harp worker readiness stream".into()))?;
        let mut client = WorkerConnection {
            child,
            input: BufWriter::new(input),
            ready: BufReader::new(ready),
            output: BufReader::new(protocol_file),
            protocol_path,
            next_request: 1,
        };
        let response = client.exchange(&WorkerRequest::Hello {
            protocol: PROTOCOL_VERSION,
            target: TargetSpec {
                r_home,
                arch: worker_arch().into(),
                worker: lane,
                libraries,
            },
        })?;
        match response {
            WorkerResponse::Hello {
                protocol: PROTOCOL_VERSION,
                harp_worker: true,
                target,
            } => {
                let target = TargetEnvironment {
                    r_home: target.r_home,
                    target: Target {
                        r_version: target.r_version,
                        os: target.os,
                        arch: target.arch,
                    },
                    libraries: target.libraries,
                    base_bindings: target.base_bindings.into_iter().collect::<BTreeSet<_>>(),
                };
                Ok((
                    Self {
                        connection: client,
                        packages: Arc::new(FrozenPackages::new(target.clone())),
                        images: BTreeMap::new(),
                        executable: WorkerExecutable::Standalone(executable_path),
                        lane,
                    },
                    target,
                ))
            }
            response => Err(worker_error("worker startup", response)),
        }
    }

    fn call<T>(
        &mut self,
        operation: &str,
        request: impl FnOnce(u64) -> WorkerRequest,
        pick: impl FnOnce(WorkerResponse) -> Option<T>,
    ) -> Result<T> {
        let request_id = self.next_request();
        let original = request(request_id);
        for image in original.images() {
            if let Some(previous) = self.images.get(&image.name)
                && previous != image
            {
                return Err(Error::Analysis(format!(
                    "inspection image changed within an epoch: {}",
                    image.name
                )));
            }
            self.images.insert(image.name.clone(), image.clone());
        }
        let mut current = original.clone();
        let mut requested = HashSet::new();
        loop {
            let response = self.connection.exchange(&current)?;
            if matches!(response, WorkerResponse::Error { .. }) {
                return Err(worker_error(operation, response));
            }
            if response.request_id() != current.request_id() {
                return Err(Error::Analysis(format!(
                    "Harp worker returned an unexpected {operation} request id"
                )));
            }
            match response {
                WorkerResponse::NamespaceRequired { package, .. } => {
                    if !requested.insert(package.clone()) {
                        return Err(Error::Analysis(format!(
                            "inspection repeatedly requested namespace `{package}` after image registration"
                        )));
                    }
                    let installed = self.packages.locate(&package)?.ok_or_else(|| {
                        Error::Analysis(format!(
                            "inspection requires missing namespace `{package}`"
                        ))
                    })?;
                    self.images.insert(package, package_spec(&installed));
                    let target = self.packages.target();
                    let (replacement, actual) = Self::connect(
                        target.r_home.clone(),
                        target.libraries.clone(),
                        self.lane,
                        &self.executable,
                    )?;
                    if actual != *target {
                        return Err(Error::Analysis(
                            "target changed while replacing a failed inspection epoch".into(),
                        ));
                    }
                    self.connection = replacement.connection;
                    current = WorkerRequest::RegisterImages {
                        request_id: self.next_request(),
                        packages: self.images.values().cloned().collect(),
                    };
                }
                WorkerResponse::ImagesRegistered { .. }
                    if matches!(current, WorkerRequest::RegisterImages { .. }) =>
                {
                    current = original.clone();
                }
                response => {
                    return pick(response).ok_or_else(|| {
                        Error::Analysis(format!(
                            "Harp worker returned an unexpected {operation} response"
                        ))
                    });
                }
            }
        }
    }

    fn next_request(&mut self) -> u64 {
        let request = self.connection.next_request;
        self.connection.next_request = request.wrapping_add(1);
        request
    }

    pub(crate) fn packages(&self) -> Arc<FrozenPackages> {
        Arc::clone(&self.packages)
    }

    pub(crate) fn target(&self) -> &TargetEnvironment {
        self.packages.target()
    }

    pub(crate) fn executable(&self) -> WorkerExecutable {
        self.executable.clone()
    }

    pub(crate) fn package_index(
        &mut self,
        package: &InstalledPackage,
    ) -> Result<WorkerPackageIndex> {
        self.call(
            "package index",
            |request_id| WorkerRequest::PackageIndex {
                request_id,
                package: package_spec(package),
            },
            |response| match response {
                WorkerResponse::PackageIndex { index, .. } => Some(index),
                _ => None,
            },
        )
    }

    pub(crate) fn bindings(
        &mut self,
        package: &InstalledPackage,
        names: &[&str],
    ) -> Result<Vec<WorkerBinding>> {
        profile::add(Counter::RBatchItems, names.len() as u64);
        self.call(
            "binding batch",
            |request_id| WorkerRequest::BindingBatch {
                request_id,
                package: package_spec(package),
                names: names.iter().map(|name| (*name).into()).collect(),
            },
            |response| match response {
                WorkerResponse::Bindings { bindings, .. }
                    if bindings.len() == names.len()
                        && bindings
                            .iter()
                            .zip(names)
                            .all(|(binding, name)| binding.binding.name == *name) =>
                {
                    Some(bindings)
                }
                _ => None,
            },
        )
    }

    pub(crate) fn dispatch_generics(
        &mut self,
        package: Option<&InstalledPackage>,
        name: &str,
    ) -> Result<Vec<String>> {
        self.call(
            "dispatch generics",
            |request_id| WorkerRequest::DispatchGenerics {
                request_id,
                package: package.map(package_spec),
                name: name.into(),
            },
            |response| match response {
                WorkerResponse::DispatchGenerics { generics, .. } => Some(generics),
                _ => None,
            },
        )
    }

    pub(crate) fn configure_libraries(
        &mut self,
        libraries: Vec<std::path::PathBuf>,
    ) -> Result<TargetEnvironment> {
        let target = self.call(
            "library configuration",
            |request_id| WorkerRequest::ConfigureLibraries {
                request_id,
                libraries,
            },
            |response| match response {
                WorkerResponse::Configured { target, .. } => Some(TargetEnvironment {
                    r_home: target.r_home,
                    target: Target {
                        r_version: target.r_version,
                        os: target.os,
                        arch: target.arch,
                    },
                    libraries: target.libraries,
                    base_bindings: target.base_bindings.into_iter().collect(),
                }),
                _ => None,
            },
        )?;
        self.packages = Arc::new(FrozenPackages::new(target.clone()));
        Ok(target)
    }

    pub(crate) fn data_library(
        &mut self,
        package: PackageSpec,
        objects: Vec<DatasetName>,
        sets: BTreeMap<DataSetId, Vec<DatasetName>>,
    ) -> Result<DataLibraryFiles> {
        self.call(
            "data library",
            |request_id| WorkerRequest::DataLibrary {
                request_id,
                package,
                objects,
                sets,
            },
            |response| match response {
                WorkerResponse::DataLibrary { library, .. } => Some(library),
                _ => None,
            },
        )
    }

    pub(crate) fn serialize_payloads(
        &mut self,
        namespaces: Vec<NamespaceImageSpec>,
        payloads: Vec<PayloadSpec>,
    ) -> Result<PayloadSerialization> {
        let names = payloads
            .iter()
            .map(|payload| payload.names.clone())
            .collect::<Vec<_>>();
        let reaches = |site: &PayloadSite| {
            names
                .get(site.payload)
                .is_some_and(|names| names.contains(&site.binding))
        };
        self.call(
            "payload serialization",
            |request_id| WorkerRequest::SerializePayloads {
                request_id,
                namespaces,
                payloads,
            },
            |response| match response {
                WorkerResponse::Payloads { serialization, .. }
                    if match &serialization {
                        PayloadSerialization::Serialized { bundles } => {
                            bundles.len() == names.len()
                        }
                        PayloadSerialization::SharedIdentity { first, second } => {
                            first.payload != second.payload && reaches(first) && reaches(second)
                        }
                    } =>
                {
                    Some(serialization)
                }
                _ => None,
            },
        )
    }

    fn syntax_verdict(
        &mut self,
        operation: &str,
        request: impl FnOnce(u64) -> WorkerRequest,
    ) -> Result<SyntaxValidation> {
        self.call(operation, request, |response| match response {
            WorkerResponse::SyntaxValidation { accepted: true, .. } => {
                Some(SyntaxValidation::Accepted)
            }
            WorkerResponse::SyntaxValidation {
                accepted: false,
                message,
                ..
            } => Some(SyntaxValidation::Rejected(
                message.unwrap_or_else(|| format!("target R rejected {operation}")),
            )),
            _ => None,
        })
    }

    pub(crate) fn validate_syntax(&mut self, source: &str) -> Result<SyntaxValidation> {
        self.syntax_verdict("syntax validation", |request_id| {
            WorkerRequest::ValidateSyntax {
                request_id,
                source: source.to_owned(),
            }
        })
    }

    pub(crate) fn verify_relocation(
        &mut self,
        original: &str,
        rewritten: &str,
        sites: Vec<RelocationSiteSpec>,
    ) -> Result<SyntaxValidation> {
        self.syntax_verdict("relocation verification", |request_id| {
            WorkerRequest::VerifyRelocation {
                request_id,
                original: original.to_owned(),
                rewritten: rewritten.to_owned(),
                sites,
            }
        })
    }

    pub(crate) fn canonical_syntax(&mut self, source: &str) -> Result<CanonicalSyntax> {
        self.canonical_syntax_batch(&[source])?
            .pop()
            .ok_or_else(|| Error::Analysis("Harp worker returned no syntax normalization".into()))?
            .map_err(|rejection| {
                Error::Analysis(format!(
                    "Harp worker syntax normalization failed (TargetSyntaxRejection) for target: {rejection}"
                ))
            })
    }

    pub(crate) fn canonical_syntax_batch(
        &mut self,
        sources: &[&str],
    ) -> Result<Vec<Normalization>> {
        profile::add(Counter::RBatchItems, sources.len() as u64);
        let expected = sources.len();
        self.call(
            "syntax normalization",
            |request_id| WorkerRequest::NormalizeSyntax {
                request_id,
                sources: sources.iter().map(|source| (*source).to_owned()).collect(),
            },
            |response| match response {
                WorkerResponse::NormalizedSyntax { results, .. } if results.len() == expected => {
                    Some(results.into_iter().map(Normalization::from).collect())
                }
                _ => None,
            },
        )
    }
}

impl WorkerConnection {
    fn await_response_ready(&mut self, context: &str) -> Result<()> {
        let mut console_noise = Vec::new();
        let bytes = self
            .ready
            .read_until(RESPONSE_READY, &mut console_noise)
            .map_err(|source| Error::Io {
                path: "<r-worker-stdout>".into(),
                source,
            })?;
        if bytes == 0 || console_noise.last() != Some(&RESPONSE_READY) {
            let status = self
                .child
                .wait()
                .map_or_else(|error| error.to_string(), |status| status.to_string());
            return Err(Error::Analysis(format!(
                "Harp worker terminated while processing {context}; status {status}"
            )));
        }
        Ok(())
    }

    fn exchange(&mut self, request: &WorkerRequest) -> Result<WorkerResponse> {
        let _span = profile::span(Probe::WorkerRequest);
        let started = std::time::Instant::now();
        let context = request_context(request);
        let payload = serde_json::to_vec(request).map_err(|error| {
            Error::Analysis(format!("failed to serialize Harp worker request: {error}"))
        })?;
        self.input.write_all(&payload).map_err(|source| Error::Io {
            path: "<r-worker-stdin>".into(),
            source,
        })?;
        self.input.write_all(b"\n").map_err(|source| Error::Io {
            path: "<r-worker-stdin>".into(),
            source,
        })?;
        self.input.flush().map_err(|source| Error::Io {
            path: "<r-worker-stdin>".into(),
            source,
        })?;
        self.await_response_ready(&context)?;
        let mut line = Vec::new();
        self.output
            .read_until(b'\n', &mut line)
            .map_err(|source| Error::Io {
                path: self.protocol_path.to_path_buf(),
                source,
            })?;
        if !line.ends_with(b"\n") {
            return Err(Error::Analysis(format!(
                "Harp worker response to {context} was incomplete"
            )));
        }
        if profile::enabled() {
            profile::r_request(
                request_opcode(&payload),
                payload.len(),
                line.len(),
                u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX),
            );
        }
        serde_json::from_slice(&line).map_err(|error| {
            Error::Analysis(format!(
                "invalid Harp worker response: {error}; payload {:?}",
                String::from_utf8_lossy(&line)
            ))
        })
    }
}

fn request_opcode(payload: &[u8]) -> &str {
    const PREFIX: &[u8] = br#"{"kind":""#;
    payload
        .strip_prefix(PREFIX)
        .and_then(|rest| {
            rest.iter()
                .position(|byte| *byte == b'"')
                .map(|end| &rest[..end])
        })
        .and_then(|name| std::str::from_utf8(name).ok())
        .unwrap_or("unknown")
}

fn request_context(request: &WorkerRequest) -> String {
    match request {
        WorkerRequest::Hello { .. } => "target startup".into(),
        WorkerRequest::ConfigureLibraries { request_id, .. } => {
            format!("request {request_id} library configuration")
        }
        WorkerRequest::RegisterImages { request_id, .. } => {
            format!("request {request_id} inspection image registration")
        }
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
        WorkerRequest::BindingBatch {
            request_id,
            package,
            names,
        } => format!(
            "request {request_id} {} bindings of {} {} {}",
            names.len(),
            package.name,
            package.version,
            package.image_fingerprint
        ),
        WorkerRequest::DispatchGenerics {
            request_id,
            package,
            name,
        } => format!(
            "request {request_id} dispatch generics of {}::{name}",
            package
                .as_ref()
                .map_or("base", |package| package.name.as_str())
        ),
        WorkerRequest::DataLibrary {
            request_id,
            package,
            objects,
            ..
        } => format!(
            "request {request_id} data library of {} ({} objects)",
            package.name,
            objects.len()
        ),
        WorkerRequest::SerializePayloads {
            request_id,
            payloads,
            ..
        } => format!(
            "request {request_id} payload bundles of {}",
            payloads
                .iter()
                .map(|payload| format!(
                    "{} {} ({} bindings)",
                    payload.package.name,
                    payload.package.version,
                    payload.names.len()
                ))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        WorkerRequest::ValidateSyntax { request_id, .. } => {
            format!("request {request_id} target syntax validation")
        }
        WorkerRequest::NormalizeSyntax { request_id, .. } => {
            format!("request {request_id} target syntax normalization")
        }
        WorkerRequest::VerifyRelocation {
            request_id, sites, ..
        } => format!(
            "request {request_id} relocation verification of {} sites",
            sites.len()
        ),
        WorkerRequest::Shutdown => "worker shutdown".into(),
    }
}

fn worker_arch() -> &'static str {
    match std::env::consts::ARCH {
        "x86" => "i386",
        arch => arch,
    }
}

impl Drop for WorkerConnection {
    fn drop(&mut self) {
        let _ = serde_json::to_writer(&mut self.input, &WorkerRequest::Shutdown);
        let _ = self.input.write_all(b"\n");
        let _ = self.input.flush();
        let _ = self.child.wait();
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
pub fn target_library_path(r_home: &std::path::Path) -> Result<std::ffi::OsString> {
    use std::os::unix::ffi::OsStringExt;
    let output = Command::new("sh")
        .arg("-c")
        .arg(". \"${R_HOME}/etc${R_ARCH}/ldpaths\" && printf '%s' \"${LD_LIBRARY_PATH}\"")
        .env("R_HOME", r_home)
        .stdin(Stdio::null())
        .output()
        .map_err(|source| Error::Io {
            path: r_home.join("etc").join("ldpaths"),
            source,
        })?;
    if !output.status.success() {
        return Err(Error::Analysis(format!(
            "target R {} library path configuration failed: {}",
            r_home.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(std::ffi::OsString::from_vec(output.stdout))
}

#[cfg(unix)]
fn target_resource_directories(
    r_home: &std::path::Path,
) -> Result<Vec<(&'static str, std::ffi::OsString)>> {
    use std::os::unix::ffi::OsStringExt;
    const NAMES: [&str; 3] = ["R_SHARE_DIR", "R_INCLUDE_DIR", "R_DOC_DIR"];
    let executable = crate::r_executable(r_home).ok_or_else(|| Error::Io {
        path: r_home.join("bin").join("R"),
        source: std::io::Error::from(std::io::ErrorKind::NotFound),
    })?;
    let expression = format!(
        "cat(Sys.getenv(c({})), sep = '\\n')",
        NAMES.map(|name| format!("'{name}'")).join(", ")
    );
    let output = Command::new(&executable)
        .args(["--slave", "--no-save", "--no-restore", "-e", &expression])
        .env_remove("R_ENVIRON_USER")
        .env_remove("R_PROFILE_USER")
        .env_remove("R_LIBS")
        .env_remove("R_LIBS_USER")
        .env_remove("R_LIBS_SITE")
        .stdin(Stdio::null())
        .output()
        .map_err(|source| Error::Io {
            path: executable.clone(),
            source,
        })?;
    let lines = output
        .stdout
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>();
    if !output.status.success() || lines.len() != NAMES.len() {
        return Err(Error::Analysis(format!(
            "target R {} did not report its resource directories: {}",
            r_home.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(NAMES
        .into_iter()
        .zip(lines)
        .map(|(name, line)| (name, std::ffi::OsString::from_vec(line.to_vec())))
        .collect())
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
        image_fingerprint: package.identity.image_fingerprint.clone(),
        root: package.location.root.clone(),
    }
}

fn worker_error(operation: &str, response: WorkerResponse) -> Error {
    match response {
        WorkerResponse::Error { error } => {
            Error::Analysis(format!("Harp worker {operation} failed {error}"))
        }
        response => Error::Analysis(format!(
            "Harp worker returned unexpected {operation} response: {response:?}"
        )),
    }
}

#[cfg(test)]
#[path = "../../tests/unit/worker/client.rs"]
mod tests;
