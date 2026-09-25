use crate::package::{
    BindingImage, ExportMap, ImportSpec, NativeComponent, PrivateEnvironmentImage, S3Registration,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

pub const PROTOCOL_VERSION: u32 = 1;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TargetSpec {
    pub r_home: PathBuf,
    pub arch: String,
    #[serde(default)]
    pub libraries: Vec<PathBuf>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PackageSpec {
    pub name: String,
    pub version: String,
    pub image_fingerprint: String,
    pub root: PathBuf,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorkerTarget {
    pub r_home: PathBuf,
    pub r_version: String,
    pub os: String,
    pub arch: String,
    pub libraries: Vec<PathBuf>,
    pub base_bindings: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorkerPackageIndex {
    pub name: String,
    pub version: String,
    pub image_fingerprint: String,
    pub exports: ExportMap,
    pub imports: Vec<ImportSpec>,
    pub s3: Vec<S3Registration>,
    pub dynlibs: Vec<NativeComponent>,
    pub on_load: bool,
    pub binding_names: Vec<String>,
    pub datasets: Vec<String>,
    pub has_sysdata: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorkerBinding {
    pub package_name: String,
    pub package_version: String,
    pub image_fingerprint: String,
    pub binding: BindingImage,
    pub private_environments: HashMap<String, PrivateEnvironmentImage>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WorkerRequest {
    Hello {
        protocol: u32,
        target: TargetSpec,
    },
    PackageIndex {
        request_id: u64,
        package: PackageSpec,
    },
    Binding {
        request_id: u64,
        package: PackageSpec,
        name: String,
    },
    SerializeBinding {
        request_id: u64,
        package: PackageSpec,
        name: String,
    },
    ValidateSyntax {
        request_id: u64,
        source: String,
    },
    NormalizeSyntax {
        request_id: u64,
        source: String,
    },
    Shutdown,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WorkerResponse {
    Hello {
        protocol: u32,
        harp_worker: bool,
        target: WorkerTarget,
    },
    PackageIndex {
        request_id: u64,
        index: WorkerPackageIndex,
    },
    Binding {
        request_id: u64,
        binding: WorkerBinding,
    },
    Payload {
        request_id: u64,
        bytes: Vec<u8>,
    },
    SyntaxValidation {
        request_id: u64,
        accepted: bool,
        message: Option<String>,
    },
    NormalizedSyntax {
        request_id: u64,
        source: String,
    },
    Error {
        error: WorkerFailure,
    },
    Shutdown,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorkerFailure {
    pub request_id: Option<u64>,
    pub package: Option<WorkerPackageIdentity>,
    pub binding: Option<String>,
    pub code: WorkerErrorCode,
    pub message: String,
    pub captured_output: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WorkerPackageIdentity {
    pub name: String,
    pub version: String,
    pub image_fingerprint: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkerErrorCode {
    Protocol,
    SharedLibraryLoad,
    RuntimeStartup,
    ArchitectureMismatch,
    TargetIdentityMismatch,
    PackageMetadata,
    LazyLoadDatabase,
    MissingBinding,
    BindingForce,
    WorkerCrash,
    TargetSyntaxRejection,
}
