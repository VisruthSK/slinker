use super::InspectionError;
use super::InspectionResult;
use harp::RFunctionExt;
use slinker_core::package::{
    BindingImage, BindingName, BindingOrigin, BindingRepresentation, ClassName, ClosureSource,
    EmbeddedClosureSource, EmbeddedEnvironmentRef, ObjectIssue, ObjectKind, PrivateBindingImage,
    PrivateEnvironmentImage,
};
use std::collections::{HashMap, HashSet};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct InspectionEpoch {
    pub(super) worker: u64,
    pub(super) context: usize,
}

impl std::fmt::Display for InspectionEpoch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}", self.worker, self.context)
    }
}

pub(super) struct ObjectScanner {
    image_environment: libr::SEXP,
    package: String,
    pub(super) private_ids: HashMap<libr::SEXP, String>,
    epoch: InspectionEpoch,
    visiting: HashSet<libr::SEXP>,
    walking: HashSet<libr::SEXP>,
    pub(super) private_environments: HashMap<String, PrivateEnvironmentImage>,
}

impl ObjectScanner {
    pub(super) fn new(
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

    pub(super) fn top_binding(
        &mut self,
        name: &str,
        origin: BindingOrigin,
        value: harp::environment_iter::BindingValue,
    ) -> InspectionResult<BindingImage> {
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
    ) -> InspectionResult<PrivateBindingImage> {
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
    ) -> InspectionResult<ObjectFacts> {
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

    fn deparse(&self, binding: Option<&str>, value: libr::SEXP) -> InspectionResult<String> {
        harp::RFunction::new("", ".slinker_deparse_binding")
            .add(binding.unwrap_or(".slinker_embedded"))
            .add(value)
            .call()
            .and_then(String::try_from)
            .map_err(InspectionError::from)
    }

    fn environment_ref(&mut self, environment: libr::SEXP) -> InspectionResult<String> {
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

    fn inventory_private(&mut self, environment: libr::SEXP, id: &str) -> InspectionResult<()> {
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
            .collect::<InspectionResult<HashMap<_, _>>>()?;
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
    pub(super) fn new(kind: ObjectKind) -> Self {
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

pub(super) fn names(value: libr::SEXP) -> Vec<String> {
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
