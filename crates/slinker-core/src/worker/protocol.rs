use crate::ir::ObjectStep;
use crate::package::{
    BindingImage, BindingName, DataSetId, DataStorage, DatasetName, Digest, EnvironmentLabel,
    ExportMap, ImportSpec, NativeComponent, PackageName, PrivateEnvironmentImage, S3Registration,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

pub const RESPONSE_READY: u8 = 0;

pub const PROTOCOL_VERSION: u32 = 7;

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
    pub name: PackageName,
    pub version: String,
    pub image_fingerprint: Digest,
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
    pub names: Vec<BindingName>,
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
    pub binding: BindingName,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ClosurePatchSpec {
    pub root: Option<BindingName>,
    pub steps: Vec<ObjectStep>,
    pub binding: BindingName,
    pub expected_shape: Digest,
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
pub struct WorkerTarget {
    pub r_home: PathBuf,
    pub r_version: String,
    pub os: String,
    pub arch: String,
    pub libraries: Vec<PathBuf>,
    pub base_bindings: Vec<BindingName>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorkerPackageIndex {
    pub name: PackageName,
    pub version: String,
    pub image_fingerprint: Digest,
    pub exports: ExportMap,
    pub imports: Vec<ImportSpec>,
    pub s3: Vec<S3Registration>,
    pub dynlibs: Vec<NativeComponent>,
    pub on_load: bool,
    pub binding_names: Vec<BindingName>,
    pub data_sets: BTreeMap<DataSetId, Vec<DatasetName>>,
    pub data_storage: DataStorage,
    pub has_sysdata: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorkerBinding {
    pub package_name: PackageName,
    pub package_version: String,
    pub image_fingerprint: Digest,
    pub binding: BindingImage,
    pub private_environments: HashMap<EnvironmentLabel, PrivateEnvironmentImage>,
    #[serde(default)]
    pub normalizations: Vec<WorkerNormalization>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorkerNormalization {
    pub original: String,
    pub canonical: NormalizedSource,
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
        name: BindingName,
    },
    BindingBatch {
        request_id: u64,
        package: PackageSpec,
        names: Vec<BindingName>,
    },
    DispatchGenerics {
        request_id: u64,
        package: Option<PackageSpec>,
        name: BindingName,
    },
    DataLibrary {
        request_id: u64,
        package: PackageSpec,
        objects: Vec<DatasetName>,
        sets: BTreeMap<DataSetId, Vec<DatasetName>>,
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
        sources: Vec<String>,
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
    Bindings {
        request_id: u64,
        bindings: Vec<WorkerBinding>,
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
        results: Vec<NormalizeOutcome>,
    },
    Error {
        error: WorkerFailure,
    },
    Shutdown,
}

impl WorkerResponse {
    pub fn request_id(&self) -> Option<u64> {
        match self {
            Self::PackageIndex { request_id, .. }
            | Self::Binding { request_id, .. }
            | Self::Bindings { request_id, .. }
            | Self::DispatchGenerics { request_id, .. }
            | Self::DataLibrary { request_id, .. }
            | Self::Payloads { request_id, .. }
            | Self::SyntaxValidation { request_id, .. }
            | Self::NormalizedSyntax { request_id, .. } => Some(*request_id),
            Self::Hello { .. } | Self::Error { .. } | Self::Shutdown => None,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NormalizeOutcome {
    Normalized(NormalizedSource),
    Rejected(String),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NormalizedSource {
    pub source: String,
    pub stable: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorkerFailure {
    pub request_id: Option<u64>,
    pub package: Option<WorkerPackageIdentity>,
    pub binding: Option<BindingName>,
    pub code: WorkerErrorCode,
    pub message: String,
    pub captured_output: Vec<String>,
}

impl std::fmt::Display for WorkerFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "({:?}) for ", self.code)?;
        match &self.package {
            Some(package) => write!(
                f,
                "{} {} {}",
                package.name, package.version, package.image_fingerprint
            )?,
            None => f.write_str("target")?,
        }
        if let Some(binding) = &self.binding {
            write!(f, "::{binding}")?;
        }
        write!(f, ": {}", self.message)?;
        if !self.captured_output.is_empty() {
            write!(f, "; R output: {}", self.captured_output.join(" | "))?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WorkerPackageIdentity {
    pub name: PackageName,
    pub version: String,
    pub image_fingerprint: Digest,
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
