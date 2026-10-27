use crate::package::PackageName;
use crate::{Description, Version};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use std::fmt::{self, Write as _};
use std::path::PathBuf;

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Digest(String);

impl Digest {
    pub fn of(bytes: impl AsRef<[u8]>) -> Self {
        Self::finish(Sha256::new_with_prefix(bytes))
    }

    pub(crate) fn finish(hash: Sha256) -> Self {
        Self(
            hash.finalize()
                .iter()
                .fold(String::with_capacity(64), |mut hex, byte| {
                    let _ = write!(hex, "{byte:02x}");
                    hex
                }),
        )
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<String> for Digest {
    fn from(digest: String) -> Self {
        Self(digest)
    }
}

impl From<&str> for Digest {
    fn from(digest: &str) -> Self {
        Self(digest.to_owned())
    }
}

impl fmt::Display for Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PackageId(u32);

impl PackageId {
    pub(crate) fn from_index(index: usize) -> Self {
        Self(u32::try_from(index).expect("package universe exceeds u32"))
    }

    pub(crate) fn index(self) -> usize {
        self.0 as usize
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct PackageIdentity {
    pub name: PackageName,
    pub version: Version,
    pub image_fingerprint: Digest,
}

impl fmt::Display for PackageIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} {}", self.name, self.version)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackageLocation {
    pub library: PathBuf,
    pub root: PathBuf,
}

#[derive(Clone, Debug)]
pub struct InstalledPackage {
    pub identity: PackageIdentity,
    pub location: PackageLocation,
    pub description: Description,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum PackageRole {
    Root,
    Linked,
    External,
}
