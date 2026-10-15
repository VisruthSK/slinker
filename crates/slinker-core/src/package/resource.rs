use super::Atom;
use serde::{Deserialize, Deserializer, Serialize};
use std::borrow::Borrow;
use std::fmt;
use std::ops::Deref;
use std::path::{Component, Path};
use std::str::FromStr;

/// A resource selector contained within its owning package, including the package root.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct ResourcePath(Atom);

#[derive(Clone, Debug, thiserror::Error)]
#[error("resource path `{path}` cannot be represented inside one package")]
pub struct InvalidResourcePath {
    path: String,
}

impl ResourcePath {
    pub(crate) fn package_metadata() -> Self {
        Self("Meta/package.rds".into())
    }

    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl FromStr for ResourcePath {
    type Err = InvalidResourcePath;

    fn from_str(path: &str) -> Result<Self, Self::Err> {
        if path.contains('\0')
            || Path::new(path)
                .components()
                .any(|component| !matches!(component, Component::Normal(_) | Component::CurDir))
        {
            return Err(InvalidResourcePath { path: path.into() });
        }
        Ok(Self(path.into()))
    }
}

impl<'de> Deserialize<'de> for ResourcePath {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let path = <std::borrow::Cow<'de, str>>::deserialize(deserializer)?;
        path.parse().map_err(serde::de::Error::custom)
    }
}

impl Deref for ResourcePath {
    type Target = str;

    fn deref(&self) -> &str {
        self.as_str()
    }
}

impl Borrow<str> for ResourcePath {
    fn borrow(&self) -> &str {
        self.as_str()
    }
}

impl fmt::Display for ResourcePath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}
