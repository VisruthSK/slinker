use super::intern::intern;
use serde::{Deserialize, Serialize};
use std::borrow::Borrow;
use std::fmt;
use std::ops::Deref;

macro_rules! name_type {
    ($name:ident) => {
        #[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
        #[serde(transparent)]
        pub struct $name(std::sync::Arc<str>);

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let text = <std::borrow::Cow<'de, str>>::deserialize(deserializer)?;
                Ok(Self(intern(&text)))
            }
        }

        impl $name {
            pub fn new(name: impl Into<String>) -> Self {
                Self(intern(&name.into()))
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl Deref for $name {
            type Target = str;

            fn deref(&self) -> &str {
                &self.0
            }
        }

        impl Borrow<str> for $name {
            fn borrow(&self) -> &str {
                &self.0
            }
        }

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                &self.0
            }
        }

        impl PartialEq<str> for $name {
            fn eq(&self, other: &str) -> bool {
                &*self.0 == other
            }
        }

        impl PartialEq<&str> for $name {
            fn eq(&self, other: &&str) -> bool {
                &*self.0 == *other
            }
        }

        impl PartialEq<String> for $name {
            fn eq(&self, other: &String) -> bool {
                &*self.0 == other.as_str()
            }
        }

        impl PartialEq<$name> for str {
            fn eq(&self, other: &$name) -> bool {
                self == &*other.0
            }
        }

        impl PartialEq<$name> for &str {
            fn eq(&self, other: &$name) -> bool {
                *self == &*other.0
            }
        }

        impl PartialEq<$name> for String {
            fn eq(&self, other: &$name) -> bool {
                *self == &*other.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl From<String> for $name {
            fn from(name: String) -> Self {
                Self(intern(&name))
            }
        }

        impl From<&String> for $name {
            fn from(name: &String) -> Self {
                Self(intern(name))
            }
        }

        impl From<&str> for $name {
            fn from(name: &str) -> Self {
                Self(intern(name))
            }
        }
    };
}

name_type!(BindingName);
name_type!(ClassName);
name_type!(GenericName);
name_type!(PackageName);
name_type!(ComponentName);
name_type!(DatasetName);
name_type!(SymbolName);
name_type!(DataSetId);
name_type!(ExportName);
name_type!(GenericLabel);
name_type!(Atom);

#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum EnvironmentLabel {
    Namespace(PackageName),
    Base,
    Empty,
    InstalledPrivate(Atom),
    Unsupported(Atom),
}

impl Serialize for EnvironmentLabel {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for EnvironmentLabel {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let label = <std::borrow::Cow<'de, str>>::deserialize(deserializer)?;
        match label.as_ref() {
            "base:base" => Ok(Self::Base),
            "base:empty" => Ok(Self::Empty),
            text => match text.split_once(':') {
                Some(("namespace", name)) if !name.is_empty() => Ok(Self::namespace(name)),
                Some(("private", key)) if !key.is_empty() => Ok(Self::private(key)),
                Some(("unsupported", detail)) => Ok(Self::unsupported(detail)),
                _ => Err(serde::de::Error::custom(
                    "invalid installed environment identity",
                )),
            },
        }
    }
}

impl fmt::Display for EnvironmentLabel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Namespace(package) => write!(f, "namespace:{package}"),
            Self::Base => f.write_str("base:base"),
            Self::Empty => f.write_str("base:empty"),
            Self::InstalledPrivate(key) => write!(f, "private:{key}"),
            Self::Unsupported(detail) => write!(f, "unsupported:{detail}"),
        }
    }
}

impl EnvironmentLabel {
    pub fn namespace(package: &str) -> Self {
        Self::Namespace(package.into())
    }

    pub fn base() -> Self {
        Self::Base
    }

    pub fn empty() -> Self {
        Self::Empty
    }

    pub fn private(key: impl fmt::Display) -> Self {
        Self::InstalledPrivate(key.to_string().into())
    }

    pub fn unsupported(detail: &str) -> Self {
        Self::Unsupported(detail.into())
    }

    pub fn is_unsupported(&self) -> bool {
        matches!(self, Self::Unsupported(_))
    }

    pub fn is_namespace_of(&self, package: &str) -> bool {
        matches!(self, Self::Namespace(name) if name == package)
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum MemberStep {
    Element(std::num::NonZeroUsize),
    Attribute {
        position: std::num::NonZeroUsize,
        name: BindingName,
    },
    Formals,
    Body,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct MemberPath(Vec<MemberStep>);

impl MemberPath {
    pub fn root() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn child(&self, step: MemberStep) -> Self {
        let mut path = self.clone();
        path.0.push(step);
        path
    }

    pub fn is_root(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Display for MemberPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("$")?;
        for step in &self.0 {
            match step {
                MemberStep::Element(position) => write!(f, "[[{position}]]")?,
                MemberStep::Attribute { position, name } => {
                    write!(f, ".attr[[{position}, {:?}]]", name.as_str())?;
                }
                MemberStep::Formals => f.write_str(".formals")?,
                MemberStep::Body => f.write_str(".body")?,
            }
        }
        Ok(())
    }
}

macro_rules! from_atom {
    ($($name:ident),+ $(,)?) => {
        $(
            impl From<Atom> for $name {
                fn from(atom: Atom) -> Self {
                    Self(atom.0)
                }
            }

            impl From<&Atom> for $name {
                fn from(atom: &Atom) -> Self {
                    Self(std::sync::Arc::clone(&atom.0))
                }
            }

            impl From<$name> for Atom {
                fn from(name: $name) -> Self {
                    Self(name.0)
                }
            }

            impl From<&$name> for Atom {
                fn from(name: &$name) -> Self {
                    Self(std::sync::Arc::clone(&name.0))
                }
            }

            impl PartialEq<Atom> for $name {
                fn eq(&self, other: &Atom) -> bool {
                    *self.0 == *other.0
                }
            }
        )+
    };
}

from_atom!(
    BindingName,
    ClassName,
    GenericName,
    PackageName,
    ComponentName,
    DatasetName,
    SymbolName,
    DataSetId,
    ExportName,
    GenericLabel,
);
