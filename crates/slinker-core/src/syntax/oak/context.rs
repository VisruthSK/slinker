use crate::package::{BindingName, ExportMap, PackageName};
use oak_semantic::{EffectsHandlers, ImportsResolver, SourceResolution};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ExternalNameOrigin {
    Base,
    Imported {
        package: PackageName,
        name: BindingName,
    },
    Shadowed,
    UnknownImportAll,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum NamespaceImportResolution {
    Imported {
        package: PackageName,
        binding: BindingName,
        effect_name: BindingName,
    },
    MissingImportAll {
        package: PackageName,
        binding: BindingName,
    },
    BaseFallback,
}

pub(super) const IMPORT_ALL_EXCLUDED: [&str; 10] = [
    ".__NAMESPACE__.",
    ".__S3MethodsTable__.",
    ".packageName",
    ".First.lib",
    ".Last.lib",
    ".onLoad",
    ".onAttach",
    ".onDetach",
    ".conflicts.OK",
    ".noGenerics",
];

#[derive(Debug, Clone)]
pub(super) enum NamespaceImport {
    From {
        package: PackageName,
        bindings: Vec<(BindingName, BindingName)>,
    },
    All {
        package: PackageName,
        exports: Option<ExportMap>,
        except: BTreeSet<BindingName>,
    },
}

impl NamespaceImport {
    pub(super) fn imports_from_all(except: &BTreeSet<BindingName>, name: &str) -> bool {
        !except.contains(name) && !IMPORT_ALL_EXCLUDED.contains(&name)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ImportedBinding {
    pub(crate) package: PackageName,
    pub(crate) binding: BindingName,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ImportRecord {
    pub(crate) package: PackageName,
    pub(crate) names: Vec<(BindingName, BindingName)>,
}

#[derive(Debug, Default, Clone)]
pub(crate) struct NamespaceImports {
    pub(super) imports: Vec<NamespaceImport>,
}

impl NamespaceImports {
    pub(crate) fn add_import_from(
        &mut self,
        package: PackageName,
        bindings: impl IntoIterator<Item = (BindingName, BindingName)>,
    ) {
        self.imports.push(NamespaceImport::From {
            package,
            bindings: bindings.into_iter().collect(),
        });
    }

    pub(crate) fn add_import_all(
        &mut self,
        package: PackageName,
        exports: Option<ExportMap>,
        except: impl IntoIterator<Item = BindingName>,
    ) {
        self.imports.push(NamespaceImport::All {
            package,
            exports,
            except: except.into_iter().collect(),
        });
    }

    pub(crate) fn resolve(&self, name: &str) -> NamespaceImportResolution {
        for import in self.imports.iter().rev() {
            match import {
                NamespaceImport::From { package, bindings } => {
                    if let Some((_, remote)) =
                        bindings.iter().rev().find(|(local, _)| local == name)
                    {
                        return NamespaceImportResolution::Imported {
                            package: package.clone(),
                            binding: remote.clone(),
                            effect_name: remote.clone(),
                        };
                    }
                }
                NamespaceImport::All {
                    package,
                    exports,
                    except,
                } => {
                    if !NamespaceImport::imports_from_all(except, name) {
                        continue;
                    }
                    let Some(exports) = exports else {
                        return NamespaceImportResolution::MissingImportAll {
                            package: package.clone(),
                            binding: BindingName::from(name),
                        };
                    };
                    let Some(binding) = exports.get(name) else {
                        continue;
                    };
                    return NamespaceImportResolution::Imported {
                        package: package.clone(),
                        binding: binding.clone(),
                        effect_name: BindingName::from(name),
                    };
                }
            }
        }

        NamespaceImportResolution::BaseFallback
    }

    pub(crate) fn names(
        &self,
    ) -> std::result::Result<BTreeMap<BindingName, ImportedBinding>, &PackageName> {
        let mut names = BTreeMap::new();
        for import in &self.imports {
            match import {
                NamespaceImport::From { package, bindings } => {
                    for (local, remote) in bindings {
                        names.insert(
                            local.clone(),
                            ImportedBinding {
                                package: package.clone(),
                                binding: remote.clone(),
                            },
                        );
                    }
                }
                NamespaceImport::All {
                    package,
                    exports,
                    except,
                } => {
                    let exports = exports.as_ref().ok_or(package)?;
                    for (name, binding) in exports {
                        if NamespaceImport::imports_from_all(except, name) {
                            names.insert(
                                BindingName::from(name.as_str()),
                                ImportedBinding {
                                    package: package.clone(),
                                    binding: binding.clone(),
                                },
                            );
                        }
                    }
                }
            }
        }
        Ok(names)
    }

    pub(crate) fn records(&self) -> std::result::Result<Vec<ImportRecord>, &PackageName> {
        self.imports
            .iter()
            .map(|import| match import {
                NamespaceImport::From { package, bindings } => Ok(ImportRecord {
                    package: package.clone(),
                    names: bindings.clone(),
                }),
                NamespaceImport::All {
                    package,
                    exports,
                    except,
                } => {
                    let exports = exports.as_ref().ok_or(package)?;
                    Ok(ImportRecord {
                        package: package.clone(),
                        names: exports
                            .keys()
                            .filter(|name| NamespaceImport::imports_from_all(except, name))
                            .map(|name| {
                                (
                                    BindingName::from(name.as_str()),
                                    BindingName::from(name.as_str()),
                                )
                            })
                            .collect(),
                    })
                }
            })
            .collect()
    }
}

#[derive(Debug, Default, Clone)]
pub(crate) struct SharedNames(Arc<BTreeSet<BindingName>>);

impl SharedNames {
    pub(crate) fn contains(&self, name: &str) -> bool {
        self.0.contains(name)
    }

    #[cfg(test)]
    pub(super) fn insert(&mut self, name: BindingName) {
        Arc::make_mut(&mut self.0).insert(name);
    }
}

impl From<Arc<BTreeSet<BindingName>>> for SharedNames {
    fn from(names: Arc<BTreeSet<BindingName>>) -> Self {
        Self(names)
    }
}

impl From<BTreeSet<BindingName>> for SharedNames {
    fn from(names: BTreeSet<BindingName>) -> Self {
        Self(Arc::new(names))
    }
}

#[derive(Debug, Default, Clone)]
pub struct OakParseContext {
    pub(super) shadowed_names: SharedNames,
    pub(super) imports: Arc<NamespaceImports>,
    pub(super) non_returning_names: SharedNames,
}

impl OakParseContext {
    pub fn new(shadowed_names: BTreeSet<BindingName>) -> Self {
        Self {
            shadowed_names: shadowed_names.into(),
            imports: Arc::default(),
            non_returning_names: SharedNames::default(),
        }
    }

    pub(crate) fn with_imports(
        shadowed_names: SharedNames,
        imports: Arc<NamespaceImports>,
        non_returning_names: SharedNames,
    ) -> Self {
        Self {
            shadowed_names,
            imports,
            non_returning_names,
        }
    }

    #[cfg(test)]
    pub(super) fn add_import_from(
        &mut self,
        local: impl Into<BindingName>,
        package: impl Into<PackageName>,
        remote: impl Into<BindingName>,
    ) {
        Arc::make_mut(&mut self.imports)
            .add_import_from(package.into(), [(local.into(), remote.into())]);
    }

    pub(super) fn origin(&self, name: &str) -> ExternalNameOrigin {
        if self.shadowed_names.contains(name) {
            return ExternalNameOrigin::Shadowed;
        }
        match self.imports.resolve(name) {
            NamespaceImportResolution::Imported {
                package,
                effect_name,
                ..
            } => ExternalNameOrigin::Imported {
                package,
                name: effect_name,
            },
            NamespaceImportResolution::MissingImportAll { .. } => {
                ExternalNameOrigin::UnknownImportAll
            }
            NamespaceImportResolution::BaseFallback => ExternalNameOrigin::Base,
        }
    }

    pub(super) fn resolves_to_base(&self, name: &str) -> bool {
        matches!(self.origin(name), ExternalNameOrigin::Base)
    }
}

pub(super) struct SlinkerImportsResolver<'a> {
    pub(super) context: &'a OakParseContext,
}

impl ImportsResolver for SlinkerImportsResolver<'_> {
    fn resolve_source(&mut self, _path: &str) -> Option<SourceResolution> {
        None
    }

    fn resolve_effects(&mut self, name: &str, attached: &[String]) -> Option<EffectsHandlers> {
        if !attached.is_empty() {
            return None;
        }

        match self.context.origin(name) {
            ExternalNameOrigin::Base => oak_semantic::effects::lookup("base", name).copied(),
            ExternalNameOrigin::Imported { package, name } => {
                oak_semantic::effects::lookup(&package, &name).copied()
            }
            ExternalNameOrigin::Shadowed | ExternalNameOrigin::UnknownImportAll => None,
        }
    }

    fn resolve_qualified_effects(&mut self, package: &str, name: &str) -> Option<EffectsHandlers> {
        oak_semantic::effects::lookup(package, name).copied()
    }

    fn package_exists(&mut self, _package: &str) -> bool {
        true
    }
}
