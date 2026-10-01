use crate::package::{BindingName, ClassName, PackageIndex};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
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
    Other(String),
    Unavailable,
}

impl ObjectKind {
    pub fn from_r_type(value: &str) -> Self {
        match value {
            "closure" => Self::Closure,
            "NULL" => Self::Null,
            "logical" => Self::Logical,
            "integer" => Self::Integer,
            "double" => Self::Double,
            "complex" => Self::Complex,
            "character" => Self::Character,
            "raw" => Self::Raw,
            "symbol" => Self::Symbol,
            "list" => Self::List,
            "pairlist" => Self::Pairlist,
            "language" => Self::Language,
            "expression" => Self::Expression,
            "environment" => Self::Environment,
            "builtin" => Self::Builtin,
            "special" => Self::Special,
            "unavailable" => Self::Unavailable,
            other => Self::Other(other.to_owned()),
        }
    }

    pub fn needs_air(&self) -> bool {
        matches!(self, Self::Closure)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ObjectIssue {
    pub path: String,
    pub kind: String,
    pub detail: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ClosureSource {
    pub source: Arc<str>,
    pub environment: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct EmbeddedClosureSource {
    pub path: String,
    pub source: Arc<str>,
    pub environment: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct EmbeddedEnvironmentRef {
    pub path: String,
    pub environment: String,
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
pub struct BindingImage {
    pub name: BindingName,
    pub origin: BindingOrigin,
    pub representation: BindingRepresentation,
    #[serde(default)]
    pub classes: Vec<ClassName>,
    pub object_kind: ObjectKind,
    pub closure: Option<ClosureSource>,
    pub environment: Option<String>,
    pub embedded_closures: Vec<EmbeddedClosureSource>,
    pub embedded_environments: Vec<EmbeddedEnvironmentRef>,
    pub issues: Vec<ObjectIssue>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PrivateBindingImage {
    pub name: BindingName,
    pub representation: BindingRepresentation,
    #[serde(default)]
    pub classes: Vec<ClassName>,
    pub object_kind: ObjectKind,
    pub closure: Option<ClosureSource>,
    pub environment: Option<String>,
    pub embedded_closures: Vec<EmbeddedClosureSource>,
    pub embedded_environments: Vec<EmbeddedEnvironmentRef>,
    pub issues: Vec<ObjectIssue>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PrivateEnvironmentImage {
    pub id: String,
    pub parent: String,
    pub bindings: HashMap<BindingName, PrivateBindingImage>,
}

#[derive(Clone, Debug)]
pub struct PackageImage {
    pub index: Arc<PackageIndex>,
    pub bindings: HashMap<BindingName, BindingImage>,
    pub private_environments: HashMap<String, PrivateEnvironmentImage>,
}

impl PackageImage {
    pub fn binding(&self, name: &str) -> Option<&BindingImage> {
        self.bindings.get(name)
    }

    pub fn private_environment(&self, id: &str) -> Option<&PrivateEnvironmentImage> {
        self.private_environments.get(id)
    }

    pub fn private_binding(&self, environment: &str, name: &str) -> Option<&PrivateBindingImage> {
        self.private_environment(environment)?.bindings.get(name)
    }
}
