use crate::package::{BindingName, ClassName, EnvironmentLabel, MemberPath, PackageIndex};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum BindingOrigin {
    Code,
    Sysdata,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum ObjectKind {
    Closure,
    Null,
    Logical,
    Integer,
    Double,
    Complex,
    Character,
    Raw,
    Symbol,
    List,
    Pairlist,
    Language,
    Expression,
    Environment,
    Builtin,
    Special,
    Promise,
    ActiveBinding,
    Altrep,
    ExternalPointer,
    WeakReference,
    Unsupported(UnsupportedObject),
    Unavailable,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum UnsupportedObject {
    SexpType(u32),
    DepthLimit,
}

impl fmt::Display for UnsupportedObject {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SexpType(code) => write!(f, "SEXPTYPE {code}"),
            Self::DepthLimit => f.write_str("depth"),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ObjectIssue {
    pub path: MemberPath,
    pub kind: ObjectIssueKind,
    pub detail: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObjectIssueKind {
    Altrep,
    EnvironmentIdentity,
    ExternalPointer,
    ObjectDepth,
    UnforcedPromise,
    WeakReference,
}

impl fmt::Display for ObjectIssueKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Altrep => "altrep",
            Self::EnvironmentIdentity => "environment_identity",
            Self::ExternalPointer => "external_pointer",
            Self::ObjectDepth => "object_depth",
            Self::UnforcedPromise => "unforced_promise",
            Self::WeakReference => "weak_reference",
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ClosureSource {
    pub source: Arc<str>,
    pub environment: EnvironmentLabel,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct EmbeddedClosureSource {
    pub path: MemberPath,
    pub source: Arc<str>,
    pub environment: EnvironmentLabel,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct EmbeddedEnvironmentRef {
    pub path: MemberPath,
    pub environment: EnvironmentLabel,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BindingRepresentation {
    Value,
    LazyLoadPromise,
    Promise { forced: bool },
    ActiveBinding,
    Altrep { class: String },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ObjectImage {
    pub representation: BindingRepresentation,
    #[serde(default)]
    pub classes: Vec<ClassName>,
    pub object_kind: ObjectKind,
    pub closure: Option<ClosureSource>,
    pub environment: Option<EnvironmentLabel>,
    pub embedded_closures: Vec<EmbeddedClosureSource>,
    pub embedded_environments: Vec<EmbeddedEnvironmentRef>,
    pub issues: Vec<ObjectIssue>,
}

impl ObjectImage {
    pub fn absorb_members(&mut self, member: ObjectImage) {
        self.embedded_closures.extend(member.embedded_closures);
        self.embedded_environments
            .extend(member.embedded_environments);
        self.issues.extend(member.issues);
    }

    pub fn environment_labels(&self) -> impl Iterator<Item = &EnvironmentLabel> {
        self.closure
            .iter()
            .map(|closure| &closure.environment)
            .chain(&self.environment)
            .chain(
                self.embedded_closures
                    .iter()
                    .map(|closure| &closure.environment),
            )
            .chain(
                self.embedded_environments
                    .iter()
                    .map(|reference| &reference.environment),
            )
    }

    pub fn of_kind(representation: BindingRepresentation, object_kind: ObjectKind) -> Self {
        Self {
            representation,
            classes: Vec::new(),
            object_kind,
            closure: None,
            environment: None,
            embedded_closures: Vec::new(),
            embedded_environments: Vec::new(),
            issues: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BindingImage {
    pub name: BindingName,
    pub origin: BindingOrigin,
    #[serde(flatten)]
    pub object: ObjectImage,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PrivateBindingImage {
    pub name: BindingName,
    #[serde(flatten)]
    pub object: ObjectImage,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PrivateEnvironmentImage {
    pub id: EnvironmentLabel,
    pub parent: EnvironmentLabel,
    pub bindings: HashMap<BindingName, PrivateBindingImage>,
}

#[derive(Clone, Debug)]
pub struct PackageImage {
    pub index: Arc<PackageIndex>,
    pub bindings: HashMap<BindingName, BindingImage>,
    pub private_environments: HashMap<EnvironmentLabel, PrivateEnvironmentImage>,
}

impl PackageImage {
    pub fn binding(&self, name: &str) -> Option<&BindingImage> {
        self.bindings.get(name)
    }

    pub fn private_environment(&self, id: &EnvironmentLabel) -> Option<&PrivateEnvironmentImage> {
        self.private_environments.get(id)
    }

    pub fn private_binding(
        &self,
        environment: &EnvironmentLabel,
        name: &str,
    ) -> Option<&PrivateBindingImage> {
        self.private_environment(environment)?.bindings.get(name)
    }
}
