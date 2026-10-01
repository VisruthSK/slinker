use super::dynamic_names::UnresolvedName;
use super::object_world::{ClosureId, ClosureObject, ObjectGraph, ObjectId};
use super::state::AnalyzerState;
use crate::Result;
use crate::analysis::{EdgeKind, Need, NodeId, NodeKind, RejectCode};
use crate::ir::ExternalBindingAccess;
use crate::package::{BindingName, ImportSpec, PackageId, PackageImage, PackageProvider};
use crate::syntax::{
    NamespaceImportResolution, NamespaceImports, OakParseContext, SourceKey, Span,
    closure_definitely_non_returning,
};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

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

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum BindingTarget {
    Local,
    Base,
    Namespace {
        package: PackageId,
        binding: BindingName,
    },
    Imported {
        package: PackageId,
        binding: BindingName,
    },
    Private {
        package: PackageId,
        environment: String,
        binding: BindingName,
    },
    Closure {
        package: PackageId,
        closure: ClosureId,
    },
    Native {
        package: PackageId,
        component: String,
        binding: BindingName,
    },
    External {
        package: PackageId,
        binding: BindingName,
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
            match import {
                ImportSpec::From { package, bindings } => {
                    imports.add_import_from(
                        package.as_str(),
                        bindings
                            .iter()
                            .map(|binding| (binding.local.to_string(), binding.remote.to_string())),
                    );
                }
                ImportSpec::All {
                    package: package_name,
                    except,
                } => {
                    let exports = match self.packages.resolve(package_name)? {
                        Some(foreign) => Some(self.packages.index(foreign)?.exports.clone()),
                        None => None,
                    };
                    imports.add_import_all(
                        package_name.as_str(),
                        exports.map(|exports| {
                            exports
                                .into_iter()
                                .map(|(export, binding)| (export, binding.into_string()))
                                .collect()
                        }),
                        except.iter().map(ToString::to_string),
                    );
                }
            }
        }

        self.namespace_imports.insert(package, imports.clone());
        Ok(imports)
    }

    pub(super) fn inferred_non_returning_bindings(
        &mut self,
        package: PackageId,
        image: &PackageImage,
        imports: &NamespaceImports,
    ) -> Result<BTreeSet<String>> {
        if let Some(bindings) = self.non_returning_bindings.get(&package) {
            return Ok(bindings.clone());
        }

        let mut namespace_shadowed = BTreeSet::<String>::new();
        namespace_shadowed.extend(image.bindings.keys().map(ToString::to_string));
        namespace_shadowed.extend(
            self.loaded(package)?
                .namespace
                .bindings
                .iter()
                .map(ToString::to_string),
        );
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
                if proven.contains(name.as_str()) {
                    continue;
                }
                let Some(closure) = &binding.closure else {
                    continue;
                };
                if closure.environment != namespace_environment {
                    continue;
                }
                if closure_definitely_non_returning(closure.source.as_ref(), &context) {
                    proven.insert(name.to_string());
                }
            }
            if proven.len() == before {
                break;
            }
        }

        self.non_returning_bindings.insert(package, proven.clone());
        Ok(proven)
    }

    pub(super) fn oak_parse_context(
        &mut self,
        package: PackageId,
        image: &PackageImage,
        lexical_environment: &str,
    ) -> Result<OakParseContext> {
        let mut shadowed = BTreeSet::new();
        shadowed.extend(image.index.binding_names.iter().map(ToString::to_string));
        shadowed.extend(
            self.loaded(package)?
                .namespace
                .bindings
                .iter()
                .map(ToString::to_string),
        );
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
                    environment.clone_from(&shape.label);
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
                private_shadowed.insert(name.to_string());
                shadowed.insert(name.to_string());
                visible_private.entry(name.clone()).or_insert(binding);
            }
            environment = private.parent.clone();
        }

        let imports = self.namespace_imports(package, image)?;
        let mut non_returning = self.inferred_non_returning_bindings(package, image, &imports)?;
        non_returning.retain(|name| !private_shadowed.contains(name));

        loop {
            let context = OakParseContext::with_imports(
                shadowed.clone(),
                imports.clone(),
                non_returning.clone(),
            );
            let before = non_returning.len();
            for (name, binding) in &visible_private {
                if non_returning.contains(name.as_str()) {
                    continue;
                }
                let Some(closure) = &binding.closure else {
                    continue;
                };
                if closure_definitely_non_returning(closure.source.as_ref(), &context) {
                    non_returning.insert(name.to_string());
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

    pub(super) fn private_source_key(environment: &str, binding: &str) -> SourceKey {
        SourceKey::Private {
            environment: environment.to_owned(),
            binding: binding.to_owned(),
        }
    }

    pub(super) fn closure_execution_source(
        &self,
        package: PackageId,
        closure: ClosureId,
    ) -> Option<(ClosureObject, SourceKey, SourceKey, String)> {
        let graph = self.objects.get(package)?;
        let closure = graph.closure(closure).clone();
        let environment = graph.environment(closure.enclosure).label.clone();
        let owner = match (
            &closure.provenance.namespace_binding,
            &closure.provenance.private_environment,
            &closure.provenance.private_binding,
        ) {
            (Some(binding), _, _) => SourceKey::Binding(binding.clone()),
            (_, Some(private), Some(binding)) => Self::private_source_key(private, binding),
            _ => SourceKey::Runtime,
        };
        let source_key = SourceKey::Closure {
            owner: Box::new(owner.clone()),
            path: closure.provenance.path.clone(),
            environment: environment.clone(),
        };
        Some((closure, owner, source_key, environment))
    }

    pub(super) fn resolve_lexical_name(
        &mut self,
        current: PackageId,
        image: &PackageImage,
        lexical_environment: &str,
        name: &str,
    ) -> Result<Resolution<BindingTarget>> {
        let _timer = crate::profile::time(crate::profile::Probe::ResolveLexicalName);
        crate::profile::unique(
            crate::profile::Probe::ResolveLexicalName,
            &(current, lexical_environment, name),
        );
        let unresolved = || Resolution::OpenDynamic(OpenReason::Unresolved(name.to_owned()));
        let mut environment = lexical_environment.to_owned();
        let mut seen = HashSet::new();
        while environment.starts_with("derived:") {
            if !seen.insert(environment.clone()) {
                return Ok(unresolved());
            }
            let Some(graph) = self.objects.get(current) else {
                return Ok(unresolved());
            };
            let Some(environment_id) = graph.environment_id(&environment) else {
                return Ok(unresolved());
            };
            let (object, blocked) = graph.lookup_environment_binding(environment_id, name);
            if let Some(object) = object {
                return Ok(Resolution::Static(derived_binding_target(
                    graph, current, object,
                )));
            }
            if blocked {
                return Ok(unresolved());
            }
            let Some(parent) = graph.environment(environment_id).parent else {
                return Ok(unresolved());
            };
            environment = graph.environment(parent).label.clone();
        }
        self.resolve_installed_environment(current, image, &environment, name)
    }

    fn resolve_installed_environment(
        &mut self,
        current: PackageId,
        image: &PackageImage,
        environment: &str,
        name: &str,
    ) -> Result<Resolution<BindingTarget>> {
        if let Some(memoized) = self
            .lexical_memo
            .get(self.namespace_epoch, current, environment, name)
        {
            crate::profile::count(crate::profile::Count::LexicalMemoHit, 1);
            #[cfg(debug_assertions)]
            {
                let recomputed =
                    self.resolve_installed_environment_uncached(current, image, environment, name)?;
                assert_eq!(
                    memoized, recomputed,
                    "lexical memo diverged for {environment}::{name}"
                );
            }
            return Ok(memoized);
        }
        crate::profile::count(crate::profile::Count::LexicalMemoMiss, 1);
        let resolved =
            self.resolve_installed_environment_uncached(current, image, environment, name)?;
        self.lexical_memo.insert(
            self.namespace_epoch,
            current,
            environment,
            name,
            resolved.clone(),
        );
        Ok(resolved)
    }

    fn resolve_installed_environment_uncached(
        &mut self,
        current: PackageId,
        image: &PackageImage,
        environment: &str,
        name: &str,
    ) -> Result<Resolution<BindingTarget>> {
        let unresolved = || Resolution::OpenDynamic(OpenReason::Unresolved(name.to_owned()));
        let mut environment = environment.to_owned();
        let mut seen = HashSet::new();
        loop {
            if !seen.insert(environment.clone()) {
                return Ok(unresolved());
            }
            if let Some(private) = image.private_environment(&environment) {
                if private.bindings.contains_key(name) {
                    return Ok(Resolution::Static(BindingTarget::Private {
                        package: current,
                        environment: private.id.clone(),
                        binding: name.to_owned().into(),
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
                        binding: name.to_owned().into(),
                    }));
                }
                let foreign_image = self.image(foreign)?;
                return self.resolve_name(foreign, &foreign_image, name);
            }
            if environment == "base:base" || environment == "base:empty" {
                return Ok(if self.packages.is_base_binding(name) {
                    Resolution::Static(BindingTarget::Base)
                } else {
                    unresolved()
                });
            }
            return Ok(unresolved());
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
                .loaded
                .get(&current)
                .is_some_and(|loaded| loaded.namespace.contains(name))
        {
            return Ok(Resolution::Static(BindingTarget::Namespace {
                package: current,
                binding: name.to_owned().into(),
            }));
        }

        if let Some(component) = Self::native_component_for_binding(&image.index, name) {
            return Ok(Resolution::Static(BindingTarget::Native {
                package: current,
                component: component.to_owned(),
                binding: name.to_owned().into(),
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
                        binding: binding.into(),
                    })
                } else {
                    Resolution::Static(BindingTarget::Imported {
                        package: foreign,
                        binding: binding.into(),
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
        use_kind: ReferenceUse,
    ) {
        match resolved {
            Resolution::Static(BindingTarget::Namespace { package, binding }) => self
                .require_binding_at(
                    from,
                    Need::Binding {
                        package,
                        binding: binding.clone(),
                    },
                    EdgeKind::Lexical,
                    format!("lexical reference `{binding}`"),
                    Some(span),
                    use_kind,
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
                Some(span),
            ),
            Resolution::Static(BindingTarget::Closure { package, closure }) => self.require_at(
                from,
                Need::ClosureExecution { package, closure },
                EdgeKind::ClosureExecution,
                "reachable lexical reference resolves to an executable retained closure",
                Some(span),
            ),
            Resolution::Static(BindingTarget::Native {
                package,
                component,
                binding: native_binding,
            }) => self.require_at(
                from,
                Need::Native {
                    package,
                    component: component.clone().into(),
                },
                EdgeKind::Native,
                format!("registered native symbol `{native_binding}` is provided by `{component}`"),
                Some(span),
            ),
            Resolution::Static(BindingTarget::Imported { package, binding }) => {
                self.require_at(
                    from,
                    Need::Activation { package },
                    EdgeKind::Import,
                    "imported binding requires namespace activation",
                    Some(span.clone()),
                );
                self.require_binding_at(
                    from,
                    Need::Binding {
                        package,
                        binding: binding.clone(),
                    },
                    EdgeKind::Import,
                    format!("imported binding `{binding}`"),
                    Some(span),
                    use_kind,
                );
            }
            Resolution::Static(BindingTarget::External { package, binding }) => {
                let node = self.external_binding(
                    package,
                    &binding,
                    ExternalBindingAccess::Exported,
                    Some(span.clone()),
                );
                self.depend(
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
                self.depend(
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
                let detail = binding.as_deref().map_or_else(
                    || format!("reachable reference requires missing namespace {package}"),
                    |name| {
                        format!("imported binding `{name}` requires missing namespace {package}")
                    },
                );
                self.record_missing_package(
                    from,
                    requester,
                    &package,
                    EdgeKind::Import,
                    detail,
                    Some(span),
                );
            }
            Resolution::Static(BindingTarget::Local | BindingTarget::Base) => {}
            Resolution::OpenDynamic(OpenReason::Unresolved(name)) => {
                self.dynamic_names.observe_unresolved(UnresolvedName {
                    package: requester,
                    binding: binding.map(str::to_owned),
                    name,
                    span,
                });
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ReferenceUse {
    Recorded,
    Unrecorded,
}

impl<P: PackageProvider> AnalyzerState<P> {
    fn require_binding_at(
        &mut self,
        from: NodeId,
        need: Need,
        kind: EdgeKind,
        reason: String,
        span: Option<Span>,
        use_kind: ReferenceUse,
    ) {
        match use_kind {
            ReferenceUse::Recorded => self.require_classified_at(from, need, kind, reason, span),
            ReferenceUse::Unrecorded => self.require_at(from, need, kind, reason, span),
        }
    }
}

fn derived_binding_target(
    graph: &ObjectGraph,
    current: PackageId,
    object: ObjectId,
) -> BindingTarget {
    let Some(closure) = graph.closure_of(object) else {
        return BindingTarget::Local;
    };
    let closure_object = graph.closure(closure);
    let provenance = &closure_object.provenance;
    if closure_object.derived_from.is_some() || provenance.path != "$" {
        BindingTarget::Closure {
            package: current,
            closure,
        }
    } else if let Some(binding) = &provenance.namespace_binding {
        BindingTarget::Namespace {
            package: current,
            binding: binding.clone().into(),
        }
    } else if let (Some(environment), Some(binding)) =
        (&provenance.private_environment, &provenance.private_binding)
    {
        BindingTarget::Private {
            package: current,
            environment: environment.clone(),
            binding: binding.clone().into(),
        }
    } else {
        BindingTarget::Local
    }
}

#[derive(Default)]
pub(super) struct LexicalMemo {
    epoch: u64,
    entries: HashMap<PackageId, HashMap<String, HashMap<String, Resolution<BindingTarget>>>>,
}

impl LexicalMemo {
    fn get(
        &mut self,
        epoch: u64,
        package: PackageId,
        environment: &str,
        name: &str,
    ) -> Option<Resolution<BindingTarget>> {
        if self.epoch != epoch {
            self.entries.clear();
            self.epoch = epoch;
            return None;
        }
        self.entries
            .get(&package)?
            .get(environment)?
            .get(name)
            .cloned()
    }

    fn insert(
        &mut self,
        epoch: u64,
        package: PackageId,
        environment: &str,
        name: &str,
        resolution: Resolution<BindingTarget>,
    ) {
        if self.epoch != epoch {
            self.entries.clear();
            self.epoch = epoch;
        }
        self.entries
            .entry(package)
            .or_default()
            .entry(environment.to_owned())
            .or_default()
            .insert(name.to_owned(), resolution);
    }
}
