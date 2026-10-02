use super::invocation::PinnedUse;
use super::relocation::PendingRelocation;
use super::s3::CallableId;
use super::state::{AnalyzerState, Caller, ParsedSite};
use crate::Result;
use crate::analysis::{EdgeKind, Need, NodeId, RejectCode};
use crate::ir::ExternalBindingAccess;
use crate::package::PackageRole;
use crate::package::{PackageId, PackageProvider};
use crate::syntax::PackageRef;
use crate::syntax::ResourceRef;
use crate::syntax::{CallSite, ParsedExpression, ParsedRFile, ResourcePackage};

impl<P: PackageProvider> AnalyzerState<P> {
    pub(super) fn namespace_access(
        &self,
        from: NodeId,
        current: PackageId,
        reference: &PackageRef,
    ) -> Result<()> {
        if reference.package == "base" {
            return Ok(());
        }
        if self.package_is_suggested_only(current, &reference.package)?
            && !self.optional_package_selected(&reference.package)
        {
            return Ok(());
        }
        let Some(foreign) = self.packages.resolve(&reference.package)? else {
            self.record_missing_package(
                from,
                current,
                &reference.package,
                EdgeKind::PackageQualified,
                format!(
                    "reachable {}{}{} access",
                    reference.package,
                    if reference.internal { ":::" } else { "::" },
                    reference.symbol
                ),
                Some(reference.span.clone()),
            );
            return Ok(());
        };
        if self.is_root(current) && foreign == current {
            let binding = if reference.internal {
                reference.symbol.clone()
            } else {
                let index = self.packages.index(foreign)?;
                index
                    .exports
                    .get(reference.symbol.as_str())
                    .cloned()
                    .unwrap_or_else(|| reference.symbol.clone())
            };
            self.require_at(
                from,
                Need::Binding {
                    package: current,
                    binding: binding.clone(),
                },
                EdgeKind::PackageQualified,
                format!(
                    "root self access {}{}{} resolves to `{binding}`",
                    reference.package,
                    if reference.internal { ":::" } else { "::" },
                    reference.symbol
                ),
                Some(reference.span.clone()),
            );
            return Ok(());
        }
        if self.packages.is_external(foreign) {
            self.external.lock().insert(foreign);
            let external = self.external_binding(
                foreign,
                &reference.symbol,
                if reference.internal {
                    ExternalBindingAccess::Internal
                } else {
                    ExternalBindingAccess::Exported
                },
                Some(reference.span.clone()),
            );
            self.depend(
                from,
                external,
                EdgeKind::PackageQualified,
                format!(
                    "{} access to External binding",
                    if reference.internal { ":::" } else { "::" }
                ),
                Some(reference.span.clone()),
            );
            return Ok(());
        }
        let index = self.packages.index(foreign)?;
        if !reference.internal
            && !index.exports.contains_key(reference.symbol.as_str())
            && index.data.defines(&reference.symbol)
        {
            self.dataset_access(from, foreign, reference);
            return Ok(());
        }
        let binding = if reference.internal {
            reference.symbol.clone()
        } else {
            index
                .exports
                .get(reference.symbol.as_str())
                .cloned()
                .unwrap_or_else(|| reference.symbol.clone())
        };
        self.require_at(
            from,
            Need::Activation { package: foreign },
            EdgeKind::NamespaceLoad,
            "qualified namespace access requires activation",
            Some(reference.span.clone()),
        );
        self.require_at(
            from,
            Need::Binding {
                package: foreign,
                binding: binding.clone(),
            },
            EdgeKind::PackageQualified,
            format!(
                "{}{}{} resolves to `{binding}`",
                reference.package,
                if reference.internal { ":::" } else { "::" },
                reference.symbol
            ),
            Some(reference.span.clone()),
        );
        self.relocations
            .lock()
            .push(PendingRelocation::NamespaceAccess {
                source: reference.span.clone(),
                package: foreign,
                binding,
                internal: reference.internal,
            });
        Ok(())
    }

    pub(super) fn resource_access(
        &self,
        site: ParsedSite<'_>,
        parsed: &ParsedRFile,
        expression: &ParsedExpression,
        resource: &ResourceRef,
    ) -> Result<()> {
        let (from, current) = (site.node, site.package);
        let package_name = match &resource.package {
            ResourcePackage::Literal(name) => name.clone(),
            ResourcePackage::Computed(binding) => {
                let declared = binding
                    .as_ref()
                    .and_then(|binding| parsed.string_domain_for(binding, resource.scope));
                let Some(names) = declared else {
                    let pinned = binding.as_ref().and_then(|binding| {
                        expression
                            .pinned_defaults
                            .iter()
                            .find(|pinned| pinned.name == binding.name)
                    });
                    let Some(pinned) = pinned else {
                        self.relocations.lock().defer_dynamic_resource_lookup(
                            from,
                            current,
                            resource.span.clone(),
                        );
                        return Ok(());
                    };
                    self.invocations.lock().pin_default(PinnedUse {
                        node: from,
                        package: current,
                        callable: CallableId {
                            package: current,
                            binding: site.binding.into(),
                        },
                        formals: expression.parameters.clone(),
                        formal: pinned.name.clone(),
                        value: pinned.value.clone(),
                        span: resource.span.clone(),
                    });
                    let names_linked = self
                        .known_package(&pinned.value)
                        .is_some_and(|package| self.packages.role(package) == PackageRole::Linked);
                    if !names_linked {
                        return Ok(());
                    }
                    return self.literal_resource_access(from, current, resource, &pinned.value);
                };
                for name in names {
                    let linked = self
                        .resource_package(from, current, resource, &name)?
                        .is_some_and(|package| self.packages.role(package) == PackageRole::Linked);
                    if linked {
                        self.diagnostic(
                            from,
                            current,
                            None,
                            RejectCode::DynamicLookup,
                            format!(
                                "system.file() can name Linked `{name}` through a declared computed package, whose installation slinker removes"
                            ),
                            Some(resource.span.clone()),
                        );
                    }
                }
                return Ok(());
            }
        };
        self.literal_resource_access(from, current, resource, &package_name)
    }

    fn literal_resource_access(
        &self,
        from: NodeId,
        current: PackageId,
        resource: &ResourceRef,
        package_name: &str,
    ) -> Result<()> {
        let Some(foreign) = self.resource_package(from, current, resource, package_name)? else {
            return Ok(());
        };
        let Some(path) = &resource.path else {
            self.diagnostic(
                from,
                current,
                None,
                RejectCode::DynamicLookup,
                format!("dynamic system.file() path for package {package_name}"),
                Some(resource.span.clone()),
            );
            return Ok(());
        };
        if !self.packages.resource_exists(foreign, path)? {
            match resource.must_work {
                Some(false) => return Ok(()),
                Some(true) => {
                    self.diagnostic(
                        from,
                        current,
                        None,
                        RejectCode::MissingResource,
                        format!("system.file(..., mustWork = TRUE) requires absent path {package_name}/{path}"),
                        Some(resource.span.clone()),
                    );
                    return Ok(());
                }
                None => {
                    self.diagnostic(
                        from,
                        current,
                        None,
                        RejectCode::DynamicLookup,
                        format!("dynamic mustWork controls absent system.file path {package_name}/{path}"),
                        Some(resource.span.clone()),
                    );
                    return Ok(());
                }
            }
        }
        self.require_at(
            from,
            Need::Resource {
                package: foreign,
                resource: path.clone().into(),
            },
            EdgeKind::Resource,
            format!("system.file requires {package_name}/{path}"),
            Some(resource.span.clone()),
        );
        self.relocations
            .lock()
            .push(PendingRelocation::ResourceAccess {
                source: resource.span.clone(),
                package: foreign,
                resource: path.into(),
            });
        Ok(())
    }

    fn resource_package(
        &self,
        from: NodeId,
        current: PackageId,
        resource: &ResourceRef,
        name: &str,
    ) -> Result<Option<PackageId>> {
        if self.is_root(current) && name == self.packages.name(current) {
            return Ok(None);
        }
        if self.package_is_suggested_only(current, name)? && !self.optional_package_selected(name) {
            return Ok(None);
        }
        let Some(foreign) = self.packages.resolve(name)? else {
            self.record_missing_package(
                from,
                current,
                name,
                EdgeKind::Resource,
                format!("system.file references package {name}"),
                Some(resource.span.clone()),
            );
            return Ok(None);
        };
        if self.packages.is_external(foreign) {
            self.external.lock().insert(foreign);
            return Ok(None);
        }
        Ok(Some(foreign))
    }

    pub(super) fn dynamic_package_name(
        &self,
        Caller {
            node: from,
            package: current,
            binding,
        }: Caller<'_>,
        call: &CallSite,
    ) {
        self.diagnostic(
            from,
            current,
            Some(binding),
            RejectCode::DynamicPackageDiscovery,
            format!(
                "{}() with a dynamic package name can name a Linked package",
                call.callee
            ),
            Some(call.span.clone()),
        );
    }

    pub(super) fn unrewritable_package_call(
        &self,
        from: NodeId,
        current: PackageId,
        call: &CallSite,
        name: &str,
    ) {
        self.diagnostic(
            from,
            current,
            None,
            RejectCode::UnsupportedRootTransformation,
            format!(
                "{}() on Linked `{name}` has no equivalent on its private namespace",
                call.callee
            ),
            Some(call.span.clone()),
        );
    }

    pub(super) fn missing_package_call(
        &self,
        from: NodeId,
        current: PackageId,
        call: &CallSite,
        name: &str,
    ) {
        self.record_missing_package(
            from,
            current,
            name,
            EdgeKind::Discovery,
            format!("{} requires unavailable package {name}", call.callee),
            Some(call.span.clone()),
        );
    }

    pub(super) fn discovered_package(
        &self,
        from: NodeId,
        current: PackageId,
        call: &CallSite,
        name: &str,
    ) -> Result<Discovered> {
        if name == self.packages.name(current) {
            return Ok(if self.is_root(current) {
                Discovered::Settled
            } else {
                Discovered::Linked(current)
            });
        }
        let selected = self.optional_package_selected(name);
        if !selected && self.package_is_suggested_only(current, name)? {
            return Ok(Discovered::Optional);
        }
        let Some(target) = self.packages.resolve(name)? else {
            return Ok(if selected {
                self.record_missing_package(
                    from,
                    current,
                    name,
                    EdgeKind::Discovery,
                    format!("{} requires unavailable package {name}", call.callee),
                    Some(call.span.clone()),
                );
                Discovered::Settled
            } else {
                Discovered::Missing
            });
        };
        if self.packages.is_external(target) {
            self.external.lock().insert(target);
            return Ok(Discovered::Settled);
        }
        if self.is_root(target) {
            return Ok(Discovered::Settled);
        }
        if !selected && !self.package_is_required(current, name)? {
            self.diagnostic(
                from,
                current,
                None,
                RejectCode::DynamicPackageDiscovery,
                format!(
                    "{}() names `{name}`, which is installed but not a declared dependency of `{}`",
                    call.callee,
                    self.packages.name(current)
                ),
                Some(call.span.clone()),
            );
            return Ok(Discovered::Settled);
        }
        Ok(Discovered::Linked(target))
    }
}

pub(super) enum Discovered {
    Linked(PackageId),
    Settled,
    Optional,
    Missing,
}
