use super::state::{AnalyzerState, ParsedSite};
use crate::analysis::{NodeId, RejectCode};
use crate::metadata::{RelationField, relations};
use crate::package::{ImportSpec, PackageId, PackageImage, PackageName, PackageProvider};
use crate::syntax::{PackageGuard, Span};
use crate::{Error, Result};
use std::collections::HashSet;
use std::sync::Arc;

impl<P: PackageProvider> AnalyzerState<P> {
    pub(super) fn guards_active(
        &mut self,
        site: ParsedSite<'_>,
        guards: &[PackageGuard],
        span: &Span,
    ) -> Result<bool> {
        match self.guard_verdict(site.package, site.image, guards)? {
            GuardVerdict::Active => Ok(true),
            GuardVerdict::Pruned => Ok(false),
            GuardVerdict::PrunedByUnselectedOptional(optional) => {
                self.optional_availability_blocker(
                    site.node,
                    site.package,
                    site.binding,
                    &optional,
                    span,
                );
                Ok(false)
            }
        }
    }

    pub(super) fn optional_availability_blocker(
        &mut self,
        from: NodeId,
        current: PackageId,
        binding: &str,
        optional: &str,
        span: &Span,
    ) {
        self.diagnostic(
            from,
            current,
            Some(binding),
            RejectCode::OptionalAvailability,
            format!(
                "reachable code depends on whether unselected optional package `{optional}` is installed; `{}` lists it only in Suggests, so the build cannot fix either answer (select it with --link or --external)",
                self.packages.name(current)
            ),
            Some(span.clone()),
        );
    }

    pub(super) fn guard_verdict(
        &mut self,
        owner: PackageId,
        image: &PackageImage,
        guards: &[PackageGuard],
    ) -> Result<GuardVerdict> {
        if guards.is_empty() {
            return Ok(GuardVerdict::Active);
        }

        let helper_shadowed = |helper: &str, image: &PackageImage| {
            image.bindings.contains_key(helper) || image.index.import_from(helper).is_some()
        };
        if guards.iter().any(|guard| {
            let helper = match guard {
                PackageGuard::Available(_) => "requireNamespace",
                PackageGuard::Loaded(_) => "isNamespaceLoaded",
                PackageGuard::Selected(_) => return false,
            };
            helper_shadowed(helper, image)
        }) {
            return Ok(GuardVerdict::Active);
        }

        for guard in guards {
            let package = guard.package();
            if self.optional_package_selected(package) {
                continue;
            }
            if self.package_is_suggested_only(owner, package)? {
                return Ok(GuardVerdict::PrunedByUnselectedOptional(package.clone()));
            }
            let imported = image.index.imports.iter().any(|import| match import {
                ImportSpec::All {
                    package: imported, ..
                }
                | ImportSpec::From {
                    package: imported, ..
                } => imported == package,
            });
            match guard {
                PackageGuard::Selected(_) => return Ok(GuardVerdict::Pruned),
                PackageGuard::Loaded(_) => {
                    if !imported {
                        return Ok(GuardVerdict::Pruned);
                    }
                }
                PackageGuard::Available(_) => {
                    if imported {
                        continue;
                    }
                    match self.packages.resolve(package)? {
                        Some(candidate) if self.packages.is_external(candidate) => {
                            self.external.insert(candidate);
                        }
                        Some(_) if self.package_is_required(owner, package)? => {}
                        _ => return Ok(GuardVerdict::Pruned),
                    }
                }
            }
        }
        Ok(GuardVerdict::Active)
    }

    pub(super) fn optional_package_selected(&self, name: &str) -> bool {
        self.linked_packages.contains(name) || self.explicit_external_packages.contains(name)
    }

    pub(super) fn package_is_suggested_only(
        &mut self,
        package: PackageId,
        name: &str,
    ) -> Result<bool> {
        Ok(self
            .declared_dependencies(package)?
            .suggested_only
            .contains(name))
    }

    pub(super) fn package_is_required(&mut self, package: PackageId, name: &str) -> Result<bool> {
        Ok(self.declared_dependencies(package)?.required.contains(name))
    }

    pub(super) fn declared_dependencies(
        &mut self,
        package: PackageId,
    ) -> Result<&DeclaredDependencies> {
        if !self.declared_dependencies.contains_key(&package) {
            let index = Arc::clone(&self.image(package)?.index);
            let mut required = HashSet::new();
            for import in &index.imports {
                let package = match import {
                    ImportSpec::All { package, .. } | ImportSpec::From { package, .. } => package,
                };
                required.insert(package.to_string());
            }
            for dependency in relations(&index.description, RelationField::Imports)
                .map_err(Error::Analysis)?
                .into_iter()
                .chain(
                    relations(&index.description, RelationField::Depends)
                        .map_err(Error::Analysis)?,
                )
            {
                required.insert(dependency.package().to_owned());
            }
            let suggested_only = relations(&index.description, RelationField::Suggests)
                .map_err(Error::Analysis)?
                .into_iter()
                .filter_map(|dependency| {
                    let name = dependency.package().to_owned();
                    (!required.contains(&name)).then_some(name)
                })
                .collect::<HashSet<_>>();
            self.declared_dependencies.insert(
                package,
                DeclaredDependencies {
                    required,
                    suggested_only,
                },
            );
        }
        Ok(&self.declared_dependencies[&package])
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum GuardVerdict {
    Active,
    Pruned,
    PrunedByUnselectedOptional(PackageName),
}

pub(super) struct DeclaredDependencies {
    required: HashSet<String>,
    suggested_only: HashSet<String>,
}
