use crate::Description;
use crate::package::{BindingName, ClassName, GenericName, PackageIdentity, PackageName};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub type ExportMap = BTreeMap<String, BindingName>;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ImportBinding {
    pub local: BindingName,
    pub remote: BindingName,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum ImportSpec {
    All {
        package: PackageName,
        except: Vec<BindingName>,
    },
    From {
        package: PackageName,
        bindings: Vec<ImportBinding>,
    },
}

#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub struct S3Registration {
    pub generic: GenericSpec,
    pub class: ClassName,
    pub method: BindingName,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub struct GenericSpec {
    pub package: Option<PackageName>,
    pub name: GenericName,
}

impl std::fmt::Display for GenericSpec {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Some(package) = &self.package {
            write!(formatter, "{package}::{}", self.name)
        } else {
            formatter.write_str(&self.name)
        }
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub struct NativeFacts {
    /// Statically named R bindings called by the component regardless of call
    /// site. Retained for summaries that truly have fixed callbacks.
    pub callbacks: Vec<String>,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub struct NativeRoutineSummary {
    /// Registered R-side binding or native routine symbol used as the selector.
    pub selector: String,
    /// One-based native routine argument positions that are invoked as R
    /// callables. The `.Call`/`.External` selector itself is not counted.
    pub callback_arguments: Vec<usize>,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub enum NativeSafety {
    Unanalyzed,
    Safe(NativeFacts),
    Summarized(Vec<NativeRoutineSummary>),
    Unsupported(Vec<String>),
}

#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub struct NativeRegistration {
    pub prefix: String,
    pub suffix: String,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub struct NativeSymbolBinding {
    pub binding: String,
    pub symbol: String,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub struct NativeComponent {
    pub name: String,
    pub registration: Option<NativeRegistration>,
    pub symbols: Vec<NativeSymbolBinding>,
    pub routines: Vec<String>,
    pub library: Option<String>,
    pub safety: NativeSafety,
}

impl NativeComponent {
    pub fn bindings(&self) -> impl Iterator<Item = NativeSymbolBinding> + '_ {
        let registered = self.registration.iter().flat_map(|fixes| {
            self.routines
                .iter()
                .map(move |routine| NativeSymbolBinding {
                    binding: format!("{}{routine}{}", fixes.prefix, fixes.suffix),
                    symbol: routine.clone(),
                })
        });
        self.symbols.iter().cloned().chain(registered)
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct LifecycleMetadata {
    pub on_load: bool,
}

#[derive(Clone, Debug)]
pub struct PackageIndex {
    pub identity: PackageIdentity,
    pub description: Description,
    pub exports: ExportMap,
    pub imports: Vec<ImportSpec>,
    pub s3: Vec<S3Registration>,
    pub dynlibs: Vec<NativeComponent>,
    pub lifecycle: LifecycleMetadata,
    pub binding_names: Vec<BindingName>,
    pub datasets: Vec<String>,
    /// Installed package-relative files/directories. These are metadata for
    /// resolving resource operations; they are not retained semantic resources.
    pub files: Vec<String>,
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
