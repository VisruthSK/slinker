use crate::package::{
    InstalledPackage, PackageId, PackageImage, PackageIndex, PackageProvider, SyntaxValidation,
};
use crate::{Result, TargetEnvironment};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

/// Frozen package-name answer for one invocation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PackageAvailability {
    Root(PackageId),
    Linked(PackageId),
    External(PackageId),
    Absent,
}

/// Invocation-local owner of package resolution, absence, and role policy.
pub struct TargetUniverse<P: PackageProvider> {
    store: P,
    root: Option<String>,
    explicit_external: HashSet<String>,
    availability: HashMap<String, PackageAvailability>,
    packages: HashMap<PackageId, InstalledPackage>,
}

impl<P: PackageProvider> TargetUniverse<P> {
    pub fn new(store: P) -> Self {
        Self {
            store,
            root: None,
            explicit_external: HashSet::new(),
            availability: HashMap::new(),
            packages: HashMap::new(),
        }
    }

    pub fn set_root(&mut self, root: impl Into<String>) {
        assert!(self.root.replace(root.into()).is_none(), "Root is set once");
    }

    pub fn set_explicit_external(&mut self, packages: impl IntoIterator<Item = String>) {
        assert!(
            self.availability.is_empty(),
            "External policy freezes before resolution"
        );
        self.explicit_external.extend(packages);
    }

    pub fn target_environment(&self) -> Option<&TargetEnvironment> {
        self.store.target_environment()
    }

    pub fn locate(&mut self, name: &str) -> Result<InstalledPackage> {
        self.locate_optional(name)?.ok_or_else(|| {
            crate::Error::Analysis(format!(
                "installed package `{name}` is absent from the frozen target universe"
            ))
        })
    }

    pub fn locate_optional(&mut self, name: &str) -> Result<Option<InstalledPackage>> {
        if let Some(availability) = self.availability.get(name) {
            return Ok(match availability {
                PackageAvailability::Root(id)
                | PackageAvailability::Linked(id)
                | PackageAvailability::External(id) => self.packages.get(id).cloned(),
                PackageAvailability::Absent => None,
            });
        }
        let Some(package) = self.store.locate_optional(name)? else {
            self.availability
                .insert(name.to_owned(), PackageAvailability::Absent);
            return Ok(None);
        };
        let role = if self.root.as_deref() == Some(name) {
            PackageAvailability::Root(package.id.clone())
        } else if self.explicit_external.contains(name)
            || matches!(
                package.description.priority_parsed(),
                Some(Ok(crate::metadata::Priority::Base))
            )
        {
            PackageAvailability::External(package.id.clone())
        } else {
            PackageAvailability::Linked(package.id.clone())
        };
        self.packages.insert(package.id.clone(), package.clone());
        self.availability.insert(name.to_owned(), role);
        Ok(Some(package))
    }

    pub fn locate_many(&mut self, names: &[String], jobs: usize) -> Result<Vec<InstalledPackage>> {
        // Resolve through the memoized universe even when the physical store can prefetch in
        // parallel; this is the single point that freezes each semantic answer.
        let unresolved = names
            .iter()
            .filter(|name| !self.availability.contains_key(name.as_str()))
            .cloned()
            .collect::<Vec<_>>();
        if !unresolved.is_empty() {
            for package in self.store.locate_many(&unresolved, jobs)? {
                let name = package.id.name.clone();
                let role = if self.root.as_deref() == Some(name.as_str()) {
                    PackageAvailability::Root(package.id.clone())
                } else if self.explicit_external.contains(&name)
                    || matches!(
                        package.description.priority_parsed(),
                        Some(Ok(crate::metadata::Priority::Base))
                    )
                {
                    PackageAvailability::External(package.id.clone())
                } else {
                    PackageAvailability::Linked(package.id.clone())
                };
                self.packages.insert(package.id.clone(), package);
                self.availability.insert(name, role);
            }
        }
        names.iter().map(|name| self.locate(name)).collect()
    }

    pub fn availability(&self, name: &str) -> Option<&PackageAvailability> {
        self.availability.get(name)
    }

    pub fn is_external(&self, package: &InstalledPackage) -> bool {
        matches!(
            self.availability.get(&package.id.name),
            Some(PackageAvailability::External(id)) if id == &package.id
        )
    }

    pub fn is_base_binding(&self, name: &str) -> bool {
        self.store
            .target_environment()
            .is_some_and(|target| target.base_bindings.contains(name))
    }

    pub fn index(&mut self, package: &InstalledPackage) -> Result<Arc<PackageIndex>> {
        self.store.index(package)
    }

    pub fn binding_image(
        &mut self,
        package: &InstalledPackage,
        name: &str,
    ) -> Result<Arc<PackageImage>> {
        self.store.binding_image(package, name)
    }

    pub fn prefetch_indexes(&mut self, packages: &[InstalledPackage], jobs: usize) -> Result<()> {
        self.store.prefetch_indexes(packages, jobs)
    }

    pub fn resource_exists(&mut self, package: &InstalledPackage, path: &str) -> Result<bool> {
        self.store.resource_exists(package, path)
    }

    pub fn validate_syntax(
        &mut self,
        id: &PackageId,
        binding: &str,
        source: &str,
    ) -> Result<SyntaxValidation> {
        self.store.validate_syntax(id, binding, source)
    }

    pub fn normalize_syntax(&mut self, source: &str) -> Result<String> {
        self.store.normalize_syntax(source)
    }
}
