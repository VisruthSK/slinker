use super::protocol;
use super::protocol::{
    PROTOCOL_VERSION, WorkerErrorCode, WorkerFailure, WorkerPackageIdentity, WorkerRequest,
    WorkerResponse,
};
use super::runtime::WorkerRuntime;
use slinker_core::{Error, Result};
use std::fs::OpenOptions;
use std::io::{self, BufRead, Write};

#[derive(Default)]
struct RequestContext {
    request_id: Option<u64>,
    package: Option<WorkerPackageIdentity>,
    binding: Option<String>,
}

impl RequestContext {
    fn of(request: &WorkerRequest) -> Self {
        let (request_id, package, binding) = match request {
            WorkerRequest::Hello { .. } | WorkerRequest::Shutdown => (None, None, None),
            WorkerRequest::PackageIndex {
                request_id,
                package,
            }
            | WorkerRequest::DataLibrary {
                request_id,
                package,
                ..
            } => (Some(*request_id), Some(package), None),
            WorkerRequest::Binding {
                request_id,
                package,
                name,
            } => (Some(*request_id), Some(package), Some(name.clone())),
            WorkerRequest::DispatchGenerics {
                request_id,
                package,
                name,
            } => (Some(*request_id), package.as_ref(), Some(name.clone())),
            WorkerRequest::SerializePayloads { request_id, .. }
            | WorkerRequest::ValidateSyntax { request_id, .. }
            | WorkerRequest::NormalizeSyntax { request_id, .. }
            | WorkerRequest::VerifyRelocation { request_id, .. } => (Some(*request_id), None, None),
        };
        Self {
            request_id,
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

fn start_runtime(
    runtime: &mut Option<WorkerRuntime>,
    protocol: u32,
    target: &protocol::TargetSpec,
) -> WorkerResponse {
    let context = RequestContext::default();
    if protocol != PROTOCOL_VERSION {
        return context.failure(
            WorkerErrorCode::Protocol,
            format!("unsupported worker protocol {protocol}; expected {PROTOCOL_VERSION}"),
        );
    }
    let started = WorkerRuntime::start(target).and_then(|started| {
        let target = started.target()?;
        Ok((started, target))
    });
    match started {
        Ok((started, target)) => {
            *runtime = Some(started);
            WorkerResponse::Hello {
                protocol: PROTOCOL_VERSION,
                harp_worker: true,
                target,
            }
        }
        Err(error) => context.failure(WorkerErrorCode::RuntimeStartup, error.to_string()),
    }
}

fn respond(runtime: &mut Option<WorkerRuntime>, request: WorkerRequest) -> WorkerResponse {
    let context = RequestContext::of(&request);
    if let WorkerRequest::Hello { protocol, target } = &request {
        return start_runtime(runtime, *protocol, target);
    }
    let Some(runtime) = runtime.as_mut() else {
        return context.failure(
            WorkerErrorCode::RuntimeStartup,
            "Harp worker must receive hello before semantic requests",
        );
    };
    runtime
        .answer(request)
        .unwrap_or_else(|failure| context.failure(failure.code, failure.error.to_string()))
}

pub fn run(protocol_path: &std::path::Path) -> Result<()> {
    let mut output = OpenOptions::new()
        .append(true)
        .open(protocol_path)
        .map_err(|source| Error::Io {
            path: protocol_path.to_path_buf(),
            source,
        })?;
    let mut runtime = None;

    for line in io::stdin().lock().lines() {
        let line = line.map_err(|source| Error::Io {
            path: "<r-worker-stdin>".into(),
            source,
        })?;
        if line.trim().is_empty() {
            continue;
        }
        let response = match serde_json::from_str::<WorkerRequest>(&line) {
            Ok(WorkerRequest::Shutdown) => {
                return write_response(&mut output, &WorkerResponse::Shutdown);
            }
            Ok(request) => respond(&mut runtime, request),
            Err(error) => RequestContext::default().failure(
                WorkerErrorCode::Protocol,
                format!("invalid worker request: {error}"),
            ),
        };
        write_response(&mut output, &response)?;
    }
    Ok(())
}

pub(super) fn write_response(writer: &mut impl Write, response: &WorkerResponse) -> Result<()> {
    serde_json::to_writer(&mut *writer, response).map_err(|error| {
        Error::Analysis(format!("failed to serialize R worker response: {error}"))
    })?;
    writer.write_all(b"\n").map_err(|source| Error::Io {
        path: "<r-worker-stdout>".into(),
        source,
    })?;
    writer.flush().map_err(|source| Error::Io {
        path: "<r-worker-stdout>".into(),
        source,
    })
}
