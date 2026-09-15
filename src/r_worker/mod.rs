//! Hidden target-R worker boundary.
//!
//! The process protocol contains only owned Rust/serde data; Harp objects and
//! raw SEXPs never cross the process boundary.

pub(crate) mod client;
pub mod protocol;

use crate::package::{
    BindingImage, BindingOrigin, BindingRepresentation, ClosureSource, EmbeddedClosureSource,
    EmbeddedEnvironmentRef, ExportMap, ImportBinding, ImportSpec, NativeComponent,
    NativeRegistration, NativeSafety, NativeSymbolBinding, ObjectIssue, ObjectKind,
    PrivateBindingImage, PrivateEnvironmentImage, S3Registration,
};
use crate::{Error, Result};
use harp::{RFunctionExt, RObjectExt};
use protocol::{
    PROTOCOL_VERSION, WorkerErrorCode, WorkerPackageIndex, WorkerRequest, WorkerResponse,
};
use std::collections::{HashMap, HashSet};
use std::ffi::CString;
use std::fs::OpenOptions;
use std::io::{self, BufRead, Write};

struct PackageImageContext {
    image: harp::object::RObject,
    private_ids: HashMap<libr::SEXP, String>,
}

struct WorkerRuntime {
    _arguments: Vec<CString>,
    _contexts: HashMap<String, PackageImageContext>,
}

impl WorkerRuntime {
    fn start(target: &protocol::TargetSpec) -> std::result::Result<Self, String> {
        if !target.r_home.is_dir() {
            return Err(format!(
                "selected R home does not exist: {}",
                target.r_home.display()
            ));
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
        // Safety: the worker owns the only R runtime in this process, performs
        // initialization on its protocol thread, and keeps argv alive for the
        // process lifetime. Harp initializes all dynamic libr bindings before
        // any R API call and constant globals only after main-loop setup.
        unsafe {
            harp::CONSOLE_THREAD_ID = Some(std::thread::current().id());
            libr::set(libr::R_SignalHandlers, 0);
            libr::Rf_initialize_R(pointers.len() as i32, pointers.as_mut_ptr());
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
            ));
        }
        Ok(Self {
            _arguments: arguments,
            _contexts: HashMap::new(),
        })
    }

    fn validate_syntax(&self, source: &str) -> std::result::Result<(), String> {
        harp::parse_exprs(source)
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    fn target(&self) -> std::result::Result<protocol::WorkerTarget, String> {
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
    ) -> std::result::Result<WorkerPackageIndex, String> {
        let key = package.root.to_string_lossy().into_owned();
        if !self._contexts.contains_key(&key) {
            if !package.root.is_dir() {
                return Err(format!(
                    "installed package directory does not exist: {}",
                    package.root.display()
                ));
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
        let metadata = harp::RFunction::new("", ".slinker_package_metadata")
            .add(context)
            .call()
            .map_err(|error| format!("failed to inspect {}: {error}", package.name))?;
        let mut index = worker_package_index(&metadata)?;
        index.image_fingerprint = package.image_fingerprint.clone();
        Ok(index)
    }

    fn binding(
        &mut self,
        package: &protocol::PackageSpec,
        name: &str,
    ) -> std::result::Result<protocol::WorkerBinding, String> {
        let index = self.package_index(package)?;
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
        let origin =
            if Vec::<String>::try_from(&context.image.elt("sysdata_names").map_err(r_error)?)
                .map_err(r_error)?
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
        );
        let binding = scanner.top_binding(name, origin, binding.value)?;
        context.private_ids = scanner.private_ids.clone();
        if binding.name != name || index.name != package.name || index.version != package.version {
            return Err(format!(
                "installed binding identity changed while inspecting {}::{name}",
                package.name
            ));
        }
        Ok(protocol::WorkerBinding {
            package_name: package.name.clone(),
            package_version: package.version.clone(),
            image_fingerprint: package.image_fingerprint.clone(),
            binding,
            private_environments: scanner.private_environments,
        })
    }
}

fn r_error(error: impl std::fmt::Display) -> String {
    error.to_string()
}

fn field(
    object: &harp::object::RObject,
    name: &str,
) -> std::result::Result<harp::object::RObject, String> {
    object.elt(name).map_err(r_error)
}

fn string_field(object: &harp::object::RObject, name: &str) -> std::result::Result<String, String> {
    String::try_from(field(object, name)?).map_err(r_error)
}

fn strings_field(
    object: &harp::object::RObject,
    name: &str,
) -> std::result::Result<Vec<String>, String> {
    Vec::<String>::try_from(field(object, name)?).map_err(r_error)
}

fn list_field(
    object: &harp::object::RObject,
    name: &str,
) -> std::result::Result<Vec<harp::object::RObject>, String> {
    Vec::<harp::object::RObject>::try_from(field(object, name)?).map_err(r_error)
}

fn bool_field(object: &harp::object::RObject, name: &str) -> std::result::Result<bool, String> {
    bool::try_from(field(object, name)?).map_err(r_error)
}

fn worker_package_index(
    metadata: &harp::object::RObject,
) -> std::result::Result<WorkerPackageIndex, String> {
    let export_names = strings_field(metadata, "export_names")?;
    let export_bindings = strings_field(metadata, "export_bindings")?;
    if export_names.len() != export_bindings.len() {
        return Err("installed export names and bindings have different lengths".into());
    }
    let exports = export_names
        .into_iter()
        .zip(export_bindings)
        .collect::<ExportMap>();
    let imports = list_field(metadata, "imports")?
        .into_iter()
        .map(|item| {
            let package = string_field(&item, "package")?;
            match string_field(&item, "kind")?.as_str() {
                "all" => Ok(ImportSpec::All {
                    package,
                    except: strings_field(&item, "except")?,
                }),
                "from" => {
                    let remote = strings_field(&item, "remote")?;
                    let local = strings_field(&item, "local")?;
                    if remote.len() != local.len() {
                        return Err("installed import names have different lengths".into());
                    }
                    Ok(ImportSpec::From {
                        package,
                        bindings: local
                            .into_iter()
                            .zip(remote)
                            .map(|(local, remote)| ImportBinding { local, remote })
                            .collect(),
                    })
                }
                kind => Err(format!("unsupported installed import kind {kind:?}")),
            }
        })
        .collect::<std::result::Result<Vec<_>, String>>()?;
    let s3 = list_field(metadata, "s3")?
        .into_iter()
        .map(|item| {
            Ok(S3Registration {
                generic: string_field(&item, "generic")?,
                generic_package: Vec::<String>::try_from(field(&item, "generic_package")?)
                    .map_err(r_error)?
                    .into_iter()
                    .next(),
                class: string_field(&item, "class")?,
                method: string_field(&item, "method")?,
            })
        })
        .collect::<std::result::Result<Vec<_>, String>>()?;
    let dynlibs = list_field(metadata, "dynlibs")?
        .into_iter()
        .map(|item| {
            let bindings = strings_field(&item, "bindings")?;
            let symbols = strings_field(&item, "symbols")?;
            if bindings.len() != symbols.len() {
                return Err("installed native binding names have different lengths".into());
            }
            Ok(NativeComponent {
                name: string_field(&item, "name")?,
                registration: bool_field(&item, "registered")?.then(|| NativeRegistration {
                    prefix: string_field(&item, "prefix").expect("metadata prefix"),
                    suffix: string_field(&item, "suffix").expect("metadata suffix"),
                }),
                symbols: bindings
                    .into_iter()
                    .zip(symbols)
                    .map(|(binding, symbol)| NativeSymbolBinding { binding, symbol })
                    .collect(),
                safety: NativeSafety::Unanalyzed,
            })
        })
        .collect::<std::result::Result<Vec<_>, String>>()?;
    Ok(WorkerPackageIndex {
        name: string_field(metadata, "name")?,
        version: string_field(metadata, "version")?,
        image_fingerprint: String::new(),
        exports,
        imports,
        s3,
        dynlibs,
        on_load: bool_field(metadata, "on_load")?,
        binding_names: strings_field(metadata, "binding_names")?,
        datasets: strings_field(metadata, "datasets")?,
        has_sysdata: bool_field(metadata, "has_sysdata")?,
    })
}

struct ObjectScanner {
    image_environment: libr::SEXP,
    package: String,
    private_ids: HashMap<libr::SEXP, String>,
    visiting: HashSet<libr::SEXP>,
    walking: HashSet<libr::SEXP>,
    private_environments: HashMap<String, PrivateEnvironmentImage>,
}

impl ObjectScanner {
    fn new(
        image_environment: libr::SEXP,
        package: String,
        private_ids: HashMap<libr::SEXP, String>,
    ) -> Self {
        Self {
            image_environment,
            package,
            private_ids,
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
    ) -> std::result::Result<BindingImage, String> {
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
                    issues: vec![ObjectIssue {
                        path: "$".into(),
                        kind: "altrep".into(),
                        detail: class,
                    }],
                });
            }
            harp::environment_iter::BindingValue::Standard { object } => {
                (BindingRepresentation::Value, object)
            }
        };
        let mut facts = self.scan_value(object.sexp, "$", false, 0)?;
        if let Some(closure) = &mut facts.closure {
            closure.source = self.deparse(name, object.sexp, false)?.into();
        }
        Ok(BindingImage {
            name: name.into(),
            origin,
            representation,
            classes: classes(object.sexp),
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
    ) -> std::result::Result<PrivateBindingImage, String> {
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
                    issues: vec![ObjectIssue {
                        path: "$".into(),
                        kind: "altrep".into(),
                        detail: class,
                    }],
                });
            }
            harp::environment_iter::BindingValue::Standard { object } => {
                (BindingRepresentation::Value, object)
            }
        };
        let mut facts = self.scan_value(object.sexp, "$", false, 0)?;
        Ok(PrivateBindingImage {
            name: name.into(),
            representation,
            classes: classes(object.sexp),
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
        embedded: bool,
        depth: usize,
    ) -> std::result::Result<ObjectFacts, String> {
        if depth > 128 {
            return Ok(ObjectFacts::issue(
                ObjectKind::Other("depth".into()),
                path,
                "object_depth",
                "object graph exceeds 128 levels",
            ));
        }
        if harp::utils::r_is_altrep(value) {
            return Ok(ObjectFacts::issue(
                ObjectKind::Altrep,
                path,
                "altrep",
                &harp::utils::r_altrep_class(value),
            ));
        }
        let recursive = matches!(
            harp::utils::r_typeof(value),
            libr::CLOSXP | libr::ENVSXP | libr::VECSXP | libr::LISTSXP
        );
        if recursive && !self.walking.insert(value) {
            return Ok(ObjectFacts::new(object_kind(value)));
        }
        let mut facts = ObjectFacts::new(object_kind(value));
        match harp::utils::r_typeof(value) {
            libr::CLOSXP => {
                let closure_environment = harp::RFunction::new("base", "environment")
                    .add(value)
                    .call()
                    .map_err(r_error)?;
                let environment = self.environment_ref(closure_environment.sexp)?;
                if embedded {
                    facts.closures.push(EmbeddedClosureSource {
                        path: path.into(),
                        environment: environment.clone(),
                        source: self.deparse(".slinker_embedded", value, true)?.into(),
                    });
                } else {
                    facts.closure = Some(ClosureSource {
                        environment: environment.clone(),
                        source: self.deparse("", value, false)?.into(),
                    });
                }
                facts.environment = Some(environment);
            }
            libr::ENVSXP => {
                let environment = self.environment_ref(value)?;
                if embedded && !environment.starts_with("unsupported:") {
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
                    let member = names
                        .get(index as usize)
                        .filter(|name| !name.is_empty())
                        .map_or_else(|| format!("[[{}]]", index + 1), |name| format!("${name}"));
                    facts.merge(self.scan_value(
                        harp::object::list_get(value, index),
                        &format!("{path}{member}"),
                        true,
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
                            true,
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
                    true,
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
        name: &str,
        value: libr::SEXP,
        embedded: bool,
    ) -> std::result::Result<String, String> {
        harp::RFunction::new("", ".slinker_deparse_binding")
            .add(name)
            .add(value)
            .add(embedded)
            .call()
            .and_then(String::try_from)
            .map_err(r_error)
    }

    fn environment_ref(&mut self, environment: libr::SEXP) -> std::result::Result<String, String> {
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
                .map_err(r_error);
        }
        if harp::utils::r_env_is_pkg_env(environment) {
            return harp::utils::r_envir_name(environment)
                .map(|name| format!("unsupported:{name}"))
                .map_err(r_error);
        }
        let pointer = environment;
        if let Some(id) = self.private_ids.get(&pointer) {
            return Ok(id.clone());
        }
        let id = format!("private:{}", self.private_ids.len() + 1);
        self.private_ids.insert(pointer, id.clone());
        self.inventory_private(environment, &id)?;
        Ok(id)
    }

    fn inventory_private(
        &mut self,
        environment: libr::SEXP,
        id: &str,
    ) -> std::result::Result<(), String> {
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
                let binding = binding.map_err(r_error)?;
                let name = String::from(binding.name);
                Ok((name.clone(), self.private_binding(&name, binding.value)?))
            })
            .collect::<std::result::Result<HashMap<_, _>, String>>()?;
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

fn configure_libraries(libraries: &[std::path::PathBuf]) -> std::result::Result<(), String> {
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
        .map_err(|error| format!("failed to select R library universe: {error}"))
}

fn install_helpers() -> std::result::Result<(), String> {
    harp::parse_eval_global(include_str!("helpers.R"))
        .map(|_| ())
        .map_err(|error| format!("failed to initialize installed-image helpers: {error}"))
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
                    &WorkerResponse::Error {
                        request_id: None,
                        code: WorkerErrorCode::Protocol,
                        message: format!("invalid worker request: {error}"),
                    },
                )?;
                continue;
            }
        };

        let response = match request {
            WorkerRequest::Hello { protocol, target } => {
                if protocol != PROTOCOL_VERSION {
                    WorkerResponse::Error {
                        request_id: None,
                        code: WorkerErrorCode::Protocol,
                        message: format!(
                            "unsupported worker protocol {protocol}; expected {PROTOCOL_VERSION}"
                        ),
                    }
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
                            Err(message) => WorkerResponse::Error {
                                request_id: None,
                                code: WorkerErrorCode::Startup,
                                message,
                            },
                        },
                        Err(message) => WorkerResponse::Error {
                            request_id: None,
                            code: WorkerErrorCode::Startup,
                            message,
                        },
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
                    Err(message) => WorkerResponse::SyntaxValidation {
                        request_id,
                        accepted: false,
                        message: Some(message),
                    },
                },
                None => WorkerResponse::Error {
                    request_id: Some(request_id),
                    code: WorkerErrorCode::Startup,
                    message: "Harp worker must receive hello before semantic requests".into(),
                },
            },
            WorkerRequest::PackageIndex {
                request_id,
                package,
            } => match runtime.as_mut() {
                Some(runtime) => match runtime.package_index(&package) {
                    Ok(index) => WorkerResponse::PackageIndex { request_id, index },
                    Err(message) => WorkerResponse::Error {
                        request_id: Some(request_id),
                        code: WorkerErrorCode::PackageMetadata,
                        message,
                    },
                },
                None => WorkerResponse::Error {
                    request_id: Some(request_id),
                    code: WorkerErrorCode::Startup,
                    message: "Harp worker must receive hello before semantic requests".into(),
                },
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
                    Err(message) => WorkerResponse::Error {
                        request_id: Some(request_id),
                        code: WorkerErrorCode::BindingInspection,
                        message,
                    },
                },
                None => WorkerResponse::Error {
                    request_id: Some(request_id),
                    code: WorkerErrorCode::Startup,
                    message: "Harp worker must receive hello before semantic requests".into(),
                },
            },
        };

        write_response(&mut output, &response)?;
    }

    Ok(())
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
}
