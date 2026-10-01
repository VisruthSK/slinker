use crate::Description;
use crate::package::{
    BindingName, ClassName, ComponentName, DataSetName, DatasetName, ExportName, GenericName,
    PackageIdentity, PackageName, ResourcePath, SymbolName,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub type ExportMap = BTreeMap<ExportName, BindingName>;

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
    pub callbacks: Vec<BindingName>,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub struct NativeRoutineSummary {
    pub selector: SymbolName,
    #[serde(default)]
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
    pub binding: BindingName,
    pub symbol: SymbolName,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub struct NativeComponent {
    pub name: ComponentName,
    pub alias: String,
    pub registration: Option<NativeRegistration>,
    pub symbols: Vec<NativeSymbolBinding>,
    pub library: NativeLibrary,
    pub safety: NativeSafety,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub enum NativeLibrary {
    Missing,
    Unloadable {
        library: ResourcePath,
        error: String,
    },
    Loaded {
        library: ResourcePath,
        routines: NativeRoutines,
        name_lookup: NameLookup,
    },
}

impl NativeLibrary {
    pub fn path(&self) -> Option<&ResourcePath> {
        match self {
            Self::Missing => None,
            Self::Unloadable { library, .. } | Self::Loaded { library, .. } => Some(library),
        }
    }

    pub fn routines(&self) -> Option<&NativeRoutines> {
        match self {
            Self::Loaded { routines, .. } => Some(routines),
            Self::Missing | Self::Unloadable { .. } => None,
        }
    }

    pub fn name_lookup(&self) -> Option<NameLookup> {
        match self {
            Self::Loaded { name_lookup, .. } => Some(*name_lookup),
            Self::Missing | Self::Unloadable { .. } => None,
        }
    }
}

#[derive(Clone, Debug, Default, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub struct NativeRoutines {
    pub c: Vec<SymbolName>,
    pub call: Vec<SymbolName>,
    pub fortran: Vec<SymbolName>,
    pub external: Vec<SymbolName>,
}

impl NativeRoutines {
    pub fn of(&self, interface: NativeInterface) -> &[SymbolName] {
        match interface {
            NativeInterface::C => &self.c,
            NativeInterface::Call => &self.call,
            NativeInterface::Fortran => &self.fortran,
            NativeInterface::External => &self.external,
        }
    }

    pub fn names(&self) -> BTreeSet<&str> {
        [&self.c, &self.call, &self.fortran, &self.external]
            .into_iter()
            .flatten()
            .map(SymbolName::as_str)
            .collect()
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub enum NativeInterface {
    C,
    Call,
    Fortran,
    External,
}

impl NativeInterface {
    pub fn of_callee(callee: &str) -> Option<Self> {
        match callee {
            ".C" => Some(Self::C),
            ".Call" => Some(Self::Call),
            ".Fortran" => Some(Self::Fortran),
            ".External" => Some(Self::External),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub enum NameLookup {
    Forced,
    Allowed,
}

impl NativeComponent {
    pub fn bindings(&self) -> impl Iterator<Item = NativeSymbolBinding> + '_ {
        let registered = self.registration.iter().flat_map(|fixes| {
            self.library
                .routines()
                .into_iter()
                .flat_map(NativeRoutines::names)
                .map(move |routine| NativeSymbolBinding {
                    binding: format!("{}{routine}{}", fixes.prefix, fixes.suffix).into(),
                    symbol: routine.into(),
                })
        });
        self.symbols.iter().cloned().chain(registered)
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct LifecycleMetadata {
    pub on_load: bool,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PackageData {
    sets: BTreeMap<DataSetName, Vec<DatasetName>>,
    file_backed: bool,
}

impl PackageData {
    pub fn new(sets: BTreeMap<DataSetName, Vec<DatasetName>>, file_backed: bool) -> Self {
        Self { sets, file_backed }
    }

    pub fn set(&self, name: &str) -> Option<&[DatasetName]> {
        self.sets.get(name).map(Vec::as_slice)
    }

    pub fn defines(&self, object: &str) -> bool {
        self.sets
            .values()
            .any(|objects| objects.iter().any(|candidate| candidate == object))
    }

    pub fn is_file_backed(&self) -> bool {
        self.file_backed
    }
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
    pub data: PackageData,
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
