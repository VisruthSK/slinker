use crate::Description;
use crate::package::{BindingName, InstalledPackage};
use std::collections::BTreeMap;

pub type ExportMap = BTreeMap<String, BindingName>;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImportBinding {
    pub local: String,
    pub remote: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ImportSpec {
    All {
        package: String,
        except: Vec<String>,
    },
    From {
        package: String,
        bindings: Vec<ImportBinding>,
    },
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct S3Registration {
    pub generic: String,
    pub class: String,
    pub method: String,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct NativeComponent {
    pub name: String,
    pub callbacks: Vec<String>,
    pub opaque_r_lookup: bool,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct ResourceInfo {
    pub path: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct LifecycleMetadata {
    pub on_load: bool,
}

#[derive(Clone, Debug)]
pub struct PackageIndex {
    pub package: InstalledPackage,
    pub description: Description,
    pub exports: ExportMap,
    pub imports: Vec<ImportSpec>,
    pub s3: Vec<S3Registration>,
    pub dynlibs: Vec<NativeComponent>,
    pub lifecycle: LifecycleMetadata,
    pub binding_names: Vec<BindingName>,
    pub datasets: Vec<String>,
    pub resources: Vec<ResourceInfo>,
    pub has_sysdata: bool,
}

impl PackageIndex {
    pub fn import_from(&self, local: &str) -> Option<(&str, &str)> {
        self.imports.iter().find_map(|import| match import {
            ImportSpec::From { package, bindings } => bindings
                .iter()
                .find(|binding| binding.local == local)
                .map(|binding| (package.as_str(), binding.remote.as_str())),
            ImportSpec::All { .. } => None,
        })
    }
}
