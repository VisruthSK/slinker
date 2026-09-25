use crate::{Description, Version};
use sha2::{Digest as _, Sha256};
use std::fmt;
use std::path::PathBuf;

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Digest(pub String);

impl Digest {
    pub fn of(bytes: impl AsRef<[u8]>) -> Self {
        Self::finish(Sha256::new_with_prefix(bytes))
    }

    pub(crate) fn finish(hash: Sha256) -> Self {
        Self(hex::encode(hash.finalize()))
    }
}

/// Invocation-local semantic handle allocated by
/// [`TargetUniverse`](crate::package::TargetUniverse). It has no meaning across invocations.
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

/// Exact installed image selected for one invocation, independent of where its bytes live.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct PackageIdentity {
    pub name: String,
    pub version: Version,
    pub image_fingerprint: Digest,
}

impl fmt::Display for PackageIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} {}", self.name, self.version)
    }
}

/// Build-time physical location of an installed image.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackageLocation {
    pub library: PathBuf,
    pub root: PathBuf,
}

/// An installed image found by the physical package locator.
#[derive(Clone, Debug)]
pub struct InstalledPackage {
    pub identity: PackageIdentity,
    pub location: PackageLocation,
    pub description: Description,
}

/// How a package participates in the generated artifact.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum PackageRole {
    Root,
    Linked,
    External,
}
