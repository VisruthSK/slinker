use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;

use crate::metadata::{Description, MetadataError};
use crate::target_env::{TargetEnvironment, TargetProvidedPackage};

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Hash)]
pub struct PackageId(pub(crate) u32);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PackageRole {
    Root,
    Internalized,
    TargetProvided,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum Capability {
    Pure,
    SyntaxObservation,
    DynamicPackageLookup,
    FileSystem,
    Network,
    Rng,
    Time,
    GlobalMutation,
    AmbientPackageDiscovery,
    ExternalPointer,
    EnvironmentIdentity,
    Promise,
    WeakReference,
    Altrep,
    S4,
    S7,
    R6,
    UnknownCall,
    NativeOpaque,
    OnLoad,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Package {
    pub id: PackageId,
    pub name: String,
    pub version: String,
    pub role: PackageRole,
    pub has_nonprovided_depends: bool,
    /// Present only for exact target-provided identities captured from the target R library.
    pub target_library: Option<PathBuf>,
}

#[derive(Clone, Debug, Default)]
pub struct PackageSet {
    packages: Vec<Package>,
    by_name: BTreeMap<String, PackageId>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PackageSetError {
    IdentityConflict {
        name: String,
        existing_version: String,
        existing_role: PackageRole,
        requested_version: String,
        requested_role: PackageRole,
    },
}

impl PackageSet {
    pub fn insert(
        &mut self,
        name: impl Into<String>,
        version: impl Into<String>,
        role: PackageRole,
    ) -> PackageId {
        let name = name.into();
        if let Some(id) = self.by_name.get(&name) {
            return *id;
        }

        let id = PackageId(self.packages.len() as u32);
        self.packages.push(Package {
            id,
            name: name.clone(),
            version: version.into(),
            role,
            has_nonprovided_depends: false,
            target_library: None,
        });
        self.by_name.insert(name, id);
        id
    }

    pub fn insert_target_provided(
        &mut self,
        package: &TargetProvidedPackage,
    ) -> Result<PackageId, PackageSetError> {
        if let Some(id) = self.by_name.get(&package.name).copied() {
            let existing = &mut self.packages[id.0 as usize];
            if existing.version != package.version || existing.role != PackageRole::TargetProvided {
                return Err(PackageSetError::IdentityConflict {
                    name: package.name.clone(),
                    existing_version: existing.version.clone(),
                    existing_role: existing.role,
                    requested_version: package.version.clone(),
                    requested_role: PackageRole::TargetProvided,
                });
            }
            existing.target_library = Some(package.library.clone());
            return Ok(id);
        }

        let id = self.insert(&package.name, &package.version, PackageRole::TargetProvided);
        self.get_mut(id).target_library = Some(package.library.clone());
        Ok(id)
    }

    pub fn insert_target_environment(
        &mut self,
        target: &TargetEnvironment,
    ) -> Result<Vec<PackageId>, PackageSetError> {
        target
            .packages
            .iter()
            .map(|package| self.insert_target_provided(package))
            .collect()
    }

    pub fn apply_description_policy(
        &mut self,
        package: PackageId,
        description: &Description,
        target: &TargetEnvironment,
    ) -> Result<(), MetadataError> {
        self.get_mut(package).has_nonprovided_depends = description
            .depends()?
            .iter()
            .any(|dependency| target.package(&dependency.name).is_none());
        Ok(())
    }

    pub fn get(&self, id: PackageId) -> &Package {
        &self.packages[id.0 as usize]
    }

    pub fn get_mut(&mut self, id: PackageId) -> &mut Package {
        &mut self.packages[id.0 as usize]
    }

    pub fn find(&self, name: &str) -> Option<PackageId> {
        self.by_name.get(name).copied()
    }

    pub fn iter(&self) -> impl Iterator<Item = &Package> {
        self.packages.iter()
    }
}

impl fmt::Display for PackageSetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::IdentityConflict {
                name,
                existing_version,
                existing_role,
                requested_version,
                requested_role,
            } => write!(
                f,
                "package identity conflict for {name}: existing {existing_version}/{existing_role:?}, requested {requested_version}/{requested_role:?}"
            ),
        }
    }
}

impl std::error::Error for PackageSetError {}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Target {
    pub r_version: String,
    pub os: String,
    pub arch: String,
}

impl fmt::Display for PackageId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "pkg#{}", self.0)
    }
}
