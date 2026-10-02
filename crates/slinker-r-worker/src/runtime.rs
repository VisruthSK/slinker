use super::index::worker_package_index;
use super::scan::ObjectScanner;
use super::{Coded, InspectionError, WorkerOperationError, field, protocol};
use super::{InspectionResult, OperationResult};
use harp::{RFunctionExt, RObjectExt};
use protocol::{
    NormalizedSource, WorkerErrorCode, WorkerPackageIndex, WorkerRequest, WorkerResponse,
};
use slinker_core::package::{BindingName, BindingOrigin, DataSetId, DatasetName, EnvironmentLabel};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::ffi::CString;

struct PackageImageContext {
    image: harp::object::RObject,
    index: WorkerPackageIndex,
    delivered: HashSet<EnvironmentLabel>,
    labels: HashMap<libr::SEXP, EnvironmentLabel>,
}

pub(super) struct WorkerRuntime {
    _arguments: Vec<CString>,
    contexts: HashMap<String, PackageImageContext>,
}

impl WorkerRuntime {
    pub(super) fn start(target: &protocol::TargetSpec) -> InspectionResult<Self> {
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
            _arguments: arguments,
            contexts: HashMap::new(),
        })
    }

    pub(super) fn validate_syntax(&self, source: &str) -> InspectionResult<()> {
        harp::parse_exprs(source)
            .map(|_| ())
            .map_err(InspectionError::from)
    }

    fn normalize_syntax(&self, source: &str) -> InspectionResult<String> {
        harp::RFunction::new("", ".slinker_normalize_source")
            .add(source)
            .call()
            .and_then(String::try_from)
            .map_err(InspectionError::from)
    }

    pub(super) fn canonical_syntax(&self, source: &str) -> InspectionResult<(String, bool)> {
        let normalized = self.normalize_syntax(source)?;
        let stable = self.normalize_syntax(&normalized)? == normalized;
        Ok((normalized, stable))
    }

    pub(super) fn verify_relocation(
        &self,
        original: &str,
        rewritten: &str,
        sites: &[protocol::RelocationSiteSpec],
    ) -> InspectionResult<()> {
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

    pub(super) fn target(&self) -> InspectionResult<protocol::WorkerTarget> {
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
                .names()
                .into_iter()
                .map(BindingName::from)
                .collect(),
        })
    }

    fn context(
        &mut self,
        package: &protocol::PackageSpec,
    ) -> InspectionResult<&mut PackageImageContext> {
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
                .add(package.name.as_str())
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
                    delivered: HashSet::new(),
                    labels: HashMap::new(),
                },
            );
        }
        Ok(self
            .contexts
            .get_mut(&key)
            .expect("package image context inserted"))
    }

    pub(super) fn package_index(
        &mut self,
        package: &protocol::PackageSpec,
    ) -> InspectionResult<WorkerPackageIndex> {
        let mut index = self.context(package)?.index.clone();
        index
            .image_fingerprint
            .clone_from(&package.image_fingerprint);
        Ok(index)
    }

    pub(super) fn binding(
        &mut self,
        package: &protocol::PackageSpec,
        name: &str,
    ) -> OperationResult<protocol::WorkerBinding> {
        let context = self
            .context(package)
            .coded(WorkerErrorCode::PackageMetadata)?;
        let image_environment =
            field(&context.image, "image_env").coded(WorkerErrorCode::PackageMetadata)?;
        if !harp::environment::Environment::new(image_environment).exists(name) {
            return Err(WorkerOperationError::new(
                WorkerErrorCode::MissingBinding,
                format!("installed image has no binding {name}"),
            ));
        }
        self.binding_value(package, name)
            .coded(WorkerErrorCode::BindingForce)
    }

    pub(super) fn data_library(
        &mut self,
        package: &protocol::PackageSpec,
        objects: &[DatasetName],
        sets: &BTreeMap<DataSetId, Vec<DatasetName>>,
    ) -> OperationResult<protocol::DataLibraryFiles> {
        let root = self
            .context(package)
            .coded(WorkerErrorCode::PackageMetadata)?
            .image
            .elt("root")
            .coded(WorkerErrorCode::BindingForce)?;
        let set_lengths = sets
            .values()
            .map(|members| f64::from(u32::try_from(members.len()).unwrap_or(u32::MAX)))
            .collect::<Vec<_>>();
        let library = harp::RFunction::new("", ".slinker_data_library")
            .add(root)
            .add(strings(objects))
            .add(strings(sets.keys()))
            .add(&set_lengths)
            .add(strings(sets.values().flatten()))
            .call()
            .coded(WorkerErrorCode::BindingForce)?;
        let bytes = |name: &str| {
            library
                .elt(name)
                .and_then(|value| Vec::<u8>::try_from(&value))
                .coded(WorkerErrorCode::BindingForce)
        };
        Ok(protocol::DataLibraryFiles {
            rdb: bytes("rdb")?,
            rdx: bytes("rdx")?,
            rds: bytes("rds")?,
        })
    }

    pub(super) fn dispatch_generics(
        &mut self,
        package: Option<&protocol::PackageSpec>,
        name: &str,
    ) -> OperationResult<Vec<String>> {
        let environment = match package {
            Some(package) => {
                let context = self
                    .context(package)
                    .coded(WorkerErrorCode::PackageMetadata)?;
                field(&context.image, "image_env").coded(WorkerErrorCode::PackageMetadata)?
            }
            None => harp::RFunction::new("base", "baseenv")
                .call()
                .coded(WorkerErrorCode::PackageMetadata)?,
        };
        if !harp::environment::Environment::new(environment.clone()).exists(name) {
            return Err(WorkerOperationError::new(
                WorkerErrorCode::MissingBinding,
                format!("installed image has no binding {name}"),
            ));
        }
        harp::RFunction::new("", ".slinker_dispatch_generics")
            .add(environment)
            .add(name)
            .call()
            .and_then(Vec::<String>::try_from)
            .coded(WorkerErrorCode::BindingForce)
    }

    fn binding_value(
        &mut self,
        package: &protocol::PackageSpec,
        name: &str,
    ) -> InspectionResult<protocol::WorkerBinding> {
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
        let registries = context.image.elt("lazy_environments")?;
        let labels = &mut context.labels;
        let mut identify = |environment: libr::SEXP| -> InspectionResult<EnvironmentLabel> {
            if let Some(label) = labels.get(&environment) {
                return Ok(label.clone());
            }
            let key = harp::RFunction::new("", ".slinker_environment_key")
                .add(registries.sexp)
                .add(environment)
                .call()
                .and_then(String::try_from)?;
            if key.is_empty() {
                return Err("a private environment has no lazy-load identity"
                    .to_owned()
                    .into());
            }
            let label = EnvironmentLabel::private(key);
            labels.insert(environment, label.clone());
            Ok(label)
        };
        let mut scanner = ObjectScanner::new(
            environment.inner.sexp,
            package.name.as_str(),
            &mut identify,
            &context.delivered,
        );
        let binding = scanner.top_binding(name, origin, binding.value)?;
        let outcome = scanner.finish();
        context
            .delivered
            .extend(outcome.private_environments.keys().cloned());
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
            private_environments: outcome.private_environments,
        })
    }

    pub(super) fn image_environment(
        &mut self,
        package: &protocol::PackageSpec,
    ) -> OperationResult<harp::object::RObject> {
        let context = self
            .context(package)
            .coded(WorkerErrorCode::PackageMetadata)?;
        field(&context.image, "image_env").coded(WorkerErrorCode::PackageMetadata)
    }

    pub(super) fn answer(&mut self, request: WorkerRequest) -> OperationResult<WorkerResponse> {
        Ok(match request {
            WorkerRequest::ValidateSyntax { request_id, source } => {
                syntax_verdict(request_id, self.validate_syntax(&source))
            }
            WorkerRequest::NormalizeSyntax {
                request_id,
                sources,
            } => {
                let results = sources
                    .iter()
                    .map(|source| {
                        self.canonical_syntax(source)
                            .map(|(source, stable)| NormalizedSource { source, stable })
                    })
                    .collect::<InspectionResult<Vec<_>>>()
                    .coded(WorkerErrorCode::TargetSyntaxRejection)?;
                WorkerResponse::NormalizedSyntax {
                    request_id,
                    results,
                }
            }
            WorkerRequest::VerifyRelocation {
                request_id,
                original,
                rewritten,
                sites,
            } => syntax_verdict(
                request_id,
                self.verify_relocation(&original, &rewritten, &sites),
            ),
            WorkerRequest::PackageIndex {
                request_id,
                package,
            } => WorkerResponse::PackageIndex {
                request_id,
                index: self
                    .package_index(&package)
                    .coded(WorkerErrorCode::PackageMetadata)?,
            },
            WorkerRequest::Binding {
                request_id,
                package,
                name,
            } => WorkerResponse::Binding {
                request_id,
                binding: self.binding(&package, name.as_str())?,
            },
            WorkerRequest::BindingBatch {
                request_id,
                package,
                names,
            } => WorkerResponse::Bindings {
                request_id,
                bindings: names
                    .iter()
                    .map(|name| self.binding(&package, name.as_str()))
                    .collect::<OperationResult<Vec<_>>>()?,
            },
            WorkerRequest::DataLibrary {
                request_id,
                package,
                objects,
                sets,
            } => WorkerResponse::DataLibrary {
                request_id,
                library: self.data_library(&package, &objects, &sets)?,
            },
            WorkerRequest::DispatchGenerics {
                request_id,
                package,
                name,
            } => WorkerResponse::DispatchGenerics {
                request_id,
                generics: self.dispatch_generics(package.as_ref(), name.as_str())?,
            },
            WorkerRequest::SerializePayloads {
                request_id,
                namespaces,
                payloads,
            } => WorkerResponse::Payloads {
                request_id,
                serialization: self.serialize_payloads(&namespaces, &payloads)?,
            },
            WorkerRequest::Hello { .. } | WorkerRequest::Shutdown => {
                return Err(WorkerOperationError::new(
                    WorkerErrorCode::Protocol,
                    "lifecycle request received after worker startup",
                ));
            }
        })
    }
}

fn strings<T: ToString>(values: impl IntoIterator<Item = T>) -> Vec<String> {
    values.into_iter().map(|value| value.to_string()).collect()
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

fn configure_libraries(libraries: &[std::path::PathBuf]) -> InspectionResult<()> {
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

fn install_helpers() -> InspectionResult<()> {
    harp::parse_eval_global(include_str!("helpers.R"))
        .map(|_| ())
        .map_err(|error| format!("failed to initialize installed-image helpers: {error}").into())
}
fn syntax_verdict(request_id: u64, result: InspectionResult<()>) -> WorkerResponse {
    WorkerResponse::SyntaxValidation {
        request_id,
        accepted: result.is_ok(),
        message: result.err().map(|error| error.to_string()),
    }
}
