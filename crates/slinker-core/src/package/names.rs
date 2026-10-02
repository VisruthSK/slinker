use serde::{Deserialize, Serialize};
use std::borrow::Borrow;
use std::fmt;
use std::ops::Deref;

macro_rules! name_type {
    ($name:ident) => {
        #[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            pub fn new(name: impl Into<String>) -> Self {
                Self(name.into())
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
                self.0 == other
            }
        }

        impl PartialEq<&str> for $name {
            fn eq(&self, other: &&str) -> bool {
                self.0 == *other
            }
        }

        impl PartialEq<String> for $name {
            fn eq(&self, other: &String) -> bool {
                &self.0 == other
            }
        }

        impl PartialEq<$name> for str {
            fn eq(&self, other: &$name) -> bool {
                self == other.0
            }
        }

        impl PartialEq<$name> for &str {
            fn eq(&self, other: &$name) -> bool {
                *self == other.0
            }
        }

        impl PartialEq<$name> for String {
            fn eq(&self, other: &$name) -> bool {
                *self == other.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl From<String> for $name {
            fn from(name: String) -> Self {
                Self(name)
            }
        }

        impl From<&String> for $name {
            fn from(name: &String) -> Self {
                Self(name.clone())
            }
        }

        impl From<&str> for $name {
            fn from(name: &str) -> Self {
                Self(name.to_owned())
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
name_type!(ResourcePath);
name_type!(SymbolName);
name_type!(DataSetId);
name_type!(ExportName);
name_type!(GenericLabel);
name_type!(EnvironmentLabel);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EnvironmentKind<'a> {
    Namespace(&'a str),
    Base,
    Empty,
    Private,
    Derived,
    Unsupported(&'a str),
    Other,
}

impl EnvironmentLabel {
    pub fn namespace(package: &str) -> Self {
        Self(format!("namespace:{package}"))
    }

    pub fn base() -> Self {
        Self("base:base".into())
    }

    pub fn empty() -> Self {
        Self("base:empty".into())
    }

    pub fn private(key: impl fmt::Display) -> Self {
        Self(format!("private:{key}"))
    }

    pub fn derived(sequence: usize) -> Self {
        Self(format!("derived:{sequence}"))
    }

    pub fn unsupported(detail: &str) -> Self {
        Self(format!("unsupported:{detail}"))
    }

    pub fn kind(&self) -> EnvironmentKind<'_> {
        let label = self.0.as_str();
        if let Some(package) = label.strip_prefix("namespace:") {
            EnvironmentKind::Namespace(package)
        } else if let Some(detail) = label.strip_prefix("unsupported:") {
            EnvironmentKind::Unsupported(detail)
        } else if label.starts_with("derived:") {
            EnvironmentKind::Derived
        } else if label.starts_with("private:") {
            EnvironmentKind::Private
        } else if label == "base:base" {
            EnvironmentKind::Base
        } else if label == "base:empty" {
            EnvironmentKind::Empty
        } else {
            EnvironmentKind::Other
        }
    }

    pub fn is_derived(&self) -> bool {
        self.kind() == EnvironmentKind::Derived
    }

    pub fn is_unsupported(&self) -> bool {
        matches!(self.kind(), EnvironmentKind::Unsupported(_))
    }

    pub fn is_namespace_of(&self, package: &str) -> bool {
        self.kind() == EnvironmentKind::Namespace(package)
    }
}
name_type!(MemberPath);

impl MemberPath {
    pub fn root() -> Self {
        Self("$".into())
    }

    #[must_use]
    pub fn field(&self, name: &str) -> Self {
        Self(format!("{}${name}", self.0))
    }

    #[must_use]
    pub fn element(&self, position: usize) -> Self {
        Self(format!("{}[[{position}]]", self.0))
    }

    pub fn is_root(&self) -> bool {
        self.0 == "$"
    }

    pub fn direct_field(&self) -> Option<&str> {
        let name = self.0.strip_prefix("$$")?;
        (!name.is_empty() && !name.contains(['$', '[', ']'])).then_some(name)
    }

    pub fn direct_element(&self) -> Option<usize> {
        let digits = self.0.strip_prefix("$[[")?.strip_suffix("]]")?;
        (!digits.contains(['[', '$', '.']))
            .then(|| digits.parse().ok())
            .flatten()
    }
}
