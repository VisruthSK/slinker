use crate::metadata::Priority;
use crate::package::{
    CanonicalSyntax, DispatchSubject, GenericName, InstalledPackage, PackageId, PackageIdentity,
    PackageImage, PackageIndex, PackageLocation, PackageName, PackageProvider, PackageResolver,
    PackageRole, SyntaxValidation, fingerprint_image,
};
use crate::{Error, Result, TargetEnvironment};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::{Arc, Mutex, RwLock};

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
    locating: Mutex<()>,
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
            locating: Mutex::new(()),
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
        let _locating = self.locating.lock().expect("universe locating lock");
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

    pub fn resource_exists(&self, id: PackageId, path: &str) -> Result<bool> {
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
        for (identity, location) in self.0.values() {
            if fingerprint_image(&location.root)? != identity.image_fingerprint {
                return Ok(Some(identity));
            }
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::package::PackageLocator;
    use crate::{Target, TargetEnvironment};
    use std::fs;
    use std::path::Path;

    struct CountingStore {
        locator: PackageLocator,
        located: Mutex<Vec<String>>,
    }

    impl PackageResolver for CountingStore {
        fn target_environment(&self) -> &TargetEnvironment {
            self.locator.target()
        }

        fn locate(&self, name: &str) -> Result<Option<InstalledPackage>> {
            self.located.lock().expect("located").push(name.to_owned());
            self.locator.locate(name)
        }
    }

    fn install(library: &Path, name: &str) {
        let root = library.join(name);
        fs::create_dir_all(&root).expect("package root");
        fs::write(
            root.join("DESCRIPTION"),
            format!("Package: {name}\nVersion: 1.0.0\n"),
        )
        .expect("DESCRIPTION");
    }

    fn universe(library: &Path) -> TargetUniverse<CountingStore> {
        universe_with_policy(library, "unused-root", HashSet::new())
    }

    fn universe_with_policy(
        library: &Path,
        root: &str,
        explicit_external: HashSet<PackageName>,
    ) -> TargetUniverse<CountingStore> {
        TargetUniverse::new(
            CountingStore {
                locator: PackageLocator::new(TargetEnvironment {
                    r_home: library.to_path_buf(),
                    target: Target {
                        r_version: String::new(),
                        os: String::new(),
                        arch: String::new(),
                    },
                    libraries: vec![library.to_path_buf()],
                    base_bindings: Default::default(),
                }),
                located: Mutex::new(Vec::new()),
            },
            root,
            explicit_external,
        )
    }

    #[test]
    fn absence_is_frozen_for_the_invocation() {
        let library = tempfile::tempdir().expect("library");
        let universe = universe(library.path());

        assert_eq!(universe.resolve("late").expect("first answer"), None);
        install(library.path(), "late");

        assert_eq!(universe.resolve("late").expect("frozen answer"), None);
        assert_eq!(*universe.store.located.lock().unwrap(), ["late"]);
        assert_eq!(
            universe.availability("late"),
            Some(PackageAvailability::Absent)
        );
    }

    #[test]
    fn package_ids_are_allocated_per_invocation_in_resolution_order() {
        let library = tempfile::tempdir().expect("library");
        install(library.path(), "first");
        install(library.path(), "second");
        let forward = universe(library.path());
        let reverse = universe(library.path());

        let forward_first = forward.require("first").expect("first");
        reverse.require("second").expect("second");
        let reverse_first = reverse.require("first").expect("first");

        assert_ne!(forward_first, reverse_first);
        assert_eq!(
            forward.identity(forward_first),
            reverse.identity(reverse_first)
        );
        assert_eq!(forward.require("first").expect("memoized"), forward_first);
    }

    #[test]
    fn roles_follow_the_policy_given_at_construction() {
        let library = tempfile::tempdir().expect("library");
        for name in ["root", "dependency", "kept"] {
            install(library.path(), name);
        }
        let universe = universe_with_policy(library.path(), "root", HashSet::from(["kept".into()]));

        let roles = ["root", "dependency", "kept"].map(|name| {
            let package = universe.require(name).expect("installed");
            universe.role(package)
        });

        assert_eq!(
            roles,
            [
                PackageRole::Root,
                PackageRole::Linked,
                PackageRole::External
            ]
        );
    }

    #[test]
    fn changed_selected_image_is_detected() {
        let library = tempfile::tempdir().expect("library");
        install(library.path(), "fixture");
        let universe = universe(library.path());
        let fixture = universe.require("fixture").expect("fixture");
        let sources = universe.sources([fixture]);
        assert_eq!(sources.changed().expect("unchanged"), None);

        fs::write(library.path().join("fixture/R"), "mutated").expect("mutate image");

        assert_eq!(
            sources
                .changed()
                .expect("fingerprint")
                .map(|identity| identity.name.as_str()),
            Some("fixture")
        );
    }
}
