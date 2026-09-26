//! Hidden target-R worker boundary.
//!
//! The process protocol contains only owned Rust/serde data; Harp objects and
//! raw SEXPs never cross the process boundary.

pub(crate) mod client;
pub mod protocol;

use crate::package::{
    BindingImage, BindingName, BindingOrigin, BindingRepresentation, ClassName, ClosureSource,
    EmbeddedClosureSource, EmbeddedEnvironmentRef, ExportMap, ImportBinding, ImportSpec,
    NativeComponent, NativeRegistration, NativeSafety, NativeSymbolBinding, ObjectIssue,
    ObjectKind, PrivateBindingImage, PrivateEnvironmentImage, S3Registration,
};
use crate::{Error, Result};
use harp::{RFunctionExt, RObjectExt};
use protocol::{
    PROTOCOL_VERSION, WorkerErrorCode, WorkerFailure, WorkerPackageIdentity, WorkerPackageIndex,
    WorkerRequest, WorkerResponse,
};
use std::collections::{HashMap, HashSet};
use std::ffi::CString;
use std::fs::OpenOptions;
use std::io::{self, BufRead, Write};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct InspectionEpoch {
    worker: u64,
    context: usize,
}

impl std::fmt::Display for InspectionEpoch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}", self.worker, self.context)
    }
}

struct PackageImageContext {
    image: harp::object::RObject,
    epoch: InspectionEpoch,
    private_ids: HashMap<libr::SEXP, String>,
}

struct WorkerRuntime {
    worker: u64,
    _arguments: Vec<CString>,
    _contexts: HashMap<String, PackageImageContext>,
}

impl WorkerRuntime {
    fn start(target: &protocol::TargetSpec) -> std::result::Result<Self, InspectionError> {
        if !target.r_home.is_dir() {
            return Err(format!(
                "selected R home does not exist: {}",
                target.r_home.display()
            )
            .into());
        }
        sanitize_environment(&target.r_home);
        let libraries = harp::library::RLibraries::from_r_home_path(&target.r_home);
        libraries.initialize_pre_setup_r();

        let arguments = [
            "R",
            "--slave",
            "--no-save",
            "--no-restore",
            "--no-site-file",
            "--no-init-file",
        ]
        .into_iter()
        .map(|argument| CString::new(argument).expect("static R argument has no NUL"))
        .collect::<Vec<_>>();
        let mut pointers = arguments
            .iter()
            .map(|argument| argument.as_ptr().cast_mut())
            .collect::<Vec<_>>();
        let argument_count = i32::try_from(pointers.len())
            .map_err(|_| "R startup arguments exceed the C argument count".to_owned())?;
        // Safety: the worker owns the only R runtime in this process, performs
        // initialization on its protocol thread, and keeps argv alive for the
        // process lifetime. Harp initializes all dynamic libr bindings before
        // any R API call and constant globals only after main-loop setup.
        unsafe {
            harp::CONSOLE_THREAD_ID = Some(std::thread::current().id());
            libr::set(libr::R_SignalHandlers, 0);
            libr::Rf_initialize_R(argument_count, pointers.as_mut_ptr());
            libr::set(libr::R_CStackLimit, usize::MAX);
            libr::setup_Rmainloop();
        }
        libraries.initialize_post_setup_r();
        harp::routines::r_register_routines();
        harp::initialize();

        configure_libraries(&target.libraries)?;
        install_helpers()?;
        let actual_arch = harp::parse_eval_base("R.version$arch")
            .and_then(String::try_from)
            .map_err(|error| format!("failed to query initialized R architecture: {error}"))?;
        if actual_arch != target.arch {
            return Err(format!(
                "R architecture mismatch: worker expected {}, initialized {actual_arch}",
                target.arch
            )
            .into());
        }
        Ok(Self {
            worker: target.worker,
            _arguments: arguments,
            _contexts: HashMap::new(),
        })
    }

    fn validate_syntax(&self, source: &str) -> std::result::Result<(), InspectionError> {
        harp::parse_exprs(source)
            .map(|_| ())
            .map_err(InspectionError::from)
    }

    fn normalize_syntax(&self, source: &str) -> std::result::Result<String, InspectionError> {
        harp::RFunction::new("", ".slinker_normalize_source")
            .add(source)
            .call()
            .and_then(String::try_from)
            .map_err(InspectionError::from)
    }

    fn target(&self) -> std::result::Result<protocol::WorkerTarget, InspectionError> {
        let string = |code| {
            harp::parse_eval_base(code)
                .and_then(String::try_from)
                .map_err(|error| format!("failed to query target R: {error}"))
        };
        let libraries = harp::RFunction::new("base", ".libPaths")
            .call()
            .and_then(Vec::<String>::try_from)
            .map_err(|error| format!("failed to query target R libraries: {error}"))?
            .into_iter()
            .map(std::path::PathBuf::from)
            .collect();
        Ok(protocol::WorkerTarget {
            r_home: dunce::canonicalize(string("R.home()")?)
                .map_err(|error| format!("failed to canonicalize initialized R home: {error}"))?,
            r_version: string("paste0(R.version$major, '.', R.version$minor)")?,
            os: string("R.version$os")?,
            arch: string("R.version$arch")?,
            libraries,
            base_bindings: harp::environment::Environment::view(harp::environment::R_ENVS.base)
                .names(),
        })
    }

    fn package_index(
        &mut self,
        package: &protocol::PackageSpec,
    ) -> std::result::Result<WorkerPackageIndex, InspectionError> {
        let key = package.root.to_string_lossy().into_owned();
        if !self._contexts.contains_key(&key) {
            if !package.root.is_dir() {
                return Err(format!(
                    "installed package directory does not exist: {}",
                    package.root.display()
                )
                .into());
            }
            let context = harp::RFunction::new("", ".slinker_package_context")
                .add(package.root.to_string_lossy().into_owned())
                .add(package.name.clone())
                .call()
                .map_err(|error| {
                    format!(
                        "failed to create installed image for {}: {error}",
                        package.name
                    )
                })?;
            self._contexts.insert(
                key.clone(),
                PackageImageContext {
                    image: context,
                    epoch: InspectionEpoch {
                        worker: self.worker,
                        context: self._contexts.len() + 1,
                    },
                    private_ids: HashMap::new(),
                },
            );
        }
        let context = self
            ._contexts
            .get(&key)
            .expect("package image context inserted")
            .image
            .clone();
        let mut index = worker_package_index(&context)?;
        index.image_fingerprint = package.image_fingerprint.clone();
        Ok(index)
    }

    fn binding(
        &mut self,
        package: &protocol::PackageSpec,
        name: &str,
    ) -> std::result::Result<protocol::WorkerBinding, WorkerOperationError> {
        let index = self
            .package_index(package)
            .map_err(WorkerOperationError::with(WorkerErrorCode::PackageMetadata))?;
        let key = package.root.to_string_lossy();
        let context = self
            ._contexts
            .get(key.as_ref())
            .expect("package context created by index request");
        let image_environment = field(&context.image, "image_env")
            .map_err(WorkerOperationError::with(WorkerErrorCode::PackageMetadata))?;
        if !harp::environment::Environment::new(image_environment).exists(name) {
            return Err(WorkerOperationError::with(WorkerErrorCode::MissingBinding)(
                format!("installed image has no binding {name}").into(),
            ));
        }
        self.binding_value(package, name, index)
            .map_err(WorkerOperationError::with(WorkerErrorCode::BindingForce))
    }

    fn binding_value(
        &mut self,
        package: &protocol::PackageSpec,
        name: &str,
        index: WorkerPackageIndex,
    ) -> std::result::Result<protocol::WorkerBinding, InspectionError> {
        let key = package.root.to_string_lossy();
        let context = self
            ._contexts
            .get_mut(key.as_ref())
            .expect("package context created by index request");
        let image_environment = harp::RObjectExt::elt(&context.image, "image_env")
            .map_err(|error| format!("installed image has no image environment: {error}"))?;
        let environment = harp::environment::Environment::new(image_environment);
        let binding = harp::environment_iter::Binding::new(&environment, name.into())
            .map_err(|error| format!("failed to inspect binding {name}: {error}"))?;
        let origin = if Vec::<String>::try_from(&context.image.elt("sysdata_names")?)?
            .iter()
            .any(|candidate| candidate == name)
        {
            BindingOrigin::Sysdata
        } else {
            BindingOrigin::Code
        };
        let mut scanner = ObjectScanner::new(
            environment.inner.sexp,
            package.name.clone(),
            context.private_ids.clone(),
            context.epoch,
        );
        let binding = scanner.top_binding(name, origin, binding.value)?;
        context.private_ids = scanner.private_ids.clone();
        if binding.name != name || index.name != package.name || index.version != package.version {
            return Err(format!(
                "installed binding identity changed while inspecting {}::{name}",
                package.name
            )
            .into());
        }
        Ok(protocol::WorkerBinding {
            package_name: package.name.clone(),
            package_version: package.version.clone(),
            image_fingerprint: package.image_fingerprint.clone(),
            binding,
            private_environments: scanner.private_environments,
        })
    }

    fn serialize_bundle(
        &mut self,
        package: &protocol::PackageSpec,
        names: &[String],
    ) -> std::result::Result<Vec<u8>, WorkerOperationError> {
        self.package_index(package)
            .map_err(WorkerOperationError::with(WorkerErrorCode::PackageMetadata))?;
        let context = self
            ._contexts
            .get(package.root.to_string_lossy().as_ref())
            .expect("package context created by index request");
        let image_environment = field(&context.image, "image_env")
            .map_err(WorkerOperationError::with(WorkerErrorCode::PackageMetadata))?;
        harp::RFunction::new("", ".slinker_bundle")
            .add(image_environment)
            .add(names.to_vec())
            .call()
            .and_then(|bundle| Vec::<u8>::try_from(&bundle))
            .map_err(|error| {
                WorkerOperationError::with(WorkerErrorCode::BindingForce)(
                    format!("failed to serialize payload bundle: {error}").into(),
                )
            })
    }
}

#[derive(Debug, thiserror::Error)]
enum InspectionError {
    #[error(transparent)]
    R(Box<harp::Error>),
    #[error("{0}")]
    Failed(String),
}

impl From<harp::Error> for InspectionError {
    fn from(error: harp::Error) -> Self {
        Self::R(Box::new(error))
    }
}

impl From<String> for InspectionError {
    fn from(message: String) -> Self {
        Self::Failed(message)
    }
}

#[derive(Debug)]
struct WorkerOperationError {
    code: WorkerErrorCode,
    error: InspectionError,
}

impl WorkerOperationError {
    fn with(code: WorkerErrorCode) -> impl FnOnce(InspectionError) -> Self {
        move |error| Self { code, error }
    }
}

fn field(
    object: &harp::object::RObject,
    name: &str,
) -> std::result::Result<harp::object::RObject, InspectionError> {
    Ok(object.elt(name)?)
}

fn string_field(
    object: &harp::object::RObject,
    name: &str,
) -> std::result::Result<String, InspectionError> {
    Ok(String::try_from(field(object, name)?)?)
}

fn strings_field(
    object: &harp::object::RObject,
    name: &str,
) -> std::result::Result<Vec<String>, InspectionError> {
    Ok(Vec::<String>::try_from(field(object, name)?)?)
}

fn list_field(
    object: &harp::object::RObject,
    name: &str,
) -> std::result::Result<Vec<harp::object::RObject>, InspectionError> {
    Ok(Vec::<harp::object::RObject>::try_from(field(
        object, name,
    )?)?)
}

fn worker_package_index(
    context: &harp::object::RObject,
) -> std::result::Result<WorkerPackageIndex, InspectionError> {
    let namespace = field(context, "ns_info")?;
    let image_environment = field(context, "image_env")?;
    let installed_exports = field(&namespace, "exports")?;
    let export_bindings = Vec::<String>::try_from(&installed_exports)?;
    let mut export_names = names(installed_exports.sexp);
    if export_names.len() != export_bindings.len() {
        export_names = export_bindings.clone();
    }
    let mut exports = export_names
        .into_iter()
        .zip(export_bindings.into_iter().map(BindingName::from))
        .collect::<ExportMap>();
    for pattern in strings_field(&namespace, "exportPatterns")? {
        let matches = harp::RFunction::new("base", "ls")
            .add(image_environment.clone())
            .param("pattern", pattern)
            .param("all.names", true)
            .call()
            .and_then(Vec::<String>::try_from)?;
        exports.extend(
            matches
                .into_iter()
                .map(|name| (name.clone(), BindingName::from(name))),
        );
    }

    let imports = list_field(&namespace, "imports")?
        .into_iter()
        .map(|item| {
            if harp::utils::r_typeof(item.sexp) == libr::STRSXP {
                return Ok(ImportSpec::All {
                    package: String::try_from(item)?,
                    except: Vec::new(),
                });
            }
            let values = Vec::<harp::object::RObject>::try_from(&item)?;
            let package = values
                .first()
                .ok_or_else(|| InspectionError::from("installed import has no package".to_owned()))
                .and_then(|value| Ok(String::try_from(value)?))?;
            if names(item.sexp).iter().any(|name| name == "except") {
                return Ok(ImportSpec::All {
                    package,
                    except: strings_field(&item, "except")?,
                });
            }
            let remote_object = values
                .get(1)
                .ok_or_else(|| "installed importFrom has no bindings".to_owned())?;
            let remote = Vec::<String>::try_from(remote_object)?;
            let mut local = names(remote_object.sexp);
            if local.len() != remote.len() {
                local = remote.clone();
            } else {
                for (local, remote) in local.iter_mut().zip(&remote) {
                    if local.is_empty() {
                        local.clone_from(remote);
                    }
                }
            }
            Ok(ImportSpec::From {
                package,
                bindings: local
                    .into_iter()
                    .zip(remote)
                    .map(|(local, remote)| ImportBinding { local, remote })
                    .collect(),
            })
        })
        .collect::<std::result::Result<Vec<_>, InspectionError>>()?;

    let s3_object = field(&namespace, "S3methods")?;
    let s3_values = Vec::<Option<String>>::try_from(s3_object.clone())?;
    let dimensions = Vec::<i32>::try_from(harp::object::RObject::from(harp::object::r_dim(
        s3_object.sexp,
    )))?;
    let dimension = |index: usize| {
        usize::try_from(dimensions.get(index).copied().unwrap_or_default()).map_err(|_| {
            format!("installed S3 registration table has invalid dimensions {dimensions:?}")
        })
    };
    let rows = dimension(0)?;
    let columns = dimension(1)?;
    let mut s3 = Vec::with_capacity(rows);
    for row in 0..rows {
        let generic = s3_values
            .get(row)
            .and_then(Clone::clone)
            .ok_or_else(|| "installed S3 registration has no generic".to_owned())?;
        let class = s3_values
            .get(rows + row)
            .and_then(Clone::clone)
            .ok_or_else(|| "installed S3 registration has no class".to_owned())?;
        let method = s3_values
            .get(2 * rows + row)
            .and_then(Clone::clone)
            .unwrap_or_else(|| format!("{generic}.{class}"));
        let package = (columns >= 4)
            .then(|| s3_values.get(3 * rows + row).and_then(Clone::clone))
            .flatten();
        s3.push(S3Registration {
            generic: crate::package::GenericSpec {
                package,
                name: generic.into(),
            },
            class: class.into(),
            method: method.into(),
        });
    }

    let native_routines = field(&namespace, "nativeRoutines")?;
    let root = string_field(context, "root")?;
    let dynlibs = strings_field(&namespace, "dynlibs")?
        .into_iter()
        .map(|name| {
            let compiled = harp::RFunction::new("", ".slinker_native_library")
                .add(root.as_str())
                .add(name.as_str())
                .call()?;
            let native = native_routines.elt(name.as_str()).ok();
            let registered = native
                .as_ref()
                .and_then(|native| field(native, "useRegistration").ok())
                .and_then(|value| bool::try_from(value).ok())
                .unwrap_or(false);
            let fixes = native
                .as_ref()
                .and_then(|native| strings_field(native, "registrationFixes").ok())
                .unwrap_or_default();
            let symbols_object = native
                .as_ref()
                .and_then(|native| field(native, "symbolNames").ok());
            let symbols = symbols_object
                .as_ref()
                .and_then(|symbols| Vec::<String>::try_from(symbols).ok())
                .unwrap_or_default();
            let mut bindings = symbols_object
                .as_ref()
                .map(|symbols| names(symbols.sexp))
                .unwrap_or_default();
            if bindings.len() != symbols.len() {
                bindings.clone_from(&symbols);
            }
            Ok(NativeComponent {
                name,
                registration: registered.then(|| NativeRegistration {
                    prefix: fixes.first().cloned().unwrap_or_default(),
                    suffix: fixes.get(1).cloned().unwrap_or_default(),
                }),
                symbols: bindings
                    .into_iter()
                    .zip(symbols)
                    .map(|(binding, symbol)| NativeSymbolBinding { binding, symbol })
                    .collect(),
                routines: strings_field(&compiled, "routines")?,
                library: strings_field(&compiled, "library")?.into_iter().next(),
                safety: NativeSafety::Unanalyzed,
            })
        })
        .collect::<std::result::Result<Vec<_>, InspectionError>>()?;
    Ok(WorkerPackageIndex {
        name: string_field(context, "package")?,
        version: string_field(context, "version")?,
        image_fingerprint: String::new(),
        exports,
        imports,
        s3,
        dynlibs,
        on_load: strings_field(context, "binding_names")?
            .iter()
            .any(|name| name == ".onLoad"),
        binding_names: strings_field(context, "binding_names")?,
        datasets: strings_field(context, "dataset_names")?,
        has_sysdata: !strings_field(context, "sysdata_names")?.is_empty(),
    })
}

struct ObjectScanner {
    image_environment: libr::SEXP,
    package: String,
    private_ids: HashMap<libr::SEXP, String>,
    epoch: InspectionEpoch,
    visiting: HashSet<libr::SEXP>,
    walking: HashSet<libr::SEXP>,
    private_environments: HashMap<String, PrivateEnvironmentImage>,
}

impl ObjectScanner {
    fn new(
        image_environment: libr::SEXP,
        package: String,
        private_ids: HashMap<libr::SEXP, String>,
        epoch: InspectionEpoch,
    ) -> Self {
        Self {
            image_environment,
            package,
            private_ids,
            epoch,
            visiting: HashSet::new(),
            walking: HashSet::new(),
            private_environments: HashMap::new(),
        }
    }

    fn top_binding(
        &mut self,
        name: &str,
        origin: BindingOrigin,
        value: harp::environment_iter::BindingValue,
    ) -> std::result::Result<BindingImage, InspectionError> {
        let (representation, object) = match value {
            harp::environment_iter::BindingValue::Active { .. } => {
                return Ok(BindingImage {
                    name: name.into(),
                    origin,
                    representation: BindingRepresentation::ActiveBinding,
                    classes: Vec::new(),
                    object_kind: ObjectKind::ActiveBinding,
                    closure: None,
                    environment: None,
                    embedded_closures: Vec::new(),
                    embedded_environments: Vec::new(),
                    issues: Vec::new(),
                });
            }
            harp::environment_iter::BindingValue::Promise { promise } => (
                BindingRepresentation::LazyLoadPromise,
                harp::utils::r_promise_force_with_rollback(promise.sexp).map_err(|error| {
                    format!("failed to force demanded {}::{name}: {error}", self.package)
                })?,
            ),
            harp::environment_iter::BindingValue::Altrep { object, .. } => {
                let class = harp::utils::r_altrep_class(object.sexp);
                return Ok(BindingImage {
                    name: name.into(),
                    origin,
                    representation: BindingRepresentation::Altrep {
                        class: class.clone(),
                    },
                    classes: Vec::new(),
                    object_kind: ObjectKind::Altrep,
                    closure: None,
                    environment: None,
                    embedded_closures: Vec::new(),
                    embedded_environments: Vec::new(),
                    issues: altrep_issues("$", class),
                });
            }
            harp::environment_iter::BindingValue::Standard { object } => {
                (BindingRepresentation::Value, object)
            }
        };
        let mut facts = self.scan_value(object.sexp, "$", Some(name), 0)?;
        Ok(BindingImage {
            name: name.into(),
            origin,
            representation,
            classes: classes(object.sexp)
                .into_iter()
                .map(ClassName::from)
                .collect(),
            object_kind: facts.kind,
            closure: facts.closure.take(),
            environment: facts.environment.take(),
            embedded_closures: facts.closures,
            embedded_environments: facts.environments,
            issues: facts.issues,
        })
    }

    fn private_binding(
        &mut self,
        name: &str,
        value: harp::environment_iter::BindingValue,
    ) -> std::result::Result<PrivateBindingImage, InspectionError> {
        let (representation, object) = match value {
            harp::environment_iter::BindingValue::Active { .. } => {
                return Ok(PrivateBindingImage {
                    name: name.into(),
                    representation: BindingRepresentation::ActiveBinding,
                    classes: Vec::new(),
                    object_kind: ObjectKind::ActiveBinding,
                    closure: None,
                    environment: None,
                    embedded_closures: Vec::new(),
                    embedded_environments: Vec::new(),
                    issues: Vec::new(),
                });
            }
            harp::environment_iter::BindingValue::Promise { promise } => {
                let forced = harp::utils::r_promise_is_forced(promise.sexp);
                if !forced {
                    return Ok(PrivateBindingImage {
                        name: name.into(),
                        representation: BindingRepresentation::Promise { forced: false },
                        classes: Vec::new(),
                        object_kind: ObjectKind::Promise,
                        closure: None,
                        environment: None,
                        embedded_closures: Vec::new(),
                        embedded_environments: Vec::new(),
                        issues: vec![ObjectIssue {
                            path: "$".into(),
                            kind: "unforced_promise".into(),
                            detail: "nested promise is preserved without forcing".into(),
                        }],
                    });
                }
                (
                    BindingRepresentation::Promise { forced: true },
                    harp::object::RObject::from(harp::utils::r_promise_value(promise.sexp)),
                )
            }
            harp::environment_iter::BindingValue::Altrep { object, .. } => {
                let class = harp::utils::r_altrep_class(object.sexp);
                return Ok(PrivateBindingImage {
                    name: name.into(),
                    representation: BindingRepresentation::Altrep {
                        class: class.clone(),
                    },
                    classes: Vec::new(),
                    object_kind: ObjectKind::Altrep,
                    closure: None,
                    environment: None,
                    embedded_closures: Vec::new(),
                    embedded_environments: Vec::new(),
                    issues: altrep_issues("$", class),
                });
            }
            harp::environment_iter::BindingValue::Standard { object } => {
                (BindingRepresentation::Value, object)
            }
        };
        let mut facts = self.scan_value(object.sexp, "$", Some(name), 0)?;
        Ok(PrivateBindingImage {
            name: name.into(),
            representation,
            classes: classes(object.sexp)
                .into_iter()
                .map(ClassName::from)
                .collect(),
            object_kind: facts.kind,
            closure: facts.closure.take(),
            environment: facts.environment.take(),
            embedded_closures: facts.closures,
            embedded_environments: facts.environments,
            issues: facts.issues,
        })
    }

    fn scan_value(
        &mut self,
        value: libr::SEXP,
        path: &str,
        binding: Option<&str>,
        depth: usize,
    ) -> std::result::Result<ObjectFacts, InspectionError> {
        if depth > 128 {
            return Ok(ObjectFacts::issue(
                ObjectKind::Other("depth".into()),
                path,
                "object_depth",
                "object graph exceeds 128 levels",
            ));
        }
        if harp::utils::r_is_altrep(value) {
            let mut facts = ObjectFacts::new(ObjectKind::Altrep);
            facts.issues = altrep_issues(path, harp::utils::r_altrep_class(value));
            return Ok(facts);
        }
        let recursive = matches!(
            harp::utils::r_typeof(value),
            libr::CLOSXP | libr::ENVSXP | libr::VECSXP | libr::LISTSXP
        );
        if recursive && !self.walking.insert(value) {
            let mut facts = ObjectFacts::new(object_kind(value));
            if harp::utils::r_typeof(value) == libr::ENVSXP {
                let environment = self.environment_ref(value)?;
                if binding.is_none() && !environment.starts_with("unsupported:") {
                    facts.environments.push(EmbeddedEnvironmentRef {
                        path: path.into(),
                        environment: environment.clone(),
                    });
                }
                facts.environment = Some(environment);
            }
            return Ok(facts);
        }
        let mut facts = ObjectFacts::new(object_kind(value));
        match harp::utils::r_typeof(value) {
            libr::CLOSXP => {
                let closure_environment = harp::RFunction::new("base", "environment")
                    .add(value)
                    .call()?;
                let environment = self.environment_ref(closure_environment.sexp)?;
                let source = self.deparse(binding, value)?.into();
                match binding {
                    Some(_) => {
                        facts.closure = Some(ClosureSource {
                            environment: environment.clone(),
                            source,
                        });
                    }
                    None => facts.closures.push(EmbeddedClosureSource {
                        path: path.into(),
                        environment: environment.clone(),
                        source,
                    }),
                }
                facts.environment = Some(environment);
            }
            libr::ENVSXP => {
                let environment = self.environment_ref(value)?;
                if binding.is_none() && !environment.starts_with("unsupported:") {
                    facts.environments.push(EmbeddedEnvironmentRef {
                        path: path.into(),
                        environment: environment.clone(),
                    });
                }
                if let Some(detail) = environment.strip_prefix("unsupported:") {
                    facts.issues.push(ObjectIssue {
                        path: path.into(),
                        kind: "environment_identity".into(),
                        detail: detail.into(),
                    });
                }
                facts.environment = Some(environment);
            }
            libr::VECSXP => {
                let names = names(value);
                for index in 0..harp::object::r_length(value) {
                    let member = usize::try_from(index)
                        .ok()
                        .and_then(|index| names.get(index))
                        .filter(|name| !name.is_empty())
                        .map_or_else(|| format!("[[{}]]", index + 1), |name| format!("${name}"));
                    facts.merge(self.scan_value(
                        harp::object::list_get(value, index),
                        &format!("{path}{member}"),
                        None,
                        depth + 1,
                    )?);
                }
            }
            libr::LISTSXP => {
                let mut node = value;
                let mut index = 1;
                while node != unsafe { libr::R_NilValue } {
                    let item = unsafe { libr::CAR(node) };
                    if item != unsafe { libr::R_MissingArg } {
                        let tag = symbol_name(unsafe { libr::TAG(node) });
                        let member = tag
                            .filter(|name| !name.is_empty())
                            .map_or_else(|| format!("[[{index}]]"), |name| format!("${name}"));
                        facts.merge(self.scan_value(
                            item,
                            &format!("{path}{member}"),
                            None,
                            depth + 1,
                        )?);
                    }
                    node = unsafe { libr::CDR(node) };
                    index += 1;
                }
            }
            libr::EXTPTRSXP => facts.issues.push(ObjectIssue {
                path: path.into(),
                kind: "external_pointer".into(),
                detail: "external pointer".into(),
            }),
            libr::WEAKREFSXP => facts.issues.push(ObjectIssue {
                path: path.into(),
                kind: "weak_reference".into(),
                detail: "weak reference".into(),
            }),
            _ => {}
        }
        if !matches!(harp::utils::r_typeof(value), libr::LANGSXP | libr::EXPRSXP) {
            let mut attribute = unsafe { libr::ATTRIB(value) };
            while attribute != unsafe { libr::R_NilValue } {
                let name =
                    symbol_name(unsafe { libr::TAG(attribute) }).unwrap_or_else(|| "?".into());
                facts.merge(self.scan_value(
                    unsafe { libr::CAR(attribute) },
                    &format!("{path}.attr[{name}]"),
                    None,
                    depth + 1,
                )?);
                attribute = unsafe { libr::CDR(attribute) };
            }
        }
        if recursive {
            self.walking.remove(&value);
        }
        Ok(facts)
    }

    fn deparse(
        &self,
        binding: Option<&str>,
        value: libr::SEXP,
    ) -> std::result::Result<String, InspectionError> {
        harp::RFunction::new("", ".slinker_deparse_binding")
            .add(binding.unwrap_or(".slinker_embedded"))
            .add(value)
            .call()
            .and_then(String::try_from)
            .map_err(InspectionError::from)
    }

    fn environment_ref(
        &mut self,
        environment: libr::SEXP,
    ) -> std::result::Result<String, InspectionError> {
        if environment == self.image_environment {
            return Ok(format!("namespace:{}", self.package));
        }
        if environment == unsafe { libr::R_BaseNamespace } {
            return Ok("namespace:base".into());
        }
        if environment == unsafe { libr::R_BaseEnv } {
            return Ok("base:base".into());
        }
        if environment == unsafe { libr::R_EmptyEnv } {
            return Ok("base:empty".into());
        }
        if environment == unsafe { libr::R_GlobalEnv } {
            return Ok("unsupported:global".into());
        }
        if harp::utils::r_env_is_ns_env(environment) {
            return harp::utils::r_envir_name(environment)
                .map(|name| format!("namespace:{name}"))
                .map_err(InspectionError::from);
        }
        if harp::utils::r_env_is_pkg_env(environment) {
            return harp::utils::r_envir_name(environment)
                .map(|name| format!("unsupported:{name}"))
                .map_err(InspectionError::from);
        }
        let pointer = environment;
        if let Some(id) = self.private_ids.get(&pointer) {
            return Ok(id.clone());
        }
        let id = format!("private:{}:{}", self.epoch, self.private_ids.len() + 1);
        self.private_ids.insert(pointer, id.clone());
        self.inventory_private(environment, &id)?;
        Ok(id)
    }

    fn inventory_private(
        &mut self,
        environment: libr::SEXP,
        id: &str,
    ) -> std::result::Result<(), InspectionError> {
        let pointer = environment;
        if !self.visiting.insert(pointer) || self.private_environments.contains_key(id) {
            return Ok(());
        }
        self.private_environments.insert(
            id.into(),
            PrivateEnvironmentImage {
                id: id.into(),
                parent: "base:empty".into(),
                bindings: HashMap::new(),
            },
        );
        let parent = self.environment_ref(harp::r::env_parent(environment))?;
        let bindings = harp::environment::Environment::view(environment)
            .iter()
            .map(|binding| {
                let binding = binding?;
                let name = String::from(binding.name);
                Ok((
                    BindingName::from(name.as_str()),
                    self.private_binding(&name, binding.value)?,
                ))
            })
            .collect::<std::result::Result<HashMap<_, _>, InspectionError>>()?;
        self.private_environments.insert(
            id.into(),
            PrivateEnvironmentImage {
                id: id.into(),
                parent,
                bindings,
            },
        );
        self.visiting.remove(&pointer);
        Ok(())
    }
}

struct ObjectFacts {
    kind: ObjectKind,
    closure: Option<ClosureSource>,
    environment: Option<String>,
    closures: Vec<EmbeddedClosureSource>,
    environments: Vec<EmbeddedEnvironmentRef>,
    issues: Vec<ObjectIssue>,
}

impl ObjectFacts {
    fn new(kind: ObjectKind) -> Self {
        Self {
            kind,
            closure: None,
            environment: None,
            closures: Vec::new(),
            environments: Vec::new(),
            issues: Vec::new(),
        }
    }
    fn issue(kind: ObjectKind, path: &str, issue: &str, detail: &str) -> Self {
        let mut facts = Self::new(kind);
        facts.issues.push(ObjectIssue {
            path: path.into(),
            kind: issue.into(),
            detail: detail.into(),
        });
        facts
    }
    fn merge(&mut self, mut other: Self) {
        self.closures.append(&mut other.closures);
        self.environments.append(&mut other.environments);
        self.issues.append(&mut other.issues);
    }
}

fn object_kind(value: libr::SEXP) -> ObjectKind {
    match harp::utils::r_typeof(value) {
        libr::NILSXP => ObjectKind::Null,
        libr::CLOSXP => ObjectKind::Closure,
        libr::LGLSXP => ObjectKind::Logical,
        libr::INTSXP => ObjectKind::Integer,
        libr::REALSXP => ObjectKind::Double,
        libr::CPLXSXP => ObjectKind::Complex,
        libr::STRSXP => ObjectKind::Character,
        libr::RAWSXP => ObjectKind::Raw,
        libr::SYMSXP => ObjectKind::Symbol,
        libr::VECSXP => ObjectKind::List,
        libr::LISTSXP => ObjectKind::Pairlist,
        libr::LANGSXP => ObjectKind::Language,
        libr::EXPRSXP => ObjectKind::Expression,
        libr::ENVSXP => ObjectKind::Environment,
        libr::BUILTINSXP => ObjectKind::Builtin,
        libr::SPECIALSXP => ObjectKind::Special,
        libr::PROMSXP => ObjectKind::Promise,
        libr::EXTPTRSXP => ObjectKind::ExternalPointer,
        libr::WEAKREFSXP => ObjectKind::WeakReference,
        kind => ObjectKind::Other(format!("SEXPTYPE {kind}")),
    }
}

fn names(value: libr::SEXP) -> Vec<String> {
    let names = unsafe { libr::Rf_getAttrib(value, libr::R_NamesSymbol) };
    Vec::<String>::try_from(harp::object::RObject::from(names)).unwrap_or_default()
}

fn altrep_issues(path: &str, class: String) -> Vec<ObjectIssue> {
    if class.starts_with("base::") {
        return Vec::new();
    }
    vec![ObjectIssue {
        path: path.into(),
        kind: "altrep".into(),
        detail: class,
    }]
}

fn classes(value: libr::SEXP) -> Vec<String> {
    let class = unsafe { libr::Rf_getAttrib(value, libr::R_ClassSymbol) };
    Vec::<String>::try_from(harp::object::RObject::from(class)).unwrap_or_default()
}

fn symbol_name(symbol: libr::SEXP) -> Option<String> {
    if symbol == unsafe { libr::R_NilValue } {
        return None;
    }
    let chars = unsafe { libr::PRINTNAME(symbol) };
    String::try_from(harp::object::RObject::from(chars)).ok()
}

fn sanitize_environment(r_home: &std::path::Path) {
    // Safety: worker startup is single-threaded and no library code is running
    // concurrently while its process-local environment is established.
    unsafe {
        std::env::set_var("R_HOME", r_home);
        std::env::set_var(
            "R_ENVIRON_USER",
            r_home.join("etc").join("__slinker_no_user_Renviron__"),
        );
        for name in ["R_PROFILE_USER", "R_LIBS", "R_LIBS_USER", "R_LIBS_SITE"] {
            std::env::remove_var(name);
        }
    }
}

fn configure_libraries(
    libraries: &[std::path::PathBuf],
) -> std::result::Result<(), InspectionError> {
    if libraries.is_empty() {
        return Ok(());
    }
    let libraries = libraries
        .iter()
        .map(|library| library.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    harp::RFunction::new("base", ".libPaths")
        .add(libraries)
        .call()
        .map(|_| ())
        .map_err(|error| format!("failed to select R library universe: {error}").into())
}

fn install_helpers() -> std::result::Result<(), InspectionError> {
    harp::parse_eval_global(include_str!("helpers.R"))
        .map(|_| ())
        .map_err(|error| format!("failed to initialize installed-image helpers: {error}").into())
}

pub fn run(protocol_path: &std::path::Path) -> Result<()> {
    let stdin = io::stdin();
    let mut output = OpenOptions::new()
        .append(true)
        .open(protocol_path)
        .map_err(|source| Error::Io {
            path: protocol_path.to_path_buf(),
            source,
        })?;
    let mut runtime = None;

    for line in stdin.lock().lines() {
        let line = line.map_err(|source| Error::Io {
            path: "<r-worker-stdin>".into(),
            source,
        })?;
        if line.trim().is_empty() {
            continue;
        }

        let request: WorkerRequest = match serde_json::from_str(&line) {
            Ok(request) => request,
            Err(error) => {
                write_response(
                    &mut output,
                    &worker_failure(
                        None,
                        WorkerErrorCode::Protocol,
                        format!("invalid worker request: {error}"),
                        None,
                        None,
                    ),
                )?;
                continue;
            }
        };

        let response = match request {
            WorkerRequest::Hello { protocol, target } => {
                if protocol != PROTOCOL_VERSION {
                    worker_failure(
                        None,
                        WorkerErrorCode::Protocol,
                        format!(
                            "unsupported worker protocol {protocol}; expected {PROTOCOL_VERSION}"
                        ),
                        None,
                        None,
                    )
                } else {
                    match WorkerRuntime::start(&target) {
                        Ok(started) => match started.target() {
                            Ok(target) => {
                                runtime = Some(started);
                                WorkerResponse::Hello {
                                    protocol: PROTOCOL_VERSION,
                                    harp_worker: true,
                                    target,
                                }
                            }
                            Err(error) => operation_failure(
                                None,
                                WorkerOperationError::with(WorkerErrorCode::RuntimeStartup)(error),
                                None,
                                None,
                            ),
                        },
                        Err(error) => operation_failure(
                            None,
                            WorkerOperationError::with(WorkerErrorCode::RuntimeStartup)(error),
                            None,
                            None,
                        ),
                    }
                }
            }
            WorkerRequest::Shutdown => {
                write_response(&mut output, &WorkerResponse::Shutdown)?;
                return Ok(());
            }
            WorkerRequest::ValidateSyntax { request_id, source } => match runtime.as_ref() {
                Some(runtime) => match runtime.validate_syntax(&source) {
                    Ok(()) => WorkerResponse::SyntaxValidation {
                        request_id,
                        accepted: true,
                        message: None,
                    },
                    Err(error) => WorkerResponse::SyntaxValidation {
                        request_id,
                        accepted: false,
                        message: Some(error.to_string()),
                    },
                },
                None => worker_failure(
                    Some(request_id),
                    WorkerErrorCode::RuntimeStartup,
                    "Harp worker must receive hello before semantic requests",
                    None,
                    None,
                ),
            },
            WorkerRequest::NormalizeSyntax { request_id, source } => match runtime.as_ref() {
                Some(runtime) => match runtime.normalize_syntax(&source) {
                    Ok(source) => WorkerResponse::NormalizedSyntax { request_id, source },
                    Err(error) => operation_failure(
                        Some(request_id),
                        WorkerOperationError::with(WorkerErrorCode::TargetSyntaxRejection)(error),
                        None,
                        None,
                    ),
                },
                None => worker_failure(
                    Some(request_id),
                    WorkerErrorCode::RuntimeStartup,
                    "Harp worker must receive hello before semantic requests",
                    None,
                    None,
                ),
            },
            WorkerRequest::PackageIndex {
                request_id,
                package,
            } => match runtime.as_mut() {
                Some(runtime) => match runtime.package_index(&package) {
                    Ok(index) => WorkerResponse::PackageIndex { request_id, index },
                    Err(error) => operation_failure(
                        Some(request_id),
                        WorkerOperationError::with(WorkerErrorCode::PackageMetadata)(error),
                        Some(&package),
                        None,
                    ),
                },
                None => worker_failure(
                    Some(request_id),
                    WorkerErrorCode::RuntimeStartup,
                    "Harp worker must receive hello before semantic requests",
                    Some(&package),
                    None,
                ),
            },
            WorkerRequest::Binding {
                request_id,
                package,
                name,
            } => match runtime.as_mut() {
                Some(runtime) => match runtime.binding(&package, &name) {
                    Ok(binding) => WorkerResponse::Binding {
                        request_id,
                        binding,
                    },
                    Err(error) => {
                        operation_failure(Some(request_id), error, Some(&package), Some(name))
                    }
                },
                None => worker_failure(
                    Some(request_id),
                    WorkerErrorCode::RuntimeStartup,
                    "Harp worker must receive hello before semantic requests",
                    Some(&package),
                    Some(name),
                ),
            },
            WorkerRequest::SerializeBundle {
                request_id,
                package,
                names,
            } => match runtime.as_mut() {
                Some(runtime) => match runtime.serialize_bundle(&package, &names) {
                    Ok(bytes) => WorkerResponse::Payload { request_id, bytes },
                    Err(error) => operation_failure(Some(request_id), error, Some(&package), None),
                },
                None => worker_failure(
                    Some(request_id),
                    WorkerErrorCode::RuntimeStartup,
                    "Harp worker must receive hello before semantic requests",
                    Some(&package),
                    None,
                ),
            },
        };

        write_response(&mut output, &response)?;
    }

    Ok(())
}

fn operation_failure(
    request_id: Option<u64>,
    error: WorkerOperationError,
    package: Option<&protocol::PackageSpec>,
    binding: Option<String>,
) -> WorkerResponse {
    worker_failure(
        request_id,
        error.code,
        error.error.to_string(),
        package,
        binding,
    )
}

fn worker_failure(
    request_id: Option<u64>,
    code: WorkerErrorCode,
    message: impl Into<String>,
    package: Option<&protocol::PackageSpec>,
    binding: Option<String>,
) -> WorkerResponse {
    WorkerResponse::Error {
        error: WorkerFailure {
            request_id,
            package: package.map(|package| WorkerPackageIdentity {
                name: package.name.clone(),
                version: package.version.clone(),
                image_fingerprint: package.image_fingerprint.clone(),
            }),
            binding,
            code,
            message: message.into(),
            captured_output: Vec::new(),
        },
    }
}

fn write_response(writer: &mut impl Write, response: &WorkerResponse) -> Result<()> {
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

#[cfg(test)]
mod tests {
    use super::protocol::*;
    use super::*;
    use crate::package::BindingRepresentation;

    #[test]
    fn protocol_round_trips_binding_request() {
        let request = WorkerRequest::Binding {
            request_id: 7,
            package: PackageSpec {
                name: "fixture".into(),
                version: "1.0.0".into(),
                image_fingerprint: "image".into(),
                root: "/tmp/fixture".into(),
            },
            name: "foo".into(),
        };
        let json = serde_json::to_string(&request).unwrap();
        let decoded: WorkerRequest = serde_json::from_str(&json).unwrap();
        match decoded {
            WorkerRequest::Binding {
                request_id, name, ..
            } => {
                assert_eq!(request_id, 7);
                assert_eq!(name, "foo");
            }
            _ => panic!("wrong request variant"),
        }
    }

    #[test]
    fn harp_inspection_preserves_lazy_active_altrep_and_private_state() {
        let r_home = test_r_home().expect("selected R installation");
        let (fixture_root, fixture_library, fixture_temp) =
            install_fixture(&r_home).expect("install worker fixture package");
        let target = TargetSpec {
            r_home,
            worker: 0,
            arch: match std::env::consts::ARCH {
                "x86" => "i386".into(),
                arch => arch.into(),
            },
            libraries: vec![fixture_library.clone()],
        };
        let mut runtime = WorkerRuntime::start(&target).expect("start selected target R");
        let initial_target = runtime.target().expect("capture initialized target");
        assert_eq!(
            dunce::canonicalize(&initial_target.libraries[0]).expect("canonical first library"),
            dunce::canonicalize(&fixture_library).expect("canonical fixture library")
        );
        let package = PackageSpec {
            name: "harpfixture".into(),
            version: "1.0.0".into(),
            image_fingerprint: "fixture-image".into(),
            root: fixture_root,
        };
        let index = runtime
            .package_index(&package)
            .expect("index exact fixture root");
        assert!(index.on_load);
        assert!(index.binding_names.iter().any(|name| name == "good"));
        assert!(index.s3.iter().any(|registration| {
            registration.generic.name == "head"
                && registration.generic.package.as_deref() == Some("utils")
                && registration.method == "head.harpfixture"
        }));
        let on_load_ran = || {
            harp::parse_eval_base("isTRUE(getOption(\"harpfixture.onload\"))")
                .and_then(bool::try_from)
                .expect("query fixture load hook option")
        };
        assert!(!on_load_ran(), "indexing executed .onLoad");
        let good = runtime
            .binding(&package, "good")
            .expect("inspect demanded binding");
        assert!(good.binding.closure.is_some());
        assert!(!on_load_ran(), "binding inspection executed .onLoad");
        assert_eq!(
            runtime.target().expect("target after inspection").libraries,
            initial_target.libraries,
            "package inspection mutated .libPaths()"
        );
        let fixture = harp::parse_eval_global(
            r#"
            local({
              image <- new.env(parent = baseenv())
              private <- new.env(parent = baseenv())
              private$counter <- 0L
              makeActiveBinding("active", function() {
                private$counter <- private$counter + 1L
                1L
              }, private)
              delayedAssign("promise", {
                private$counter <- private$counter + 1L
                2L
              }, assign.env = private)
              private$self <- private
              private$handler <- function(expr) expr
              image$holder <- list(private = private, closure = function(x) x)
              class(image$holder) <- c("first_class", "second_class")
              image$counter <- 0L
              delayedAssign("unrelated", {
                image$counter <- image$counter + 1L
                99L
              }, assign.env = image)
              delayedAssign("lazy", function() 1L, assign.env = image)
              image$altrep <- 1:1000000
              list(image = image, private = private)
            })
            "#,
        )
        .expect("create worker fixture");
        let image = field(&fixture, "image").expect("image environment");
        let image_environment = harp::environment::Environment::new(image.clone());

        let lazy = harp::environment_iter::Binding::new(&image_environment, "lazy".into())
            .expect("lazy binding");
        let mut scanner = ObjectScanner::new(
            image.sexp,
            "fixture".into(),
            HashMap::new(),
            InspectionEpoch {
                worker: 0,
                context: 0,
            },
        );
        let lazy = scanner
            .top_binding("lazy", BindingOrigin::Code, lazy.value)
            .expect("inspect demanded promise");
        assert_eq!(lazy.representation, BindingRepresentation::LazyLoadPromise);
        assert!(lazy.closure.is_some());
        assert_eq!(
            i32::try_from(image_environment.get("counter").expect("image counter"))
                .expect("integer"),
            0,
            "demanding lazy forced unrelated"
        );

        let holder = harp::environment_iter::Binding::new(&image_environment, "holder".into())
            .expect("holder binding");
        let holder = scanner
            .top_binding("holder", BindingOrigin::Code, holder.value)
            .expect("inspect retained private environment");
        assert_eq!(holder.embedded_closures.len(), 1);
        assert!(holder.embedded_closures[0].source.contains("function"));
        assert_eq!(holder.classes, ["first_class", "second_class"]);
        let private = scanner
            .private_environments
            .values()
            .find(|environment| environment.bindings.contains_key("active"))
            .expect("private environment");
        assert_eq!(
            private.bindings["active"].representation,
            BindingRepresentation::ActiveBinding
        );
        assert_eq!(
            private.bindings["promise"].representation,
            BindingRepresentation::Promise { forced: false }
        );
        assert_eq!(
            private.bindings["self"].environment.as_deref(),
            Some(private.id.as_str())
        );
        assert!(
            private.bindings["handler"]
                .closure
                .as_ref()
                .is_some_and(|closure| closure.source.starts_with("handler <- function"))
        );
        let mut later_epoch = ObjectScanner::new(
            image.sexp,
            "fixture".into(),
            HashMap::new(),
            InspectionEpoch {
                worker: 0,
                context: 1,
            },
        );
        let rescanned = harp::environment_iter::Binding::new(&image_environment, "holder".into())
            .expect("holder binding");
        later_epoch
            .top_binding("holder", BindingOrigin::Code, rescanned.value)
            .expect("inspect holder in a later epoch");
        assert!(
            later_epoch
                .private_environments
                .keys()
                .all(|label| !scanner.private_environments.contains_key(label)),
            "private labels from separate inspection epochs alias"
        );
        let private_environment = harp::environment::Environment::new(
            field(&fixture, "private").expect("private fixture environment"),
        );
        assert_eq!(
            i32::try_from(private_environment.get("counter").expect("counter")).expect("integer"),
            0
        );

        let altrep = harp::environment_iter::Binding::new(&image_environment, "altrep".into())
            .expect("ALTREP binding");
        let altrep = scanner
            .top_binding("altrep", BindingOrigin::Code, altrep.value)
            .expect("classify ALTREP");
        assert!(matches!(
            altrep.representation,
            BindingRepresentation::Altrep { .. }
        ));
        assert!(
            altrep.issues.is_empty(),
            "base ALTREP serializes as a plain vector"
        );

        harp::parse_eval_global("cat('worker console noise')")
            .expect("write through the embedded R console");
        let response = WorkerResponse::SyntaxValidation {
            request_id: 19,
            accepted: true,
            message: None,
        };
        let mut protocol = Vec::new();
        write_response(&mut protocol, &response).expect("write isolated protocol response");
        assert!(matches!(
            serde_json::from_slice::<WorkerResponse>(&protocol).expect("decode protocol response"),
            WorkerResponse::SyntaxValidation {
                request_id: 19,
                accepted: true,
                ..
            }
        ));
        assert!(runtime.validate_syntax("function(").is_err());
        assert_eq!(
            runtime
                .target()
                .expect("recapture initialized target")
                .libraries,
            initial_target.libraries
        );
        drop(runtime);
        std::fs::remove_dir_all(fixture_temp).expect("remove installed fixture");
    }

    fn test_r_home() -> Option<std::path::PathBuf> {
        if let Some(home) = std::env::var_os("R_HOME") {
            return dunce::canonicalize(home).ok();
        }
        let output = if cfg!(windows) {
            std::process::Command::new("cmd")
                .args(["/c", "R RHOME"])
                .output()
                .ok()?
        } else {
            std::process::Command::new("R").arg("RHOME").output().ok()?
        };
        let home = String::from_utf8(output.stdout).ok()?;
        dunce::canonicalize(
            home.lines()
                .rev()
                .find(|line| !line.trim().is_empty())?
                .trim(),
        )
        .ok()
    }

    fn install_fixture(
        r_home: &std::path::Path,
    ) -> Option<(std::path::PathBuf, std::path::PathBuf, std::path::PathBuf)> {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let temp = std::env::temp_dir().join(format!(
            "slinker-harp-fixture-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let source = temp.join("source");
        let library = temp.join("library");
        std::fs::create_dir_all(source.join("R")).ok()?;
        std::fs::create_dir_all(&library).ok()?;
        std::fs::write(
            source.join("DESCRIPTION"),
            "Package: harpfixture\nVersion: 1.0.0\nTitle: Harp fixture\nDescription: Harp worker fixture.\nAuthors@R: person('A', 'B', email='a@example.com', role=c('aut','cre'))\nLicense: MIT\nEncoding: UTF-8\n",
        )
        .ok()?;
        std::fs::write(
            source.join("NAMESPACE"),
            "export(good)\nS3method(utils::head, harpfixture)\n",
        )
        .ok()?;
        std::fs::write(
            source.join("R").join("fixture.R"),
            r#"
good <- function() 1L
head.harpfixture <- function(x, ...) x
unrelated <- function() stop("unrelated binding executed")
.onLoad <- function(...) options(harpfixture.onload = TRUE)
"#,
        )
        .ok()?;
        let executable = [
            r_home.join("bin").join("x64").join("R.exe"),
            r_home.join("bin").join("R.exe"),
            r_home.join("bin").join("R"),
        ]
        .into_iter()
        .find(|path| path.is_file())?;
        let status = std::process::Command::new(executable)
            .args(["CMD", "INSTALL", "--no-test-load"])
            .arg(format!("--library={}", library.display()))
            .arg(&source)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .ok()?;
        if !status.success() {
            return None;
        }
        Some((library.join("harpfixture"), library, temp))
    }
}
