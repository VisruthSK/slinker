use super::object_world::{ClosureId, ClosureObject};
use super::state::AnalyzerState;
use crate::Result;
use crate::analysis::{EdgeKind, Need, NodeId, NodeKind, RejectCode};
use crate::package::{ImportSpec, PackageId, PackageImage, PackageProvider};
use crate::syntax::{
    NamespaceImportResolution, NamespaceImports, OakParseContext, Span,
    closure_definitely_non_returning,
};
use std::collections::{BTreeMap, BTreeSet, HashSet};

/// Analysis-only outcome of resolving one name: a single exact target, or a typed reason the
/// target is open. Finalization lowers every static answer, so none survives into `ProgramIr`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum Resolution<T> {
    Static(T),
    OpenDynamic(OpenReason),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum OpenReason {
    Unresolved(String),
    MissingPackage {
        package: String,
        binding: Option<String>,
    },
}

/// What a lexical, imported, or qualified name denotes inside the analyzed universe.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum BindingTarget {
    Local,
    Base,
    Namespace {
        package: PackageId,
        binding: String,
    },
    Imported {
        package: PackageId,
        binding: String,
    },
    Private {
        package: PackageId,
        environment: String,
        binding: String,
    },
    Closure {
        package: PackageId,
        closure: ClosureId,
    },
    Native {
        package: PackageId,
        component: String,
        binding: String,
    },
    External {
        package: PackageId,
        binding: String,
    },
    Metadata {
        package: PackageId,
        name: String,
    },
}

impl<P: PackageProvider> AnalyzerState<P> {
    pub(super) fn namespace_imports(
        &mut self,
        package: PackageId,
        image: &PackageImage,
    ) -> Result<NamespaceImports> {
        if let Some(imports) = self.namespace_imports.get(&package) {
            return Ok(imports.clone());
        }

        let mut imports = NamespaceImports::default();
        for import in &image.index.imports {
            if let ImportSpec::From { package, bindings } = import {
                for binding in bindings {
                    imports.add_import_from(&binding.local, package, &binding.remote);
                }
            }
        }

        for import in &image.index.imports {
            let ImportSpec::All {
                package: package_name,
                except,
            } = import
            else {
                continue;
            };
            let exports = match self.packages.resolve(package_name)? {
                Some(foreign) => Some(self.packages.index(foreign)?.exports.clone()),
                None => None,
            };
            imports.add_import_all(package_name, exports, except.iter().cloned());
        }

        self.namespace_imports.insert(package, imports.clone());
        Ok(imports)
    }

    pub(super) fn inferred_non_returning_bindings(
        &mut self,
        package: PackageId,
        image: &PackageImage,
        imports: &NamespaceImports,
    ) -> BTreeSet<String> {
        if let Some(bindings) = self.non_returning_bindings.get(&package) {
            return bindings.clone();
        }

        let mut namespace_shadowed = BTreeSet::new();
        namespace_shadowed.extend(image.bindings.keys().cloned());
        namespace_shadowed.extend(self.namespace_builders[&package].bindings.iter().cloned());
        for component in &image.index.dynlibs {
            namespace_shadowed.extend(component.bindings().map(|symbol| symbol.binding));
        }

        let namespace_environment = format!("namespace:{}", self.packages.name(package));
        let mut proven = BTreeSet::new();
        loop {
            let context = OakParseContext::with_imports(
                namespace_shadowed.clone(),
                imports.clone(),
                proven.clone(),
            );
            let before = proven.len();
            for (name, binding) in &image.bindings {
                if proven.contains(name) {
                    continue;
                }
                let Some(closure) = &binding.closure else {
                    continue;
                };
                if closure.environment != namespace_environment {
                    continue;
                }
                if closure_definitely_non_returning(closure.source.as_ref(), &context) {
                    proven.insert(name.clone());
                }
            }
            if proven.len() == before {
                break;
            }
        }

        self.non_returning_bindings.insert(package, proven.clone());
        proven
    }

    pub(super) fn oak_parse_context(
        &mut self,
        package: PackageId,
        image: &PackageImage,
        lexical_environment: &str,
    ) -> Result<OakParseContext> {
        let mut shadowed = BTreeSet::new();
        shadowed.extend(image.index.binding_names.iter().cloned());
        shadowed.extend(self.namespace_builders[&package].bindings.iter().cloned());
        for component in &image.index.dynlibs {
            shadowed.extend(component.bindings().map(|symbol| symbol.binding));
        }

        let mut private_shadowed = BTreeSet::new();
        let mut visible_private = BTreeMap::new();
        let mut environment = lexical_environment.to_owned();
        if let Some(graph) = self.objects.get(package)
            && let Some(mut environment_id) = graph.environment_id(&environment)
        {
            let mut seen = BTreeSet::new();
            while seen.insert(environment_id) {
                let shape = graph.environment(environment_id);
                if !shape.derived {
                    environment = shape.label.clone();
                    break;
                }
                for name in shape.bindings.keys() {
                    private_shadowed.insert(name.clone());
                    shadowed.insert(name.clone());
                }
                let Some(parent) = shape.parent else {
                    environment.clear();
                    break;
                };
                environment_id = parent;
            }
        }
        let mut seen = HashSet::new();
        while seen.insert(environment.clone()) {
            let Some(private) = image.private_environment(&environment) else {
                break;
            };
            for (name, binding) in &private.bindings {
                private_shadowed.insert(name.clone());
                shadowed.insert(name.clone());
                // Walk inner-to-outer. The first binding is the one lexical
                // lookup can actually reach from this closure.
                visible_private.entry(name.clone()).or_insert(binding);
            }
            environment = private.parent.clone();
        }

        let imports = self.namespace_imports(package, image)?;
        let mut non_returning = self.inferred_non_returning_bindings(package, image, &imports);
        // A visible private binding masks a package-namespace helper with the
        // same name, so do not inherit the package summary through it.
        non_returning.retain(|name| !private_shadowed.contains(name));

        // Propagate NeverReturns through the visible private lexical chain.
        // This is a source-only fixed point; it does not parse unrelated
        // package bindings or introduce a second lexical resolver.
        loop {
            let context = OakParseContext::with_imports(
                shadowed.clone(),
                imports.clone(),
                non_returning.clone(),
            );
            let before = non_returning.len();
            for (name, binding) in &visible_private {
                if non_returning.contains(name) {
                    continue;
                }
                let Some(closure) = &binding.closure else {
                    continue;
                };
                if closure_definitely_non_returning(closure.source.as_ref(), &context) {
                    non_returning.insert(name.clone());
                }
            }
            if non_returning.len() == before {
                break;
            }
        }

        Ok(OakParseContext::with_imports(
            shadowed,
            imports,
            non_returning,
        ))
    }

    pub(super) fn private_source_key(environment: &str, binding: &str) -> String {
        format!("{environment}${binding}")
    }

    pub(super) fn closure_execution_source(
        &self,
        package: PackageId,
        closure: ClosureId,
    ) -> Option<(ClosureObject, String, String, String)> {
        let graph = self.objects.get(package)?;
        let closure = graph.closure(closure).clone();
        let environment = graph.environment(closure.enclosure).label.clone();
        let owner = match (
            &closure.provenance.namespace_binding,
            &closure.provenance.private_environment,
            &closure.provenance.private_binding,
        ) {
            (Some(binding), _, _) => binding.clone(),
            (_, Some(private), Some(binding)) => Self::private_source_key(private, binding),
            _ => "runtime".into(),
        };
        let source_key = format!("{owner}{}@{environment}", closure.provenance.path);
        Some((closure, owner, source_key, environment))
    }

    pub(super) fn resolve_lexical_name(
        &mut self,
        current: PackageId,
        image: &PackageImage,
        lexical_environment: &str,
        name: &str,
    ) -> Result<Resolution<BindingTarget>> {
        let mut environment = lexical_environment.to_owned();
        let mut seen = HashSet::new();
        loop {
            if !seen.insert(environment.clone()) {
                return Ok(Resolution::OpenDynamic(OpenReason::Unresolved(
                    name.to_owned(),
                )));
            }
            if environment.starts_with("derived:") {
                if let Some(graph) = self.objects.get(current)
                    && let Some(environment_id) = graph.environment_id(&environment)
                {
                    let (object, blocked) = graph.lookup_environment_binding(environment_id, name);
                    if let Some(object) = object {
                        let resolved = match graph.closure_of(object) {
                            Some(closure) => {
                                let closure_object = graph.closure(closure);
                                let provenance = &closure_object.provenance;
                                if closure_object.derived_from.is_some() || provenance.path != "$" {
                                    Resolution::Static(BindingTarget::Closure {
                                        package: current,
                                        closure,
                                    })
                                } else if let Some(binding) = &provenance.namespace_binding {
                                    Resolution::Static(BindingTarget::Namespace {
                                        package: current,
                                        binding: binding.clone(),
                                    })
                                } else if let (Some(environment), Some(binding)) =
                                    (&provenance.private_environment, &provenance.private_binding)
                                {
                                    Resolution::Static(BindingTarget::Private {
                                        package: current,
                                        environment: environment.clone(),
                                        binding: binding.clone(),
                                    })
                                } else {
                                    Resolution::Static(BindingTarget::Local)
                                }
                            }
                            None => Resolution::Static(BindingTarget::Local),
                        };
                        return Ok(resolved);
                    }
                    if blocked {
                        return Ok(Resolution::OpenDynamic(OpenReason::Unresolved(
                            name.to_owned(),
                        )));
                    }
                    if let Some(parent) = graph.environment(environment_id).parent {
                        environment = graph.environment(parent).label.clone();
                        continue;
                    }
                    return Ok(Resolution::OpenDynamic(OpenReason::Unresolved(
                        name.to_owned(),
                    )));
                }
                return Ok(Resolution::OpenDynamic(OpenReason::Unresolved(
                    name.to_owned(),
                )));
            }
            if let Some(private) = image.private_environment(&environment) {
                if private.bindings.contains_key(name) {
                    return Ok(Resolution::Static(BindingTarget::Private {
                        package: current,
                        environment: private.id.clone(),
                        binding: name.to_owned(),
                    }));
                }
                environment = private.parent.clone();
                continue;
            }
            if environment == format!("namespace:{}", self.packages.name(current)) {
                return self.resolve_name(current, image, name);
            }
            if let Some(namespace) = environment.strip_prefix("namespace:") {
                let Some(foreign) = self.packages.resolve(namespace)? else {
                    return Ok(Resolution::OpenDynamic(OpenReason::MissingPackage {
                        package: namespace.to_owned(),
                        binding: Some(name.to_owned()),
                    }));
                };
                if self.packages.is_external(foreign) {
                    self.external.insert(foreign);
                    return Ok(Resolution::Static(BindingTarget::External {
                        package: foreign,
                        binding: name.to_owned(),
                    }));
                }
                let foreign_image = self.image(foreign)?;
                return self.resolve_name(foreign, &foreign_image, name);
            }
            if environment == "base:base" || environment == "base:empty" {
                return Ok(if self.packages.is_base_binding(name) {
                    Resolution::Static(BindingTarget::Base)
                } else {
                    Resolution::OpenDynamic(OpenReason::Unresolved(name.to_owned()))
                });
            }
            return Ok(Resolution::OpenDynamic(OpenReason::Unresolved(
                name.to_owned(),
            )));
        }
    }

    pub(super) fn resolve_name(
        &mut self,
        current: PackageId,
        image: &PackageImage,
        name: &str,
    ) -> Result<Resolution<BindingTarget>> {
        if matches!(name, ".packageName" | ".__S3MethodsTable__.") {
            return Ok(Resolution::Static(BindingTarget::Metadata {
                package: current,
                name: name.to_owned(),
            }));
        }
        if name == ".__NAMESPACE__." {
            return Ok(Resolution::Static(BindingTarget::Metadata {
                package: current,
                name: name.to_owned(),
            }));
        }
        if image
            .index
            .binding_names
            .iter()
            .any(|binding| binding == name)
            || self
                .namespace_builders
                .get(&current)
                .is_some_and(|namespace| namespace.contains(name))
        {
            return Ok(Resolution::Static(BindingTarget::Namespace {
                package: current,
                binding: name.to_owned(),
            }));
        }

        if let Some(component) = Self::native_component_for_binding(&image.index, name) {
            return Ok(Resolution::Static(BindingTarget::Native {
                package: current,
                component: component.to_owned(),
                binding: name.to_owned(),
            }));
        }

        match self.namespace_imports(current, image)?.resolve(name) {
            NamespaceImportResolution::Imported {
                package: package_name,
                binding,
                ..
            } => {
                let Some(foreign) = self.packages.resolve(&package_name)? else {
                    return Ok(Resolution::OpenDynamic(OpenReason::MissingPackage {
                        package: package_name,
                        binding: Some(binding),
                    }));
                };
                return Ok(if self.packages.is_external(foreign) {
                    self.external.insert(foreign);
                    Resolution::Static(BindingTarget::External {
                        package: foreign,
                        binding,
                    })
                } else {
                    Resolution::Static(BindingTarget::Imported {
                        package: foreign,
                        binding,
                    })
                });
            }
            NamespaceImportResolution::MissingImportAll { package, binding } => {
                return Ok(Resolution::OpenDynamic(OpenReason::MissingPackage {
                    package,
                    binding: Some(binding),
                }));
            }
            NamespaceImportResolution::BaseFallback => {}
        }

        if self.packages.is_base_binding(name) {
            Ok(Resolution::Static(BindingTarget::Base))
        } else {
            Ok(Resolution::OpenDynamic(OpenReason::Unresolved(
                name.to_owned(),
            )))
        }
    }

    pub(super) fn require_resolved(
        &mut self,
        from: NodeId,
        requester: PackageId,
        binding: Option<&str>,
        resolved: Resolution<BindingTarget>,
        span: Span,
    ) -> Result<()> {
        match resolved {
            Resolution::Static(BindingTarget::Namespace { package, binding }) => self.require_at(
                from,
                Need::Binding {
                    package,
                    binding: binding.clone(),
                },
                EdgeKind::Lexical,
                format!("lexical reference `{binding}`"),
                Some(span.clone()),
            ),
            Resolution::Static(BindingTarget::Private {
                package,
                environment,
                binding,
            }) => self.require_at(
                from,
                Need::PrivateBinding {
                    package,
                    environment: environment.clone(),
                    binding: binding.clone(),
                },
                EdgeKind::Lexical,
                format!("lexical private reference `{binding}` in {environment}"),
                Some(span.clone()),
            ),
            Resolution::Static(BindingTarget::Closure { package, closure }) => self.require_at(
                from,
                Need::ClosureExecution { package, closure },
                EdgeKind::ClosureExecution,
                "reachable lexical reference resolves to an executable retained closure",
                Some(span.clone()),
            ),
            Resolution::Static(BindingTarget::Native {
                package,
                component,
                binding: native_binding,
            }) => self.require_at(
                from,
                Need::Native {
                    package,
                    component: component.clone(),
                },
                EdgeKind::Native,
                format!("registered native symbol `{native_binding}` is provided by `{component}`"),
                Some(span.clone()),
            ),
            Resolution::Static(BindingTarget::Imported { package, binding }) => {
                self.require_at(
                    from,
                    Need::Activation { package },
                    EdgeKind::Import,
                    "imported binding requires namespace activation",
                    Some(span.clone()),
                );
                self.require_at(
                    from,
                    Need::Binding {
                        package,
                        binding: binding.clone(),
                    },
                    EdgeKind::Import,
                    format!("imported binding `{binding}`"),
                    Some(span.clone()),
                );
            }
            Resolution::Static(BindingTarget::External { package, binding }) => {
                let node = self.graph.add_node(
                    self.packages.name(package).to_owned(),
                    NodeKind::ExternalBinding {
                        name: binding.clone(),
                    },
                    Some(span.clone()),
                );
                self.graph.add_edge_at(
                    from,
                    node,
                    EdgeKind::Import,
                    format!("External imported binding `{binding}`"),
                    Some(span),
                );
            }
            Resolution::Static(BindingTarget::Metadata { package, name }) => {
                let node = self.graph.add_node(
                    self.packages.name(package).to_owned(),
                    NodeKind::PackageMetadata { name: name.clone() },
                    Some(span.clone()),
                );
                self.graph.add_edge_at(
                    from,
                    node,
                    EdgeKind::Lexical,
                    format!("package metadata reference `{name}`"),
                    Some(span.clone()),
                );
                if name == ".__NAMESPACE__." && !self.is_root(package) {
                    self.diagnostic(
                        from,
                        requester,
                        binding,
                        RejectCode::DynamicLookup,
                        "synthetic dependency directly observes .__NAMESPACE__., which is intentionally not constructed",
                        Some(span),
                    );
                }
            }
            Resolution::OpenDynamic(OpenReason::MissingPackage { package, binding }) => {
                let detail = binding
                    .as_deref()
                    .map(|name| {
                        format!("imported binding `{name}` requires missing namespace {package}")
                    })
                    .unwrap_or_else(|| {
                        format!("reachable reference requires missing namespace {package}")
                    });
                self.record_missing_package(
                    from,
                    requester,
                    &package,
                    EdgeKind::Import,
                    detail,
                    Some(span),
                );
            }
            Resolution::Static(BindingTarget::Local) | Resolution::Static(BindingTarget::Base) => {}
            Resolution::OpenDynamic(OpenReason::Unresolved(name)) => {
                self.diagnostic(
                    from,
                    requester,
                    binding,
                    RejectCode::UnresolvedBinding,
                    format!("unresolved name `{name}`"),
                    Some(span),
                );
            }
        }
        Ok(())
    }
}
