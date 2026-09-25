use crate::analysis::S3Id;
use crate::package::PackageIndex;
use std::collections::BTreeSet;

#[derive(Clone, Debug)]
pub(super) struct NamespaceBuilder {
    pub(super) bindings: BTreeSet<String>,
    pub(super) registrations: Vec<S3Id>,
}

impl NamespaceBuilder {
    pub(super) fn new(index: &PackageIndex) -> Self {
        let mut bindings = index.binding_names.iter().cloned().collect::<BTreeSet<_>>();
        bindings.extend([
            ".__NAMESPACE__.".into(),
            ".__S3MethodsTable__.".into(),
            ".packageName".into(),
        ]);
        Self {
            bindings,
            registrations: Vec::new(),
        }
    }

    pub(super) fn add_binding(&mut self, name: String) -> bool {
        self.bindings.insert(name)
    }

    pub(super) fn contains(&self, name: &str) -> bool {
        self.bindings.contains(name)
    }
}
