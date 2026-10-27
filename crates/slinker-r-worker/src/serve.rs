use super::protocol;
use super::protocol::{
    PROTOCOL_VERSION, RESPONSE_READY, WorkerErrorCode, WorkerFailure, WorkerPackageIdentity,
    WorkerRequest, WorkerResponse,
};
use super::runtime::WorkerRuntime;
use slinker_core::package::BindingName;
use slinker_core::{Error, Result};
use std::fs::OpenOptions;
use std::io::{self, BufRead, Write};

#[derive(Default)]
struct RequestContext {
    request_id: Option<u64>,
    package: Option<WorkerPackageIdentity>,
    binding: Option<BindingName>,
}

impl RequestContext {
    fn of(request: &WorkerRequest) -> Self {
        let (package, binding) = match request {
            WorkerRequest::PackageIndex { package, .. }
            | WorkerRequest::DataLibrary { package, .. }
            | WorkerRequest::BindingBatch { package, .. } => (Some(package), None),
            WorkerRequest::Binding { package, name, .. } => (Some(package), Some(name.clone())),
            WorkerRequest::DispatchGenerics { package, name, .. } => {
                (package.as_ref(), Some(name.clone()))
            }
            _ => (None, None),
        };
        Self {
            request_id: request.request_id(),
            package: package.map(|package| WorkerPackageIdentity {
                name: package.name.clone(),
                version: package.version.clone(),
                image_fingerprint: package.image_fingerprint.clone(),
            }),
            binding,
        }
    }

    fn failure(self, code: WorkerErrorCode, message: impl Into<String>) -> WorkerResponse {
        WorkerResponse::Error {
            error: WorkerFailure {
                request_id: self.request_id,
                package: self.package,
                binding: self.binding,
                code,
                message: message.into(),
                captured_output: Vec::new(),
            },
        }
    }
}

pub(super) struct Initialization(());

enum Server {
    AwaitingHello,
    Configuring(WorkerRuntime),
    Running(WorkerRuntime),
    Retired,
}

fn start_runtime(
    runtime: Server,
    protocol: u32,
    target: &protocol::TargetSpec,
) -> (Server, WorkerResponse) {
    let context = RequestContext::default();
    if !matches!(runtime, Server::AwaitingHello) {
        return (
            runtime,
            context.failure(
                WorkerErrorCode::Protocol,
                "worker startup is a one-time transition",
            ),
        );
    }
    if protocol != PROTOCOL_VERSION {
        return (
            runtime,
            context.failure(
                WorkerErrorCode::Protocol,
                format!("unsupported worker protocol {protocol}; expected {PROTOCOL_VERSION}"),
            ),
        );
    }
    let started = WorkerRuntime::start(target, Initialization(())).and_then(|started| {
        let target = started.target()?;
        Ok((started, target))
    });
    match started {
        Ok((started, target)) => (
            Server::Configuring(started),
            WorkerResponse::Hello {
                protocol: PROTOCOL_VERSION,
                harp_worker: true,
                target,
            },
        ),
        Err(error) => (
            Server::Retired,
            context.failure(WorkerErrorCode::RuntimeStartup, error.to_string()),
        ),
    }
}

fn respond(runtime: Server, request: WorkerRequest) -> (Server, WorkerResponse) {
    let context = RequestContext::of(&request);
    if let WorkerRequest::Hello { protocol, target } = &request {
        return start_runtime(runtime, *protocol, target);
    }
    match (runtime, request) {
        (
            Server::Configuring(mut worker),
            WorkerRequest::ConfigureLibraries {
                request_id,
                libraries,
            },
        ) => match worker.configure(&libraries) {
            Ok(target) => (
                Server::Running(worker),
                WorkerResponse::Configured { request_id, target },
            ),
            Err(error) => (
                Server::Retired,
                context.failure(WorkerErrorCode::PackageMetadata, error.to_string()),
            ),
        },
        (state, WorkerRequest::ConfigureLibraries { .. }) => (
            state,
            context.failure(
                WorkerErrorCode::Protocol,
                "library configuration is frozen after inspection starts",
            ),
        ),
        (Server::Configuring(mut worker) | Server::Running(mut worker), request) => {
            let response = worker
                .answer(request)
                .unwrap_or_else(|failure| context.failure(failure.code, failure.error.to_string()));
            (Server::Running(worker), response)
        }
        (state, _) => (
            state,
            context.failure(
                WorkerErrorCode::RuntimeStartup,
                "Harp worker must receive hello before semantic requests",
            ),
        ),
    }
}

pub fn run(protocol_path: &std::path::Path) -> Result<()> {
    let mut output = OpenOptions::new()
        .append(true)
        .open(protocol_path)
        .map_err(|source| Error::Io {
            path: protocol_path.to_path_buf(),
            source,
        })?;
    let mut runtime = Server::AwaitingHello;

    for line in io::stdin().lock().lines() {
        let line = line.map_err(|source| Error::Io {
            path: "<r-worker-stdin>".into(),
            source,
        })?;
        if line.trim().is_empty() {
            continue;
        }
        let (state, response) = match serde_json::from_str::<WorkerRequest>(&line) {
            Ok(WorkerRequest::Shutdown) => {
                write_response(&mut output, &WorkerResponse::Shutdown)?;
                return signal_response_ready();
            }
            Ok(request) => respond(runtime, request),
            Err(error) => (
                runtime,
                RequestContext::default().failure(
                    WorkerErrorCode::Protocol,
                    format!("invalid worker request: {error}"),
                ),
            ),
        };
        runtime = state;
        write_response(&mut output, &response)?;
        signal_response_ready()?;
        // Namespace resolution can interrupt R's lazy-load decoder after it has
        // cached an incomplete environment. No observation may reuse that epoch.
        if matches!(runtime, Server::Retired)
            || matches!(response, WorkerResponse::NamespaceRequired { .. })
        {
            return Ok(());
        }
    }
    Ok(())
}

fn signal_response_ready() -> Result<()> {
    let mut stdout = io::stdout().lock();
    stdout
        .write_all(&[RESPONSE_READY])
        .and_then(|()| stdout.flush())
        .map_err(|source| Error::Io {
            path: "<r-worker-stdout>".into(),
            source,
        })
}

pub(super) fn write_response(writer: &mut impl Write, response: &WorkerResponse) -> Result<()> {
    let mut encoded = serde_json::to_vec(response).map_err(|error| {
        Error::Analysis(format!("failed to serialize R worker response: {error}"))
    })?;
    encoded.push(b'\n');
    writer.write_all(&encoded).map_err(|source| Error::Io {
        path: "<r-worker-stdout>".into(),
        source,
    })?;
    writer.flush().map_err(|source| Error::Io {
        path: "<r-worker-stdout>".into(),
        source,
    })
}

#[cfg(test)]
#[path = "../tests/unit/inspection.rs"]
mod tests;
