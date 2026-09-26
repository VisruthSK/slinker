use serde::{Deserialize, Serialize};
use std::borrow::Borrow;
use std::fmt;
use std::ops::Deref;

#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct BindingName(String);

impl BindingName {
    pub fn new(name: impl Into<String>) -> Self {
        Self(name.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_string(self) -> String {
        self.0
    }
}

impl Deref for BindingName {
    type Target = str;

    fn deref(&self) -> &str {
        &self.0
    }
}

impl Borrow<str> for BindingName {
    fn borrow(&self) -> &str {
        &self.0
    }
}

impl AsRef<str> for BindingName {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl PartialEq<str> for BindingName {
    fn eq(&self, other: &str) -> bool {
        self.0 == other
    }
}

impl PartialEq<&str> for BindingName {
    fn eq(&self, other: &&str) -> bool {
        self.0 == *other
    }
}

impl PartialEq<String> for BindingName {
    fn eq(&self, other: &String) -> bool {
        &self.0 == other
    }
}

impl fmt::Display for BindingName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<String> for BindingName {
    fn from(name: String) -> Self {
        Self(name)
    }
}

impl From<&str> for BindingName {
    fn from(name: &str) -> Self {
        Self(name.to_owned())
    }
}
