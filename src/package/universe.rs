use crate::metadata::Priority;
use crate::package::{
    InstalledPackage, PackageId, PackageIdentity, PackageImage, PackageIndex, PackageLocation,
    PackageProvider, PackageRole, SyntaxValidation, fingerprint_image,
};
use crate::{Error, Result, TargetEnvironment};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

/// Frozen package-name answer for one invocation.
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

/// Invocation-local owner of package resolution, handles, absence, and role policy.
pub struct TargetUniverse<P: PackageProvider> {
    store: P,
    root: Option<String>,
    explicit_external: HashSet<String>,
    availability: HashMap<String, PackageAvailability>,
    packages: Vec<(InstalledPackage, PackageRole)>,
}

impl<P: PackageProvider> TargetUniverse<P> {
    pub fn new(store: P) -> Self {
        Self {
            store,
            root: None,
            explicit_external: HashSet::new(),
            availability: HashMap::new(),
            packages: Vec::new(),
        }
    }

    pub fn set_root(&mut self, root: impl Into<String>) {
        assert!(
            self.availability.is_empty(),
            "Root policy freezes before resolution"
        );
        assert!(self.root.replace(root.into()).is_none(), "Root is set once");
    }

    pub fn set_explicit_external(&mut self, packages: impl IntoIterator<Item = String>) {
        assert!(
            self.availability.is_empty(),
            "External policy freezes before resolution"
        );
        self.explicit_external.extend(packages);
    }

    pub fn target_environment(&self) -> &TargetEnvironment {
        self.store.target_environment()
    }

    /// Resolve a package name once; later calls return the frozen answer, including absence.
    pub fn resolve(&mut self, name: &str) -> Result<Option<PackageId>> {
        if let Some(availability) = self.availability.get(name) {
            return Ok(availability.package());
        }
        let located = self.store.locate(name)?;
        Ok(self.ingest(name, located))
    }

    pub fn require(&mut self, name: &str) -> Result<PackageId> {
        self.resolve(name)?.ok_or_else(|| {
            Error::Analysis(format!(
                "installed package `{name}` is absent from the frozen target universe"
            ))
        })
    }

    fn ingest(&mut self, name: &str, package: Option<InstalledPackage>) -> Option<PackageId> {
        let Some(package) = package else {
            self.availability
                .insert(name.to_owned(), PackageAvailability::Absent);
            return None;
        };
        let id = PackageId::from_index(self.packages.len());
        let (role, availability) = if self.root.as_deref() == Some(name) {
            (PackageRole::Root, PackageAvailability::Root(id))
        } else if self.explicit_external.contains(name)
            || matches!(
                package.description.priority_parsed(),
                Some(Ok(Priority::Base))
            )
        {
            (PackageRole::External, PackageAvailability::External(id))
        } else {
            (PackageRole::Linked, PackageAvailability::Linked(id))
        };
        self.packages.push((package, role));
        self.availability.insert(name.to_owned(), availability);
        Some(id)
    }

    pub fn availability(&self, name: &str) -> Option<PackageAvailability> {
        self.availability.get(name).copied()
    }

    pub fn package(&self, id: PackageId) -> &InstalledPackage {
        &self.packages[id.index()].0
    }

    pub fn identity(&self, id: PackageId) -> &PackageIdentity {
        &self.package(id).identity
    }

    pub fn name(&self, id: PackageId) -> &str {
        &self.identity(id).name
    }

    pub fn role(&self, id: PackageId) -> PackageRole {
        self.packages[id.index()].1
    }

    pub fn is_external(&self, id: PackageId) -> bool {
        self.role(id) == PackageRole::External
    }

    pub fn is_base_binding(&self, name: &str) -> bool {
        self.target_environment().base_bindings.contains(name)
    }

    pub fn index(&mut self, id: PackageId) -> Result<Arc<PackageIndex>> {
        self.store.index(&self.packages[id.index()].0)
    }

    pub fn binding_image(&mut self, id: PackageId, name: &str) -> Result<Arc<PackageImage>> {
        self.store.binding_image(&self.packages[id.index()].0, name)
    }

    pub fn resource_exists(&mut self, id: PackageId, path: &str) -> Result<bool> {
        self.store
            .resource_exists(&self.packages[id.index()].0, path)
    }

    pub fn validate_syntax(&mut self, source: &str) -> Result<SyntaxValidation> {
        self.store.validate_syntax(source)
    }

    pub fn normalize_syntax(&mut self, source: &str) -> Result<String> {
        self.store.normalize_syntax(source)
    }

    /// Freeze the selected physical images of the given packages for build orchestration.
    pub fn sources(&self, ids: impl IntoIterator<Item = PackageId>) -> PackageSources {
        PackageSources(
            ids.into_iter()
                .map(|id| {
                    let package = self.package(id);
                    (id, (package.identity.clone(), package.location.clone()))
                })
                .collect(),
        )
    }
}

/// Exact installed image and physical location selected for each finalized package.
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

    /// Re-fingerprint every frozen image and return the first one whose bytes changed.
    ///
    /// # Errors
    ///
    /// Fails when an image directory can no longer be read.
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
        located: Vec<String>,
    }

    impl PackageProvider for CountingStore {
        fn target_environment(&self) -> &TargetEnvironment {
            self.locator.target()
        }

        fn locate(&mut self, name: &str) -> Result<Option<InstalledPackage>> {
            self.located.push(name.to_owned());
            self.locator.locate(name)
        }

        fn index(&mut self, _package: &InstalledPackage) -> Result<Arc<PackageIndex>> {
            unreachable!("resolution never inspects images")
        }

        fn binding_image(
            &mut self,
            _package: &InstalledPackage,
            _name: &str,
        ) -> Result<Arc<PackageImage>> {
            unreachable!("resolution never inspects images")
        }

        fn validate_syntax(&mut self, _source: &str) -> Result<SyntaxValidation> {
            unreachable!("resolution never validates syntax")
        }

        fn normalize_syntax(&mut self, _source: &str) -> Result<String> {
            unreachable!("resolution never normalizes syntax")
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
        TargetUniverse::new(CountingStore {
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
            located: Vec::new(),
        })
    }

    #[test]
    fn absence_is_frozen_for_the_invocation() {
        let library = tempfile::tempdir().expect("library");
        let mut universe = universe(library.path());

        assert_eq!(universe.resolve("late").expect("first answer"), None);
        install(library.path(), "late");

        assert_eq!(universe.resolve("late").expect("frozen answer"), None);
        assert_eq!(universe.store.located, ["late"]);
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
        let mut forward = universe(library.path());
        let mut reverse = universe(library.path());

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
    fn roles_are_frozen_by_policy_before_resolution() {
        let library = tempfile::tempdir().expect("library");
        for name in ["root", "dependency", "kept"] {
            install(library.path(), name);
        }
        let mut universe = universe(library.path());
        universe.set_root("root");
        universe.set_explicit_external(["kept".to_owned()]);

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
        let mut universe = universe(library.path());
        let fixture = universe.require("fixture").expect("fixture");
        let sources = universe.sources([fixture]);
        assert_eq!(sources.changed().expect("unchanged"), None);

        fs::write(library.path().join("fixture/R"), "mutated").expect("mutate image");

        assert_eq!(
            sources
                .changed()
                .expect("fingerprint")
                .map(|identity| &identity.name),
            Some(&"fixture".to_owned())
        );
    }
}
