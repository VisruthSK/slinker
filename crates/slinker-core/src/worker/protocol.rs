use crate::package::{
    BindingImage, ExportMap, ImportSpec, NativeComponent, PrivateEnvironmentImage, S3Registration,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

pub const PROTOCOL_VERSION: u32 = 5;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TargetSpec {
    pub r_home: PathBuf,
    pub arch: String,
    pub worker: u64,
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
pub struct NamespaceImageSpec {
    pub package: PackageSpec,
    pub registered_name: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PayloadSpec {
    pub package: PackageSpec,
    pub names: Vec<String>,
    pub patches: Vec<ClosurePatchSpec>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PayloadSerialization {
    Serialized {
        bundles: Vec<SerializedPayload>,
    },
    SharedIdentity {
        first: PayloadSite,
        second: PayloadSite,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SerializedPayload {
    pub bytes: Vec<u8>,
    pub namespaces: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PayloadSite {
    pub payload: usize,
    pub binding: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ClosurePatchSpec {
    pub root: Option<String>,
    pub steps: Vec<ObjectStepSpec>,
    pub binding: String,
    pub expected_shape: String,
    pub source: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RelocationSiteSpec {
    pub start: usize,
    pub end: usize,
    pub replacement: String,
    pub appended_argument: Option<AppendedArgumentSpec>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AppendedArgumentSpec {
    pub name: String,
    pub value: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", content = "name", rename_all = "snake_case")]
pub enum ObjectStepSpec {
    Environment,
    Parent,
    Binding(String),
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
    pub data_sets: BTreeMap<String, Vec<String>>,
    pub data_files: bool,
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
pub struct DataLibraryFiles {
    pub rdb: Vec<u8>,
    pub rdx: Vec<u8>,
    pub rds: Vec<u8>,
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
    DispatchGenerics {
        request_id: u64,
        package: Option<PackageSpec>,
        name: String,
    },
    DataLibrary {
        request_id: u64,
        package: PackageSpec,
        objects: Vec<String>,
        sets: BTreeMap<String, Vec<String>>,
    },
    SerializePayloads {
        request_id: u64,
        namespaces: Vec<NamespaceImageSpec>,
        payloads: Vec<PayloadSpec>,
    },
    ValidateSyntax {
        request_id: u64,
        source: String,
    },
    NormalizeSyntax {
        request_id: u64,
        source: String,
    },
    VerifyRelocation {
        request_id: u64,
        original: String,
        rewritten: String,
        sites: Vec<RelocationSiteSpec>,
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
    DispatchGenerics {
        request_id: u64,
        generics: Vec<String>,
    },
    DataLibrary {
        request_id: u64,
        library: DataLibraryFiles,
    },
    Payloads {
        request_id: u64,
        serialization: PayloadSerialization,
    },
    SyntaxValidation {
        request_id: u64,
        accepted: bool,
        message: Option<String>,
    },
    NormalizedSyntax {
        request_id: u64,
        source: String,
        stable: bool,
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

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkerErrorCode {
    Protocol,
    RuntimeStartup,
    PackageMetadata,
    MissingBinding,
    BindingForce,
    TargetSyntaxRejection,
}

impl WorkerRequest {
    pub fn opcode(&self) -> &'static str {
        match self {
            Self::Hello { .. } => "hello",
            Self::PackageIndex { .. } => "package_index",
            Self::Binding { .. } => "binding",
            Self::DispatchGenerics { .. } => "dispatch_generics",
            Self::DataLibrary { .. } => "data_library",
            Self::SerializePayloads { .. } => "serialize_payloads",
            Self::ValidateSyntax { .. } => "validate_syntax",
            Self::NormalizeSyntax { .. } => "normalize_syntax",
            Self::VerifyRelocation { .. } => "verify_relocation",
            Self::Shutdown => "shutdown",
        }
    }
}
