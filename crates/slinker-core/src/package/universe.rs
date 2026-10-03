use crate::metadata::Priority;
use crate::package::{
    CanonicalSyntax, DispatchSubject, GenericName, InstalledPackage, PackageId, PackageIdentity,
    PackageImage, PackageIndex, PackageLocation, PackageName, PackageProvider, PackageResolver,
    PackageRole, SyntaxValidation, fingerprint_image,
};
use crate::{Error, Result, TargetEnvironment};
use rayon::prelude::*;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::{Arc, RwLock};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PackageAvailability {
    Root(PackageId),
    Linked(PackageId),
    External(PackageId),
    Absent,
}

impl PackageAvailability {
    pub fn package(self) -> Option<PackageId> {
        match self {
            Self::Root(id) | Self::Linked(id) | Self::External(id) => Some(id),
            Self::Absent => None,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub enum DispatchCallee<'a> {
    Base {
        binding: &'a str,
    },
    Package {
        package: PackageId,
        binding: &'a str,
    },
}

#[derive(Debug)]
pub struct PackageEntry {
    pub package: InstalledPackage,
    pub role: PackageRole,
}

#[derive(Default)]
struct Roster {
    availability: HashMap<PackageName, PackageAvailability>,
    packages: Vec<Arc<PackageEntry>>,
}

pub struct TargetUniverse<P: PackageResolver> {
    store: P,
    root: PackageName,
    explicit_external: HashSet<PackageName>,
    roster: RwLock<Roster>,
}

impl<P: PackageResolver> TargetUniverse<P> {
    pub fn new(
        store: P,
        root: impl Into<PackageName>,
        explicit_external: HashSet<PackageName>,
    ) -> Self {
        Self {
            store,
            root: root.into(),
            explicit_external,
            roster: RwLock::new(Roster::default()),
        }
    }

    pub fn root_name(&self) -> &PackageName {
        &self.root
    }

    pub fn target_environment(&self) -> &TargetEnvironment {
        self.store.target_environment()
    }

    pub fn resolve(&self, name: &str) -> Result<Option<PackageId>> {
        if let Some(availability) = self.availability(name) {
            return Ok(availability.package());
        }
        let located = self.store.locate(name)?;
        Ok(self.ingest(name, located))
    }

    pub fn require(&self, name: &str) -> Result<PackageId> {
        self.resolve(name)?.ok_or_else(|| {
            Error::Analysis(format!(
                "installed package `{name}` is absent from the frozen target universe"
            ))
        })
    }

    fn ingest(&self, name: &str, package: Option<InstalledPackage>) -> Option<PackageId> {
        let mut roster = self.roster.write().expect("universe roster");
        if let Some(known) = roster.availability.get(name) {
            return known.package();
        }
        let Some(package) = package else {
            roster
                .availability
                .insert(PackageName::from(name), PackageAvailability::Absent);
            return None;
        };
        let id = PackageId::from_index(roster.packages.len());
        let (role, availability) = if self.root == name {
            (PackageRole::Root, PackageAvailability::Root(id))
        } else if self.explicit_external.contains(name) || is_platform(&package) {
            (PackageRole::External, PackageAvailability::External(id))
        } else {
            (PackageRole::Linked, PackageAvailability::Linked(id))
        };
        roster
            .packages
            .push(Arc::new(PackageEntry { package, role }));
        roster
            .availability
            .insert(PackageName::from(name), availability);
        Some(id)
    }

    pub fn availability(&self, name: &str) -> Option<PackageAvailability> {
        self.roster
            .read()
            .expect("universe roster")
            .availability
            .get(name)
            .copied()
    }

    pub fn entry(&self, id: PackageId) -> Arc<PackageEntry> {
        Arc::clone(&self.roster.read().expect("universe roster").packages[id.index()])
    }

    pub fn identity(&self, id: PackageId) -> PackageIdentity {
        self.entry(id).package.identity.clone()
    }

    pub fn name(&self, id: PackageId) -> PackageName {
        self.entry(id).package.identity.name.clone()
    }

    pub fn role(&self, id: PackageId) -> PackageRole {
        self.entry(id).role
    }

    pub fn is_external(&self, id: PackageId) -> bool {
        self.role(id) == PackageRole::External
    }

    pub fn is_platform(&self, id: PackageId) -> bool {
        is_platform(&self.entry(id).package)
    }

    pub fn is_base_binding(&self, name: &str) -> bool {
        self.target_environment().base_bindings.contains(name)
    }

    pub fn consulted(&self) -> Vec<(PackageName, Option<PackageIdentity>)> {
        let roster = self.roster.read().expect("universe roster");
        let mut consulted = roster
            .availability
            .iter()
            .map(|(name, availability)| {
                let identity = availability
                    .package()
                    .map(|id| roster.packages[id.index()].package.identity.clone());
                (name.clone(), identity)
            })
            .collect::<Vec<_>>();
        consulted.sort_by(|left, right| left.0.cmp(&right.0));
        consulted
    }

    pub fn sources(&self, ids: impl IntoIterator<Item = PackageId>) -> PackageSources {
        PackageSources(
            ids.into_iter()
                .map(|id| {
                    let entry = self.entry(id);
                    (
                        id,
                        (
                            entry.package.identity.clone(),
                            entry.package.location.clone(),
                        ),
                    )
                })
                .collect(),
        )
    }
}

impl<P: PackageProvider> TargetUniverse<P> {
    pub fn index(&self, id: PackageId) -> Result<Arc<PackageIndex>> {
        self.store.index(&self.entry(id).package)
    }

    pub fn binding_image(&self, id: PackageId, name: &str) -> Result<Arc<PackageImage>> {
        self.store.binding_image(&self.entry(id).package, name)
    }

    pub fn dispatch_generics(&self, callee: DispatchCallee<'_>) -> Result<BTreeSet<GenericName>> {
        match callee {
            DispatchCallee::Base { binding } => self
                .store
                .dispatch_generics(DispatchSubject::Base { binding }),
            DispatchCallee::Package { package, binding } => {
                self.store.dispatch_generics(DispatchSubject::Installed {
                    package: &self.entry(package).package,
                    binding,
                })
            }
        }
    }

    pub fn resource_exists(&self, id: PackageId, path: &super::ResourcePath) -> Result<bool> {
        self.store.resource_exists(&self.entry(id).package, path)
    }

    pub fn validate_syntax(&self, source: &str) -> Result<SyntaxValidation> {
        self.store.validate_syntax(source)
    }

    pub fn canonical_syntax(&self, source: &str) -> Result<CanonicalSyntax> {
        self.store.canonical_syntax(source)
    }

    pub fn prefetch_canonical_syntax(&self, sources: &[&str]) -> Result<()> {
        self.store.prefetch_canonical_syntax(sources)
    }

    pub fn prefetch_binding_images(&self, id: PackageId, names: &[&str]) -> Result<()> {
        self.store
            .prefetch_binding_images(&self.entry(id).package, names)
    }
}

fn is_platform(package: &InstalledPackage) -> bool {
    matches!(
        package.description.priority_parsed(),
        Some(Ok(Priority::Base))
    )
}

#[derive(Clone, Debug, Default)]
pub struct PackageSources(BTreeMap<PackageId, (PackageIdentity, PackageLocation)>);

impl PackageSources {
    pub fn get(&self, id: PackageId) -> Option<(&PackageIdentity, &PackageLocation)> {
        self.0
            .get(&id)
            .map(|(identity, location)| (identity, location))
    }

    pub fn iter(&self) -> impl Iterator<Item = (PackageId, &PackageIdentity, &PackageLocation)> {
        self.0
            .iter()
            .map(|(id, (identity, location))| (*id, identity, location))
    }

    pub fn changed(&self) -> Result<Option<&PackageIdentity>> {
        let packages = self.0.values().collect::<Vec<_>>();
        let unchanged = packages
            .par_iter()
            .map(|(identity, location)| {
                Ok(fingerprint_image(&location.root)? == identity.image_fingerprint)
            })
            .collect::<Vec<Result<bool>>>();
        for ((identity, _), package_unchanged) in packages.into_iter().zip(unchanged) {
            if !package_unchanged? {
                return Ok(Some(identity));
            }
        }
        Ok(None)
    }
}

#[cfg(test)]
#[path = "../../tests/unit/package/universe.rs"]
mod tests;
