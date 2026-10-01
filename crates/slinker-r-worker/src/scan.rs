use super::sexp::{classes, names, symbol_name};
use super::{InspectionError, InspectionResult};
use harp::RFunctionExt;
use harp::environment_iter::BindingValue;
use harp::object::RObject;
use slinker_core::package::{
    BindingImage, BindingName, BindingOrigin, BindingRepresentation, ClassName, ClosureSource,
    EmbeddedClosureSource, EmbeddedEnvironmentRef, EnvironmentKind, EnvironmentLabel, MemberPath,
    ObjectImage, ObjectIssue, ObjectIssueKind, ObjectKind, PrivateBindingImage,
    PrivateEnvironmentImage, UnsupportedObject,
};
use std::collections::{HashMap, HashSet};

const MAX_DEPTH: usize = 128;

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

#[derive(Clone, Copy)]
enum PromisePolicy {
    Force,
    Preserve,
}

#[derive(Clone, Copy)]
enum Site<'a> {
    Binding(&'a str),
    Member,
}

pub(super) type PrivateIds = HashMap<libr::SEXP, EnvironmentLabel>;

pub(super) struct ObjectScanner<'a> {
    image_environment: libr::SEXP,
    package: &'a str,
    known: &'a PrivateIds,
    epoch: InspectionEpoch,
    discovered: PrivateIds,
    walking: HashSet<libr::SEXP>,
    private_environments: HashMap<EnvironmentLabel, PrivateEnvironmentImage>,
}

pub(super) struct ScanOutcome {
    pub(super) discovered: PrivateIds,
    pub(super) private_environments: HashMap<EnvironmentLabel, PrivateEnvironmentImage>,
}

impl<'a> ObjectScanner<'a> {
    pub(super) fn new(
        image_environment: libr::SEXP,
        package: &'a str,
        known: &'a PrivateIds,
        epoch: InspectionEpoch,
    ) -> Self {
        Self {
            image_environment,
            package,
            known,
            epoch,
            discovered: HashMap::new(),
            walking: HashSet::new(),
            private_environments: HashMap::new(),
        }
    }

    pub(super) fn finish(self) -> ScanOutcome {
        ScanOutcome {
            discovered: self.discovered,
            private_environments: self.private_environments,
        }
    }

    pub(super) fn top_binding(
        &mut self,
        name: &str,
        origin: BindingOrigin,
        value: BindingValue,
    ) -> InspectionResult<BindingImage> {
        Ok(BindingImage {
            name: name.into(),
            origin,
            object: self.scan_binding(name, value, PromisePolicy::Force)?,
        })
    }

    fn private_binding(
        &mut self,
        name: &str,
        value: BindingValue,
    ) -> InspectionResult<PrivateBindingImage> {
        Ok(PrivateBindingImage {
            name: name.into(),
            object: self.scan_binding(name, value, PromisePolicy::Preserve)?,
        })
    }

    fn scan_binding(
        &mut self,
        name: &str,
        value: BindingValue,
        policy: PromisePolicy,
    ) -> InspectionResult<ObjectImage> {
        let (representation, object) = match value {
            BindingValue::Active { .. } => {
                return Ok(ObjectImage::of_kind(
                    BindingRepresentation::ActiveBinding,
                    ObjectKind::ActiveBinding,
                ));
            }
            BindingValue::Altrep { object, .. } => {
                return Ok(altrep_image(&harp::utils::r_altrep_class(object.sexp)));
            }
            BindingValue::Promise { promise } => match policy {
                PromisePolicy::Force => (
                    BindingRepresentation::LazyLoadPromise,
                    harp::utils::r_promise_force_with_rollback(promise.sexp).map_err(|error| {
                        format!("failed to force demanded {}::{name}: {error}", self.package)
                    })?,
                ),
                PromisePolicy::Preserve if !harp::utils::r_promise_is_forced(promise.sexp) => {
                    return Ok(unforced_promise_image());
                }
                PromisePolicy::Preserve => (
                    BindingRepresentation::Promise { forced: true },
                    RObject::from(harp::utils::r_promise_value(promise.sexp)),
                ),
            },
            BindingValue::Standard { object } => (BindingRepresentation::Value, object),
        };
        let mut image =
            self.scan_value(object.sexp, &MemberPath::root(), Site::Binding(name), 0)?;
        image.representation = representation;
        image.classes = classes(object.sexp)
            .into_iter()
            .map(ClassName::from)
            .collect();
        Ok(image)
    }

    fn scan_value(
        &mut self,
        value: libr::SEXP,
        path: &MemberPath,
        site: Site<'_>,
        depth: usize,
    ) -> InspectionResult<ObjectImage> {
        if depth > MAX_DEPTH {
            return Ok(issue_image(
                ObjectKind::Unsupported(UnsupportedObject::DepthLimit),
                path,
                ObjectIssueKind::ObjectDepth,
                "object graph exceeds 128 levels",
            ));
        }
        if harp::utils::r_is_altrep(value) {
            let mut image = ObjectImage::of_kind(BindingRepresentation::Value, ObjectKind::Altrep);
            image.issues = altrep_issues(path, &harp::utils::r_altrep_class(value));
            return Ok(image);
        }
        let kind = harp::utils::r_typeof(value);
        let recursive = matches!(
            kind,
            libr::CLOSXP | libr::ENVSXP | libr::VECSXP | libr::LISTSXP
        );
        let mut image = ObjectImage::of_kind(BindingRepresentation::Value, object_kind(value));
        if recursive && !self.walking.insert(value) {
            if kind == libr::ENVSXP {
                self.scan_environment(&mut image, value, path, site, false)?;
            }
            return Ok(image);
        }
        match kind {
            libr::CLOSXP => self.scan_closure(&mut image, value, path, site)?,
            libr::ENVSXP => self.scan_environment(&mut image, value, path, site, true)?,
            libr::VECSXP => self.scan_members(&mut image, value, path, depth)?,
            libr::LISTSXP => {
                let members = harp::RFunction::new("base", "as.list")
                    .add(RObject::view(value))
                    .call()?;
                self.scan_members(&mut image, members.sexp, path, depth)?;
            }
            libr::EXTPTRSXP => image.issues.push(member_issue(
                path,
                ObjectIssueKind::ExternalPointer,
                "external pointer",
            )),
            libr::WEAKREFSXP => image.issues.push(member_issue(
                path,
                ObjectIssueKind::WeakReference,
                "weak reference",
            )),
            _ => {}
        }
        if !matches!(kind, libr::LANGSXP | libr::EXPRSXP) {
            let mut attributes = Vec::new();
            harp::r::attrib_for_each(value, |tag, attribute| attributes.push((tag, attribute)));
            for (tag, attribute) in attributes {
                let name = symbol_name(tag).unwrap_or_else(|| "?".into());
                let member = self.scan_value(
                    attribute,
                    &MemberPath::new(format!("{path}.attr[{name}]")),
                    Site::Member,
                    depth + 1,
                )?;
                image.absorb_members(member);
            }
        }
        if recursive {
            self.walking.remove(&value);
        }
        Ok(image)
    }

    fn scan_closure(
        &mut self,
        image: &mut ObjectImage,
        value: libr::SEXP,
        path: &MemberPath,
        site: Site<'_>,
    ) -> InspectionResult<()> {
        let environment = self.environment_ref(harp::r::fn_env(value))?;
        let source = self.deparse(site, value)?.into();
        match site {
            Site::Binding(_) => {
                image.closure = Some(ClosureSource {
                    environment: environment.clone(),
                    source,
                });
            }
            Site::Member => image.embedded_closures.push(EmbeddedClosureSource {
                path: path.clone(),
                environment: environment.clone(),
                source,
            }),
        }
        image.environment = Some(environment);
        Ok(())
    }

    fn scan_environment(
        &mut self,
        image: &mut ObjectImage,
        value: libr::SEXP,
        path: &MemberPath,
        site: Site<'_>,
        report_identity: bool,
    ) -> InspectionResult<()> {
        let environment = self.environment_ref(value)?;
        if matches!(site, Site::Member) && !environment.is_unsupported() {
            image.embedded_environments.push(EmbeddedEnvironmentRef {
                path: path.clone(),
                environment: environment.clone(),
            });
        }
        if report_identity && let EnvironmentKind::Unsupported(detail) = environment.kind() {
            image.issues.push(ObjectIssue {
                path: path.clone(),
                kind: ObjectIssueKind::EnvironmentIdentity,
                detail: detail.into(),
            });
        }
        image.environment = Some(environment);
        Ok(())
    }

    fn scan_members(
        &mut self,
        image: &mut ObjectImage,
        list: libr::SEXP,
        path: &MemberPath,
        depth: usize,
    ) -> InspectionResult<()> {
        let names = names(list);
        for index in 0..harp::object::r_length(list) {
            let position = usize::try_from(index).unwrap_or(usize::MAX);
            let member = match names.get(position).filter(|name| !name.is_empty()) {
                Some(name) => path.field(name),
                None => path.element(position + 1),
            };
            let scanned = self.scan_value(
                harp::object::list_get(list, index),
                &member,
                Site::Member,
                depth + 1,
            )?;
            image.absorb_members(scanned);
        }
        Ok(())
    }

    fn deparse(&self, site: Site<'_>, value: libr::SEXP) -> InspectionResult<String> {
        let name = match site {
            Site::Binding(name) => name,
            Site::Member => ".slinker_embedded",
        };
        harp::RFunction::new("", ".slinker_deparse_binding")
            .add(name)
            .add(value)
            .call()
            .and_then(String::try_from)
            .map_err(InspectionError::from)
    }

    fn environment_ref(&mut self, environment: libr::SEXP) -> InspectionResult<EnvironmentLabel> {
        if let Some(label) = self.distinguished_environment(environment)? {
            return Ok(label);
        }
        if let Some(label) = self
            .known
            .get(&environment)
            .or(self.discovered.get(&environment))
        {
            return Ok(label.clone());
        }
        let label =
            EnvironmentLabel::private(self.epoch, self.known.len() + self.discovered.len() + 1);
        self.discovered.insert(environment, label.clone());
        self.inventory_private(environment, &label)?;
        Ok(label)
    }

    fn distinguished_environment(
        &self,
        environment: libr::SEXP,
    ) -> InspectionResult<Option<EnvironmentLabel>> {
        let envs = &*harp::environment::R_ENVS;
        let label = if environment == self.image_environment {
            EnvironmentLabel::namespace(self.package)
        } else if environment == envs.base_ns {
            EnvironmentLabel::namespace("base")
        } else if environment == envs.base {
            EnvironmentLabel::base()
        } else if environment == envs.empty {
            EnvironmentLabel::empty()
        } else if environment == envs.global {
            EnvironmentLabel::unsupported("global")
        } else if harp::utils::r_env_is_ns_env(environment) {
            EnvironmentLabel::namespace(&harp::utils::r_envir_name(environment)?)
        } else if harp::utils::r_env_is_pkg_env(environment) {
            EnvironmentLabel::unsupported(&harp::utils::r_envir_name(environment)?)
        } else {
            return Ok(None);
        };
        Ok(Some(label))
    }

    fn inventory_private(
        &mut self,
        environment: libr::SEXP,
        label: &EnvironmentLabel,
    ) -> InspectionResult<()> {
        let parent = self.environment_ref(harp::r::env_parent(environment))?;
        let bindings = harp::environment::Environment::view(environment)
            .iter()
            .map(|binding| {
                let binding = binding?;
                let name = String::from(binding.name);
                let image = self.private_binding(&name, binding.value)?;
                Ok((BindingName::from(name), image))
            })
            .collect::<InspectionResult<HashMap<_, _>>>()?;
        self.private_environments.insert(
            label.clone(),
            PrivateEnvironmentImage {
                id: label.clone(),
                parent,
                bindings,
            },
        );
        Ok(())
    }
}

fn member_issue(path: &MemberPath, kind: ObjectIssueKind, detail: &str) -> ObjectIssue {
    ObjectIssue {
        path: path.clone(),
        kind,
        detail: detail.into(),
    }
}

fn issue_image(
    kind: ObjectKind,
    path: &MemberPath,
    issue: ObjectIssueKind,
    detail: &str,
) -> ObjectImage {
    let mut image = ObjectImage::of_kind(BindingRepresentation::Value, kind);
    image.issues.push(member_issue(path, issue, detail));
    image
}

fn altrep_issues(path: &MemberPath, class: &str) -> Vec<ObjectIssue> {
    if class.starts_with("base::") {
        return Vec::new();
    }
    vec![member_issue(path, ObjectIssueKind::Altrep, class)]
}

fn altrep_image(class: &str) -> ObjectImage {
    let mut image = ObjectImage::of_kind(
        BindingRepresentation::Altrep {
            class: class.to_owned(),
        },
        ObjectKind::Altrep,
    );
    image.issues = altrep_issues(&MemberPath::root(), class);
    image
}

fn unforced_promise_image() -> ObjectImage {
    let mut image = ObjectImage::of_kind(
        BindingRepresentation::Promise { forced: false },
        ObjectKind::Promise,
    );
    image.issues.push(member_issue(
        &MemberPath::root(),
        ObjectIssueKind::UnforcedPromise,
        "nested promise is preserved without forcing",
    ));
    image
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
        code => ObjectKind::Unsupported(UnsupportedObject::SexpType(code)),
    }
}
