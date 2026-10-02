use super::dynamic_names::UnresolvedName;
use super::object_world::{ClosureId, ClosureObject, ClosureOwner, Lookup, ObjectGraph, ObjectId};
use super::state::AnalyzerState;
use crate::Result;
use crate::analysis::{EdgeKind, Need, NodeId, NodeKind, RejectCode};
use crate::ir::ExternalBindingAccess;
use crate::package::{
    BindingName, ClosureSource, ComponentName, EnvironmentKind, EnvironmentLabel, ImportSpec,
    NativeComponent, PackageId, PackageImage, PackageName, PackageProvider,
};
use crate::profile::{self, Probe};
use crate::syntax::{
    NamespaceImportResolution, NamespaceImports, OakParseContext, SharedNames, SourceKey, Span,
    closure_definitely_non_returning,
};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::sync::Arc;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum Resolution {
    Static(BindingTarget),
    OpenDynamic(OpenReason),
}

impl Resolution {
    fn unresolved(name: &str) -> Self {
        Self::OpenDynamic(OpenReason::Unresolved(name.into()))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum OpenReason {
    Unresolved(BindingName),
    MissingPackage {
        package: PackageName,
        binding: Option<BindingName>,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum MetadataBinding {
    PackageName,
    S3MethodsTable,
    Namespace,
}

impl MetadataBinding {
    pub(super) const ALL: [Self; 3] = [Self::PackageName, Self::S3MethodsTable, Self::Namespace];

    pub(super) fn of(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|metadata| metadata.name() == name)
    }

    pub(super) fn name(self) -> &'static str {
        match self {
            Self::PackageName => ".packageName",
            Self::S3MethodsTable => ".__S3MethodsTable__.",
            Self::Namespace => ".__NAMESPACE__.",
        }
    }
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
        environment: EnvironmentLabel,
        binding: BindingName,
    },
    Closure {
        package: PackageId,
        closure: ClosureId,
    },
    Native {
        package: PackageId,
        component: ComponentName,
        binding: BindingName,
    },
    External {
        package: PackageId,
        binding: BindingName,
    },
    Metadata {
        package: PackageId,
        binding: MetadataBinding,
    },
}

enum DerivedStep {
    Resolved(Resolution),
    Parent(EnvironmentLabel),
}

fn prove_non_returning<'a>(
    candidates: &(impl Iterator<Item = (&'a BindingName, &'a ClosureSource)> + Clone),
    shadowed: &BTreeSet<BindingName>,
    imports: &NamespaceImports,
    proven: &mut BTreeSet<BindingName>,
) {
    loop {
        let context = OakParseContext::with_imports(
            SharedNames::from(shadowed.clone()),
            Arc::new(imports.clone()),
            SharedNames::from(proven.clone()),
        );
        let before = proven.len();
        for (name, closure) in candidates.clone() {
            if !proven.contains(name) && closure_definitely_non_returning(&closure.source, &context)
            {
                proven.insert(name.clone());
            }
        }
        if proven.len() == before {
            return;
        }
    }
}

impl<P: PackageProvider> AnalyzerState<P> {
    pub(super) fn namespace_imports(
        &self,
        package: PackageId,
        image: &PackageImage,
    ) -> Result<Arc<NamespaceImports>> {
        let _span = profile::span(Probe::NamespaceImports);
        if let Some(imports) = self.namespace_imports.lock().get(&package) {
            return Ok(Arc::clone(imports));
        }
        let mut imports = NamespaceImports::default();
        for import in &image.index.imports {
            match import {
                ImportSpec::From { package, bindings } => imports.add_import_from(
                    package.clone(),
                    bindings
                        .iter()
                        .map(|binding| (binding.local.clone(), binding.remote.clone())),
                ),
                ImportSpec::All {
                    package: package_name,
                    except,
                } => {
                    let exports = match self.packages.resolve(package_name)? {
                        Some(foreign) => Some(self.packages.index(foreign)?.exports.clone()),
                        None => None,
                    };
                    imports.add_import_all(package_name.clone(), exports, except.iter().cloned());
                }
            }
        }
        let imports = Arc::new(imports);
        self.namespace_imports
            .lock()
            .insert(package, Arc::clone(&imports));
        Ok(imports)
    }

    fn namespace_shadowed_names<'a>(
        &self,
        package: PackageId,
        image: &PackageImage,
        indexed: impl Iterator<Item = &'a BindingName>,
    ) -> Result<BTreeSet<BindingName>> {
        self.seal_namespace(package)?;
        let mut shadowed = indexed.cloned().collect::<BTreeSet<_>>();
        shadowed.extend(
            self.loaded(package)?
                .namespace
                .lock()
                .bindings
                .iter()
                .cloned(),
        );
        shadowed.extend(
            image
                .index
                .dynlibs
                .iter()
                .flat_map(NativeComponent::bindings)
                .map(|symbol| symbol.binding),
        );
        Ok(shadowed)
    }

    fn namespace_shadow_base(
        &self,
        package: PackageId,
        image: &PackageImage,
    ) -> Result<Arc<BTreeSet<BindingName>>> {
        self.seal_namespace(package)?;
        let loaded = self.loaded(package)?;
        let version = loaded.namespace.lock().bindings.len();
        let known = self.shadow_bases.lock().get(&package).cloned();
        if let Some((known_version, base)) = known
            && known_version == version
        {
            return Ok(base);
        }
        let base = Arc::new(self.namespace_shadowed_names(
            package,
            image,
            image.index.binding_names.iter(),
        )?);
        self.shadow_bases
            .lock()
            .insert(package, (version, Arc::clone(&base)));
        Ok(base)
    }

    fn inferred_non_returning_bindings(
        &self,
        package: PackageId,
        image: &PackageImage,
        imports: &NamespaceImports,
    ) -> Result<Arc<BTreeSet<BindingName>>> {
        let known = self.non_returning_bindings.lock().get(&package).cloned();
        if let Some(bindings) = known {
            return Ok(bindings);
        }
        let shadowed = self.namespace_shadowed_names(package, image, image.bindings.keys())?;
        let namespace = EnvironmentLabel::namespace(&self.packages.name(package));
        let candidates = image.bindings.iter().filter_map(|(name, binding)| {
            let closure = binding.object.closure.as_ref()?;
            (closure.environment == namespace).then_some((name, closure))
        });
        let mut proven = BTreeSet::new();
        prove_non_returning(&candidates, &shadowed, imports, &mut proven);
        let proven = Arc::new(proven);
        self.non_returning_bindings
            .lock()
            .insert(package, Arc::clone(&proven));
        Ok(proven)
    }

    pub(super) fn oak_parse_context(
        &self,
        package: PackageId,
        image: &PackageImage,
        lexical_environment: &EnvironmentLabel,
    ) -> Result<OakParseContext> {
        let _span = profile::span(Probe::OakParseContext);
        let base = self.namespace_shadow_base(package, image)?;
        let mut extra_shadowed = BTreeSet::new();
        let mut private_shadowed = BTreeSet::new();
        let mut visible_private = BTreeMap::new();
        let mut current = Some(lexical_environment.clone());
        self.objects.existing(package, |graph| {
            let Some(mut environment) = graph.environment_id(lexical_environment) else {
                return;
            };
            let mut seen = BTreeSet::new();
            while seen.insert(environment) {
                let shape = graph.environment(environment);
                if !shape.is_derived() {
                    current = Some(shape.label.clone());
                    break;
                }
                for name in shape.bindings.keys() {
                    private_shadowed.insert(name.clone());
                    extra_shadowed.insert(name.clone());
                }
                match shape.parent {
                    Some(parent) => environment = parent,
                    None => {
                        current = None;
                        break;
                    }
                }
            }
        });
        let mut seen = HashSet::new();
        while let Some(label) = current.take() {
            let Some(private) = image
                .private_environment(&label)
                .filter(|_| seen.insert(label.clone()))
            else {
                break;
            };
            for (name, binding) in &private.bindings {
                private_shadowed.insert(name.clone());
                extra_shadowed.insert(name.clone());
                visible_private.entry(name).or_insert(binding);
            }
            current = Some(private.parent.clone());
        }

        let imports = self.namespace_imports(package, image)?;
        let proven = self.inferred_non_returning_bindings(package, image, &imports)?;
        if private_shadowed.is_empty() && visible_private.is_empty() {
            return Ok(OakParseContext::with_imports(
                SharedNames::from(base),
                imports,
                SharedNames::from(proven),
            ));
        }
        let mut shadowed = (*base).clone();
        shadowed.extend(extra_shadowed);
        let mut non_returning = (*proven).clone();
        non_returning.retain(|name| !private_shadowed.contains(name));
        let candidates = visible_private
            .iter()
            .filter_map(|(&name, binding)| Some((name, binding.object.closure.as_ref()?)));
        prove_non_returning(&candidates, &shadowed, &imports, &mut non_returning);
        Ok(OakParseContext::with_imports(
            SharedNames::from(shadowed),
            imports,
            SharedNames::from(non_returning),
        ))
    }

    pub(super) fn resolve_lexical_name(
        &self,
        current: PackageId,
        image: &PackageImage,
        lexical_environment: &EnvironmentLabel,
        name: &str,
    ) -> Result<Resolution> {
        let _span = profile::span(Probe::ResolveLexicalName);
        let mut environment = lexical_environment.clone();
        let mut checkpoint: Option<EnvironmentLabel> = None;
        let (mut steps, mut window) = (0_usize, 1_usize);
        loop {
            if checkpoint.as_ref() == Some(&environment) {
                return Ok(Resolution::unresolved(name));
            }
            steps += 1;
            if steps == window {
                checkpoint = Some(environment.clone());
                window *= 2;
                steps = 0;
            }
            if environment.is_derived() {
                match self.resolve_derived(current, &environment, name) {
                    DerivedStep::Resolved(resolution) => return Ok(resolution),
                    DerivedStep::Parent(parent) => environment = parent,
                }
                continue;
            }
            if let Some(private) = image.private_environment(&environment) {
                if private.bindings.contains_key(name) {
                    return Ok(Resolution::Static(BindingTarget::Private {
                        package: current,
                        environment: private.id.clone(),
                        binding: name.into(),
                    }));
                }
                environment = private.parent.clone();
                continue;
            }
            return match environment.kind() {
                EnvironmentKind::Namespace(package) if package == self.packages.name(current) => {
                    self.resolve_name(current, image, name)
                }
                EnvironmentKind::Namespace(package) => {
                    self.resolve_foreign_namespace(package, name)
                }
                EnvironmentKind::Base | EnvironmentKind::Empty
                    if self.packages.is_base_binding(name) =>
                {
                    Ok(Resolution::Static(BindingTarget::Base))
                }
                _ => Ok(Resolution::unresolved(name)),
            };
        }
    }

    fn resolve_derived(
        &self,
        current: PackageId,
        environment: &EnvironmentLabel,
        name: &str,
    ) -> DerivedStep {
        let unresolved = || DerivedStep::Resolved(Resolution::unresolved(name));
        self.objects
            .existing(current, |graph| {
                let Some(id) = graph.environment_id(environment) else {
                    return unresolved();
                };
                match graph.lookup_environment_binding(id, name) {
                    Lookup::Found(object) => DerivedStep::Resolved(Resolution::Static(
                        derived_binding_target(graph, current, object),
                    )),
                    Lookup::Opaque => unresolved(),
                    Lookup::Absent => match graph.environment(id).parent {
                        Some(parent) => {
                            DerivedStep::Parent(graph.environment(parent).label.clone())
                        }
                        None => unresolved(),
                    },
                }
            })
            .unwrap_or_else(unresolved)
    }

    fn resolve_foreign_namespace(&self, package: &str, name: &str) -> Result<Resolution> {
        let Some(foreign) = self.packages.resolve(package)? else {
            return Ok(Resolution::OpenDynamic(OpenReason::MissingPackage {
                package: package.into(),
                binding: Some(name.into()),
            }));
        };
        if self.packages.is_external(foreign) {
            self.external.lock().insert(foreign);
            return Ok(Resolution::Static(BindingTarget::External {
                package: foreign,
                binding: name.into(),
            }));
        }
        let foreign_image = self.image(foreign)?;
        self.resolve_name(foreign, &foreign_image, name)
    }

    pub(super) fn closure_execution_source(
        &self,
        package: PackageId,
        closure: ClosureId,
    ) -> Option<ClosureExecutionSource> {
        let (closure, environment) = self.objects.existing(package, |graph| {
            let closure = graph.closure(closure).clone();
            let environment = graph.environment(closure.enclosure).label.clone();
            (closure, environment)
        })?;
        let owner = closure.provenance.owner.source_key();
        let key = SourceKey::Closure {
            owner: Box::new(owner.clone()),
            path: closure.provenance.path.clone(),
            environment: environment.clone(),
        };
        Some(ClosureExecutionSource {
            closure,
            owner,
            key,
            environment,
        })
    }

    pub(super) fn resolve_name(
        &self,
        current: PackageId,
        image: &PackageImage,
        name: &str,
    ) -> Result<Resolution> {
        self.seal_namespace(current)?;
        if let Some(binding) = MetadataBinding::of(name) {
            return Ok(Resolution::Static(BindingTarget::Metadata {
                package: current,
                binding,
            }));
        }
        if image.index.binding_names.contains(name) || self.namespace_declares(current, name) {
            return Ok(Resolution::Static(BindingTarget::Namespace {
                package: current,
                binding: name.into(),
            }));
        }

        if let Some(component) = self.loaded(current)?.native_bindings.sole_component(name) {
            return Ok(Resolution::Static(BindingTarget::Native {
                package: current,
                component: component.clone(),
                binding: name.into(),
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
                    self.external.lock().insert(foreign);
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
            Ok(Resolution::unresolved(name))
        }
    }

    pub(super) fn require_resolved(
        &self,
        from: NodeId,
        requester: PackageId,
        binding: Option<&str>,
        resolved: Resolution,
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
                    component: component.clone(),
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
            Resolution::Static(BindingTarget::Metadata {
                package,
                binding: metadata,
            }) => {
                let node = self.graph.lock().add_node(
                    self.packages.name(package),
                    NodeKind::PackageMetadata {
                        name: BindingName::from(metadata.name()),
                    },
                    Some(span.clone()),
                );
                self.depend(
                    from,
                    node,
                    EdgeKind::Lexical,
                    format!("package metadata reference `{}`", metadata.name()),
                    Some(span.clone()),
                );
                if metadata == MetadataBinding::Namespace && !self.is_root(package) {
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
                self.dynamic_names
                    .lock()
                    .observe_unresolved(UnresolvedName {
                        package: requester,
                        binding: binding.map(BindingName::from),
                        name: BindingName::from(name.as_str()),
                        span,
                    });
            }
        }
    }
}

pub(super) struct ClosureExecutionSource {
    pub(super) closure: ClosureObject,
    pub(super) owner: SourceKey,
    pub(super) key: SourceKey,
    pub(super) environment: EnvironmentLabel,
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
    if closure_object.derived_from.is_some() || !provenance.path.is_root() {
        return BindingTarget::Closure {
            package: current,
            closure,
        };
    }
    match &provenance.owner {
        ClosureOwner::Namespace(binding) => BindingTarget::Namespace {
            package: current,
            binding: binding.clone(),
        },
        ClosureOwner::Private {
            environment,
            binding,
        } => BindingTarget::Private {
            package: current,
            environment: environment.clone(),
            binding: binding.clone(),
        },
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ReferenceUse {
    Recorded,
    Unrecorded,
}

impl<P: PackageProvider> AnalyzerState<P> {
    fn require_binding_at(
        &self,
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
