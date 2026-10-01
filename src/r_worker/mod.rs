pub(crate) mod client;
pub mod protocol;

use crate::package::{
    BindingImage, BindingName, BindingOrigin, BindingRepresentation, ClassName, ClosureSource,
    EmbeddedClosureSource, EmbeddedEnvironmentRef, ExportMap, ImportBinding, ImportSpec,
    NameLookup, NativeComponent, NativeLibrary, NativeRegistration, NativeRoutines, NativeSafety,
    NativeSymbolBinding, ObjectIssue, ObjectKind, PackageName, PrivateBindingImage,
    PrivateEnvironmentImage, S3Registration,
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
    index: WorkerPackageIndex,
    epoch: InspectionEpoch,
    private_ids: HashMap<libr::SEXP, String>,
}

struct WorkerRuntime {
    worker: u64,
    _arguments: Vec<CString>,
    contexts: HashMap<String, PackageImageContext>,
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
            contexts: HashMap::new(),
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

    fn canonical_syntax(
        &self,
        source: &str,
    ) -> std::result::Result<(String, bool), InspectionError> {
        let normalized = self.normalize_syntax(source)?;
        let stable = self.normalize_syntax(&normalized)? == normalized;
        Ok((normalized, stable))
    }

    fn verify_relocation(
        &self,
        original: &str,
        rewritten: &str,
        sites: &[protocol::RelocationSiteSpec],
    ) -> std::result::Result<(), InspectionError> {
        let column = |field: fn(&protocol::RelocationSiteSpec) -> String| {
            sites.iter().map(field).collect::<Vec<_>>()
        };
        harp::RFunction::new("", ".slinker_verify_relocation")
            .add(original)
            .add(rewritten)
            .add(column(|site| site.start.to_string()))
            .add(column(|site| site.end.to_string()))
            .add(column(|site| site.replacement.clone()))
            .add(column(|site| {
                site.appended_argument
                    .as_ref()
                    .map_or_else(String::new, |argument| argument.name.clone())
            }))
            .add(column(|site| {
                site.appended_argument
                    .as_ref()
                    .map_or_else(String::new, |argument| argument.value.clone())
            }))
            .call()
            .map(|_| ())
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

    fn context(
        &mut self,
        package: &protocol::PackageSpec,
    ) -> std::result::Result<&mut PackageImageContext, InspectionError> {
        let key = package.root.to_string_lossy().into_owned();
        if !self.contexts.contains_key(&key) {
            if !package.root.is_dir() {
                return Err(format!(
                    "installed package directory does not exist: {}",
                    package.root.display()
                )
                .into());
            }
            let image = harp::RFunction::new("", ".slinker_package_context")
                .add(package.root.to_string_lossy().into_owned())
                .add(package.name.clone())
                .call()
                .map_err(|error| {
                    format!(
                        "failed to create installed image for {}: {error}",
                        package.name
                    )
                })?;
            let index = worker_package_index(&image)?;
            self.contexts.insert(
                key.clone(),
                PackageImageContext {
                    image,
                    index,
                    epoch: InspectionEpoch {
                        worker: self.worker,
                        context: self.contexts.len() + 1,
                    },
                    private_ids: HashMap::new(),
                },
            );
        }
        Ok(self
            .contexts
            .get_mut(&key)
            .expect("package image context inserted"))
    }

    fn package_index(
        &mut self,
        package: &protocol::PackageSpec,
    ) -> std::result::Result<WorkerPackageIndex, InspectionError> {
        let mut index = self.context(package)?.index.clone();
        index
            .image_fingerprint
            .clone_from(&package.image_fingerprint);
        Ok(index)
    }

    fn binding(
        &mut self,
        package: &protocol::PackageSpec,
        name: &str,
    ) -> std::result::Result<protocol::WorkerBinding, WorkerOperationError> {
        let context = self
            .context(package)
            .map_err(WorkerOperationError::with(WorkerErrorCode::PackageMetadata))?;
        let image_environment = field(&context.image, "image_env")
            .map_err(WorkerOperationError::with(WorkerErrorCode::PackageMetadata))?;
        if !harp::environment::Environment::new(image_environment).exists(name) {
            return Err(WorkerOperationError::with(WorkerErrorCode::MissingBinding)(
                format!("installed image has no binding {name}").into(),
            ));
        }
        self.binding_value(package, name)
            .map_err(WorkerOperationError::with(WorkerErrorCode::BindingForce))
    }

    fn dispatch_generics(
        &mut self,
        package: Option<&protocol::PackageSpec>,
        name: &str,
    ) -> std::result::Result<Vec<String>, WorkerOperationError> {
        let metadata = |error: InspectionError| {
            WorkerOperationError::with(WorkerErrorCode::PackageMetadata)(error)
        };
        let environment = match package {
            Some(package) => {
                let context = self.context(package).map_err(&metadata)?;
                field(&context.image, "image_env").map_err(&metadata)?
            }
            None => harp::RFunction::new("base", "baseenv")
                .call()
                .map_err(InspectionError::from)
                .map_err(&metadata)?,
        };
        if !harp::environment::Environment::new(environment.clone()).exists(name) {
            return Err(WorkerOperationError::with(WorkerErrorCode::MissingBinding)(
                format!("installed image has no binding {name}").into(),
            ));
        }
        harp::RFunction::new("", ".slinker_dispatch_generics")
            .add(environment)
            .add(name)
            .call()
            .and_then(Vec::<String>::try_from)
            .map_err(InspectionError::from)
            .map_err(WorkerOperationError::with(WorkerErrorCode::BindingForce))
    }

    fn binding_value(
        &mut self,
        package: &protocol::PackageSpec,
        name: &str,
    ) -> std::result::Result<protocol::WorkerBinding, InspectionError> {
        let context = self.context(package)?;
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
        context.private_ids.clone_from(&scanner.private_ids);
        if binding.name != name
            || context.index.name != package.name
            || context.index.version != package.version
        {
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

    fn serialize_payloads(
        &mut self,
        namespaces: &[protocol::NamespaceImageSpec],
        payloads: &[protocol::PayloadSpec],
    ) -> std::result::Result<protocol::PayloadSerialization, WorkerOperationError> {
        let mut images = harp::RFunction::new("base", "list");
        let mut package_names = Vec::with_capacity(namespaces.len());
        let mut registered_names = Vec::with_capacity(namespaces.len());
        for namespace in namespaces {
            images.add(self.image_environment(&namespace.package)?);
            package_names.push(namespace.package.name.clone());
            registered_names.push(namespace.registered_name.clone());
        }
        let mut replaced = Vec::new();
        for payload in payloads {
            let image = self.image_environment(&payload.package)?;
            for patch in &payload.patches {
                replaced.push(patch_closure(&image, patch).map_err(|error| {
                    WorkerOperationError::with(WorkerErrorCode::BindingForce)(
                        format!(
                            "failed to rewrite payload closure {}::{}: {error}",
                            payload.package.name, patch.binding
                        )
                        .into(),
                    )
                })?);
            }
        }
        if !replaced.is_empty() {
            let mut roots = Vec::new();
            for payload in payloads {
                let image = self.image_environment(&payload.package)?;
                for name in &payload.names {
                    roots.push(
                        harp::RFunction::new("base", "get")
                            .add(name.clone())
                            .param("envir", image.clone())
                            .param("inherits", false)
                            .call()
                            .map_err(|error| {
                                WorkerOperationError::with(WorkerErrorCode::BindingForce)(
                                    InspectionError::from(error),
                                )
                            })?,
                    );
                }
            }
            let replaced = replaced
                .iter()
                .map(|closure| closure.sexp)
                .collect::<HashSet<_>>();
            if ReferenceWalk::default().reaches(&roots, &replaced) {
                return Err(WorkerOperationError::with(WorkerErrorCode::BindingForce)(
                    "a rewritten payload closure is still referenced from another payload location"
                        .to_owned()
                        .into(),
                ));
            }
        }
        let mut sources = harp::RFunction::new("base", "list");
        let mut names = harp::RFunction::new("base", "list");
        for payload in payloads {
            if !namespaces
                .iter()
                .any(|namespace| namespace.package.root == payload.package.root)
            {
                return Err(WorkerOperationError::with(WorkerErrorCode::Protocol)(
                    format!(
                        "payload of {} is not a materialized namespace image",
                        payload.package.name
                    )
                    .into(),
                ));
            }
            sources.add(self.image_environment(&payload.package)?);
            names.add(payload.names.clone());
        }
        let failed = |error: harp::Error| {
            WorkerOperationError::with(WorkerErrorCode::BindingForce)(
                format!("failed to serialize payload bundles: {error}").into(),
            )
        };
        let bundles = harp::RFunction::new("", ".slinker_payloads")
            .add(images.call().map_err(failed)?)
            .add(package_names)
            .add(registered_names)
            .add(sources.call().map_err(failed)?)
            .add(names.call().map_err(failed)?)
            .call()
            .and_then(Vec::<harp::object::RObject>::try_from)
            .map_err(failed)?;
        let mut owners = HashMap::<libr::SEXP, usize>::new();
        for (index, bundle) in bundles.iter().enumerate() {
            let references = bundle
                .elt("references")
                .and_then(Vec::<harp::object::RObject>::try_from)
                .map_err(failed)?;
            for reference in references {
                let owner = *owners.entry(reference.sexp).or_insert(index);
                if owner != index {
                    return Ok(protocol::PayloadSerialization::SharedIdentity {
                        first: self.payload_site(&payloads[owner], owner, reference.sexp)?,
                        second: self.payload_site(&payloads[index], index, reference.sexp)?,
                    });
                }
            }
        }
        let bundles = bundles
            .iter()
            .map(|bundle| {
                Ok(protocol::SerializedPayload {
                    bytes: Vec::<u8>::try_from(&bundle.elt("bytes")?)?,
                    namespaces: Vec::<String>::try_from(&bundle.elt("namespaces")?)?,
                })
            })
            .collect::<harp::Result<_>>()
            .map_err(failed)?;
        Ok(protocol::PayloadSerialization::Serialized { bundles })
    }

    fn payload_site(
        &mut self,
        payload: &protocol::PayloadSpec,
        index: usize,
        reference: libr::SEXP,
    ) -> std::result::Result<protocol::PayloadSite, WorkerOperationError> {
        let image = self.image_environment(&payload.package)?;
        let failed = |error: harp::Error| {
            WorkerOperationError::with(WorkerErrorCode::BindingForce)(InspectionError::from(error))
        };
        for name in &payload.names {
            let value = harp::RFunction::new("base", "get")
                .add(name.clone())
                .param("envir", image.clone())
                .param("inherits", false)
                .call()
                .map_err(failed)?;
            let reached = harp::RFunction::new("", ".slinker_serialize")
                .add(value)
                .call()
                .and_then(|serialized| serialized.elt("references"))
                .and_then(Vec::<harp::object::RObject>::try_from)
                .map_err(failed)?
                .iter()
                .any(|candidate| candidate.sexp == reference);
            if reached {
                return Ok(protocol::PayloadSite {
                    payload: index,
                    binding: name.clone(),
                });
            }
        }
        Err(WorkerOperationError::with(WorkerErrorCode::Protocol)(
            format!(
                "no payload binding of {} reaches its shared reference object",
                payload.package.name
            )
            .into(),
        ))
    }

    fn image_environment(
        &mut self,
        package: &protocol::PackageSpec,
    ) -> std::result::Result<harp::object::RObject, WorkerOperationError> {
        let context = self
            .context(package)
            .map_err(WorkerOperationError::with(WorkerErrorCode::PackageMetadata))?;
        field(&context.image, "image_env")
            .map_err(WorkerOperationError::with(WorkerErrorCode::PackageMetadata))
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
        export_names.clone_from(&export_bindings);
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
                    package: PackageName::from(String::try_from(item)?),
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
                    package: package.into(),
                    except: strings_field(&item, "except")?
                        .into_iter()
                        .map(BindingName::from)
                        .collect(),
                });
            }
            let remote_object = values
                .get(1)
                .ok_or_else(|| "installed importFrom has no bindings".to_owned())?;
            let remote = Vec::<String>::try_from(remote_object)?;
            let mut local = names(remote_object.sexp);
            if local.len() != remote.len() {
                local.clone_from(&remote);
            } else {
                for (local, remote) in local.iter_mut().zip(&remote) {
                    if local.is_empty() {
                        local.clone_from(remote);
                    }
                }
            }
            Ok(ImportSpec::From {
                package: package.into(),
                bindings: local
                    .into_iter()
                    .zip(remote)
                    .map(|(local, remote)| ImportBinding {
                        local: local.into(),
                        remote: remote.into(),
                    })
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
                package: package.map(PackageName::from),
                name: generic.into(),
            },
            class: class.into(),
            method: method.into(),
        });
    }

    let native_routines = field(&namespace, "nativeRoutines")?;
    let root = string_field(context, "root")?;
    let installed_dynlibs = field(&namespace, "dynlibs")?;
    let aliases = names(installed_dynlibs.sexp);
    let dynlibs = Vec::<String>::try_from(&installed_dynlibs)?
        .into_iter()
        .enumerate()
        .map(|(position, name)| {
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
                alias: aliases.get(position).cloned().unwrap_or_default(),
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
                library: native_library(&compiled)?,
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

fn native_library(
    compiled: &harp::object::RObject,
) -> std::result::Result<NativeLibrary, InspectionError> {
    let Some(library) = strings_field(compiled, "library")?.into_iter().next() else {
        return Ok(NativeLibrary::Missing);
    };
    if names(compiled.sexp).iter().any(|name| name == "error") {
        return Ok(NativeLibrary::Unloadable {
            library,
            error: string_field(compiled, "error")?,
        });
    }
    let routines = field(compiled, "routines")?;
    Ok(NativeLibrary::Loaded {
        library,
        routines: NativeRoutines {
            c: strings_field(&routines, "c")?,
            call: strings_field(&routines, "call")?,
            fortran: strings_field(&routines, "fortran")?,
            external: strings_field(&routines, "external")?,
        },
        name_lookup: if bool::try_from(field(compiled, "force_symbols")?)? {
            NameLookup::Forced
        } else {
            NameLookup::Allowed
        },
    })
}

fn patch_closure(
    image: &harp::object::RObject,
    patch: &protocol::ClosurePatchSpec,
) -> std::result::Result<harp::object::RObject, InspectionError> {
    let (kinds, names): (Vec<String>, Vec<String>) = patch
        .steps
        .iter()
        .map(|step| match step {
            protocol::ObjectStepSpec::Environment => ("environment".to_owned(), String::new()),
            protocol::ObjectStepSpec::Parent => ("parent".to_owned(), String::new()),
            protocol::ObjectStepSpec::Binding(name) => ("binding".to_owned(), name.clone()),
        })
        .unzip();
    let home = harp::RFunction::new("", ".slinker_closure_home")
        .add(image.clone())
        .add(patch.root.iter().cloned().collect::<Vec<_>>())
        .add(kinds)
        .add(names)
        .call()?;
    let closure = harp::RFunction::new("", ".slinker_closure_at")
        .add(home.clone())
        .add(patch.binding.clone())
        .call()?;
    let deparsed = harp::RFunction::new("", ".slinker_deparse_binding")
        .add(patch.binding.clone())
        .add(closure.clone())
        .call()?;
    let normalized = String::try_from(
        harp::RFunction::new("", ".slinker_normalize_source")
            .add(deparsed)
            .call()?,
    )?;
    if crate::package::Digest::of(&normalized).0 != patch.expected_shape {
        return Err("the installed closure differs from the analyzed one"
            .to_owned()
            .into());
    }
    harp::RFunction::new("", ".slinker_patch_closure")
        .add(home)
        .add(patch.binding.clone())
        .add(closure.clone())
        .add(patch.source.clone())
        .call()?;
    Ok(closure)
}

#[derive(Default)]
struct ReferenceWalk {
    visited: HashSet<libr::SEXP>,
}

impl ReferenceWalk {
    fn reaches(&mut self, roots: &[harp::object::RObject], targets: &HashSet<libr::SEXP>) -> bool {
        let mut stack = roots.iter().map(|root| root.sexp).collect::<Vec<_>>();
        while let Some(value) = stack.pop() {
            if targets.contains(&value) {
                return true;
            }
            if !self.visited.insert(value) {
                continue;
            }
            harp::r::attrib_for_each(value, |_, attribute| stack.push(attribute));
            match harp::utils::r_typeof(value) {
                libr::CLOSXP => stack.extend([
                    harp::r::fn_formals(value),
                    harp::r::fn_body(value),
                    harp::r::fn_env(value),
                ]),
                libr::ENVSXP => {
                    let special = [
                        unsafe { libr::R_GlobalEnv },
                        unsafe { libr::R_BaseEnv },
                        unsafe { libr::R_EmptyEnv },
                        unsafe { libr::R_BaseNamespace },
                    ];
                    if special.contains(&value)
                        || harp::utils::r_env_is_ns_env(value)
                        || harp::utils::r_env_is_pkg_env(value)
                    {
                        continue;
                    }
                    stack.push(harp::r::env_parent(value));
                    for binding in harp::environment::Environment::view(value).iter().flatten() {
                        match binding.value {
                            harp::environment_iter::BindingValue::Standard { object }
                            | harp::environment_iter::BindingValue::Altrep { object, .. } => {
                                stack.push(object.sexp);
                            }
                            harp::environment_iter::BindingValue::Promise { promise } => {
                                if harp::utils::r_promise_is_forced(promise.sexp) {
                                    stack.push(harp::utils::r_promise_value(promise.sexp));
                                }
                            }
                            harp::environment_iter::BindingValue::Active { .. } => {}
                        }
                    }
                }
                libr::VECSXP | libr::EXPRSXP => {
                    for index in 0..harp::object::r_length(value) {
                        stack.push(harp::object::list_get(value, index));
                    }
                }
                libr::LISTSXP | libr::LANGSXP => {
                    let mut node = value;
                    while node != unsafe { libr::R_NilValue } {
                        stack.push(unsafe { libr::CAR(node) });
                        node = unsafe { libr::CDR(node) };
                    }
                }
                _ => {}
            }
        }
        false
    }
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
                Some(runtime) => match runtime.canonical_syntax(&source) {
                    Ok((source, stable)) => WorkerResponse::NormalizedSyntax {
                        request_id,
                        source,
                        stable,
                    },
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
            WorkerRequest::VerifyRelocation {
                request_id,
                original,
                rewritten,
                sites,
            } => match runtime.as_ref() {
                Some(runtime) => match runtime.verify_relocation(&original, &rewritten, &sites) {
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
            WorkerRequest::DispatchGenerics {
                request_id,
                package,
                name,
            } => match runtime.as_mut() {
                Some(runtime) => match runtime.dispatch_generics(package.as_ref(), &name) {
                    Ok(generics) => WorkerResponse::DispatchGenerics {
                        request_id,
                        generics,
                    },
                    Err(error) => {
                        operation_failure(Some(request_id), error, package.as_ref(), Some(name))
                    }
                },
                None => worker_failure(
                    Some(request_id),
                    WorkerErrorCode::RuntimeStartup,
                    "Harp worker must receive hello before semantic requests",
                    package.as_ref(),
                    Some(name),
                ),
            },
            WorkerRequest::SerializePayloads {
                request_id,
                namespaces,
                payloads,
            } => match runtime.as_mut() {
                Some(runtime) => match runtime.serialize_payloads(&namespaces, &payloads) {
                    Ok(serialization) => WorkerResponse::Payloads {
                        request_id,
                        serialization,
                    },
                    Err(error) => operation_failure(Some(request_id), error, None, None),
                },
                None => worker_failure(
                    Some(request_id),
                    WorkerErrorCode::RuntimeStartup,
                    "Harp worker must receive hello before semantic requests",
                    None,
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
        #[cfg(all(unix, not(target_os = "macos")))]
        if std::env::var_os("SLINKER_EMBEDDED_R_TEST").is_none() {
            let output = std::process::Command::new(
                std::env::current_exe().expect("current test executable"),
            )
            .args([
                "--exact",
                "r_worker::tests::harp_inspection_preserves_lazy_active_altrep_and_private_state",
                "--nocapture",
                "--test-threads=1",
            ])
            .env("SLINKER_EMBEDDED_R_TEST", "1")
            .env(
                "LD_LIBRARY_PATH",
                super::client::target_library_path(&r_home).expect("target R library path"),
            )
            .output()
            .expect("run the test under the target R library path");
            let stdout = String::from_utf8_lossy(&output.stdout);
            assert!(
                output.status.success() && stdout.contains("test result: ok. 1 passed"),
                "embedded target R test failed:\n{stdout}\n{}",
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
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
        payload_identity_stays_within_one_bundle(&mut runtime, &package, &fixture_temp);
        drop(runtime);
        std::fs::remove_dir_all(fixture_temp).expect("remove installed fixture");
    }

    fn payload_identity_stays_within_one_bundle(
        runtime: &mut WorkerRuntime,
        package: &PackageSpec,
        scratch: &std::path::Path,
    ) {
        let image = runtime.image_environment(package).expect("fixture image");
        harp::parse_eval_global(
            r#"
            .slinker_test_payloads <- function(image) {
              shared <- new.env(parent = emptyenv())
              cycle <- new.env(parent = emptyenv())
              cycle$self <- cycle
              parent <- new.env(parent = emptyenv())
              parent$tag <- "parent"
              counter <- local({
                count <- 0L
                function() {
                  count <<- count + 1L
                  count
                }
              })
              image$first <- list(state = shared, cycle = cycle, child = new.env(parent = parent))
              image$second <- structure(list(counter = counter), home = shared, class = "tagged")
              image$third <- counter
              image$external <- tools::file_ext
              stopifnot(
                identical(image$first$state, attr(image$second, "home")),
                !identical(
                  unserialize(serialize(image$first, NULL))$state,
                  attr(unserialize(serialize(image$second, NULL)), "home")
                )
              )
            }
            "#,
        )
        .expect("define payload fixture");
        harp::RFunction::new("", ".slinker_test_payloads")
            .add(image)
            .call()
            .expect("original identity is shared and separate serializations split it");
        let namespaces = [NamespaceImageSpec {
            package: package.clone(),
            registered_name: "root:harpfixture".into(),
        }];
        let payload = |names: &[&str]| PayloadSpec {
            package: package.clone(),
            names: names.iter().map(|name| (*name).to_owned()).collect(),
            patches: Vec::new(),
        };

        let split = runtime
            .serialize_payloads(
                &namespaces,
                &[payload(&["first"]), payload(&["second", "third"])],
            )
            .expect("serialize split bundles");
        let PayloadSerialization::SharedIdentity { first, second } = split else {
            panic!("an environment shared by two bundles was serialized twice");
        };
        assert_eq!((first.payload, first.binding.as_str()), (0, "first"));
        assert_eq!((second.payload, second.binding.as_str()), (1, "second"));

        let independent = runtime
            .serialize_payloads(
                &namespaces,
                &[payload(&["first"]), payload(&["external", "good"])],
            )
            .expect("serialize independent bundles");
        let PayloadSerialization::Serialized { bundles } = independent else {
            panic!("independent bundles were reported as sharing identity");
        };
        assert!(bundles[0].namespaces.is_empty());
        let mut observed = bundles[1].namespaces.clone();
        observed.sort();
        assert_eq!(observed, ["root:harpfixture", "tools"]);

        let joint = runtime
            .serialize_payloads(&namespaces, &[payload(&["first", "second", "third"])])
            .expect("serialize one bundle");
        let PayloadSerialization::Serialized { bundles } = joint else {
            panic!("bindings of one bundle were reported as sharing identity across bundles");
        };
        let restored = scratch.join("joint-bundle");
        std::fs::write(&restored, &bundles[0].bytes).expect("write serialized bundle");
        harp::parse_eval_global(&format!(
            r#"
            local({{
              path <- "{}"
              restored <- unserialize(readBin(path, "raw", file.size(path)))
              stopifnot(
                identical(restored$first$state, attr(restored$second, "home")),
                identical(restored$first$cycle$self, restored$first$cycle),
                identical(get("tag", envir = restored$first$child), "parent"),
                identical(environment(restored$second$counter), environment(restored$third)),
                identical(restored$third(), 1L),
                identical(restored$second$counter(), 2L),
                inherits(restored$second, "tagged")
              )
            }})
            "#,
            restored.to_string_lossy().replace('\\', "/")
        ))
        .expect("one bundle preserves sharing, cycles, parents, enclosures, and attributes");
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
