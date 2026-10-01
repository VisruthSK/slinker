use super::diagnostic::{Cause, DiagnosticSink, Evidence};
use super::dynamic_names::DynamicNames;
use super::execute::{AbstractValue, ConstructionCallKey};
use super::guards::DeclaredDependencies;
use super::invocation::InvocationModel;
use super::namespace::NamespaceBuilder;
use super::native::NativeBindingIndex;
use super::need::{NeedQueue, Popped};
use super::object_world::ObjectWorld;
use super::parse_cache::{ParseCache, ParseKey};
use super::reflection::ReflectionFacts;
use super::relocation::RelocationPlan;
use super::s3::{CallableId, S3Model};
use crate::analysis::{Diagnostic, EdgeKind, GenericId, Graph, Need, NodeId, NodeKind, RejectCode};
use crate::ir::ExternalBindingAccess;
use crate::package::{
    BindingName, ClosureSource, ComponentName, EnvironmentLabel, GenericLabel, PackageId,
    PackageImage, PackageName, PackageProvider, TargetUniverse,
};
use crate::profile::{self, Counter, Probe};
use crate::syntax::{
    CallSite, NamespaceImports, OakParseContext, OakParser, ParsedRFile, SourceId, SourceKey, Span,
};
use crate::{Error, Result};
use rayon::prelude::*;
use std::collections::hash_map::Entry;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::Arc;

#[derive(Clone, Copy)]
pub(super) struct Caller<'a> {
    pub(super) node: NodeId,
    pub(super) package: PackageId,
    pub(super) binding: &'a str,
}

#[derive(Clone, Copy)]
pub(super) struct ParsedSite<'a> {
    pub(super) node: NodeId,
    pub(super) package: PackageId,
    pub(super) image: &'a PackageImage,
    pub(super) binding: &'a str,
    pub(super) lexical_environment: &'a EnvironmentLabel,
}

impl<'a> ParsedSite<'a> {
    pub(super) fn caller(&self) -> Caller<'a> {
        Caller {
            node: self.node,
            package: self.package,
            binding: self.binding,
        }
    }
}

#[derive(Clone, Copy)]
pub(super) struct ParseRequest<'a> {
    pub(super) owner: &'a SourceKey,
    pub(super) source_key: &'a SourceKey,
    pub(super) owner_node: NodeId,
}

#[derive(Clone, Copy)]
pub(super) struct NativeCallbackContext<'a> {
    pub(super) owner: NodeId,
    pub(super) package: PackageId,
    pub(super) image: &'a PackageImage,
    pub(super) binding: &'a str,
    pub(super) lexical_environment: &'a EnvironmentLabel,
    pub(super) component: &'a str,
    pub(super) parsed: &'a ParsedRFile,
    pub(super) call: &'a CallSite,
}

pub(super) struct AnalysisOptions {
    pub(super) jobs: usize,
    pub(super) provenance: bool,
    pub(super) linked_packages: HashSet<PackageName>,
    pub(super) explicit_external_packages: HashSet<PackageName>,
    pub(super) root_description: Option<Arc<str>>,
}

pub(crate) struct AnalyzerState<P: PackageProvider> {
    pub(super) packages: TargetUniverse<P>,
    pub(super) linked_packages: HashSet<PackageName>,
    pub(super) explicit_external_packages: HashSet<PackageName>,
    pub(super) jobs: usize,
    pub(super) parse_pool: Option<Arc<rayon::ThreadPool>>,
    pub(super) graph: Graph,
    pub(super) roots: Vec<NodeId>,
    pub(super) needs: NeedQueue,
    pub(super) encountered: HashSet<PackageId>,
    pub(super) external: HashSet<PackageId>,
    pub(super) parses: ParseCache,
    pub(super) loaded: HashMap<PackageId, LoadedPackage>,
    pub(super) objects: ObjectWorld,
    pub(super) diagnostics: DiagnosticSink,
    pub(super) relocations: RelocationPlan,
    pub(super) s3: S3Model,
    pub(super) invocations: InvocationModel,
    pub(super) value_closures: HashSet<NodeId>,
    pub(super) construction_calls: HashMap<ConstructionCallKey, AbstractValue>,
    pub(super) construction_evaluations: usize,
    pub(super) reflection: ReflectionFacts,
    pub(super) dynamic_names: DynamicNames,
    pub(super) external_bindings: BTreeMap<(PackageId, BindingName), ExternalBindingAccess>,
    pub(super) dependencies: HashMap<NodeId, HashSet<NodeId>>,
    pub(super) provenance: bool,
    pub(super) root: PackageId,
    pub(super) declared_dependencies: HashMap<PackageId, DeclaredDependencies>,
    pub(super) namespace_imports: HashMap<PackageId, NamespaceImports>,
    pub(super) non_returning_bindings: HashMap<PackageId, BTreeSet<BindingName>>,
    pub(super) root_description: Option<Arc<str>>,
}

pub(super) struct LoadedPackage {
    pub(super) image: Arc<PackageImage>,
    pub(super) namespace: NamespaceBuilder,
    pub(super) native_bindings: NativeBindingIndex,
}

pub(super) struct NativeCallTarget {
    pub(super) component: ComponentName,
    pub(super) consumes_selector: bool,
}

impl<P: PackageProvider> AnalyzerState<P> {
    pub(super) fn new(packages: P, root_name: &str, options: AnalysisOptions) -> Result<Self> {
        if options.explicit_external_packages.contains(root_name) {
            return Err(Error::Analysis(format!(
                "root package `{root_name}` cannot be External"
            )));
        }
        let mut packages = TargetUniverse::new(
            packages,
            root_name,
            options.explicit_external_packages.clone(),
        );
        let root = packages.require(root_name)?;
        Ok(Self {
            packages,
            linked_packages: options.linked_packages,
            explicit_external_packages: options.explicit_external_packages,
            jobs: options.jobs.max(1),
            parse_pool: None,
            graph: Graph::default(),
            roots: Vec::new(),
            needs: NeedQueue::default(),
            encountered: HashSet::from([root]),
            external: HashSet::new(),
            parses: ParseCache::default(),
            loaded: HashMap::new(),
            objects: ObjectWorld::default(),
            diagnostics: DiagnosticSink::default(),
            relocations: RelocationPlan::default(),
            s3: S3Model::default(),
            invocations: InvocationModel::default(),
            value_closures: HashSet::new(),
            construction_calls: HashMap::new(),
            construction_evaluations: 0,
            reflection: ReflectionFacts::default(),
            dynamic_names: DynamicNames::default(),
            external_bindings: BTreeMap::new(),
            dependencies: HashMap::new(),
            provenance: options.provenance,
            root,
            declared_dependencies: HashMap::new(),
            namespace_imports: HashMap::new(),
            non_returning_bindings: HashMap::new(),
            root_description: options.root_description,
        })
    }

    pub(super) fn run(mut self) -> Result<Self> {
        let root = self.root;
        let root_image = self.image(root)?;

        self.require_root(Need::Activation { package: root });
        while !self.needs.is_empty() {
            self.process_frontier()?;
        }

        let mut entry_bindings = root_image
            .index
            .exports
            .values()
            .chain(&root_image.index.binding_names)
            .cloned()
            .collect::<Vec<_>>();
        entry_bindings.sort();
        entry_bindings.dedup();
        for binding in entry_bindings {
            let exported = root_image
                .index
                .exports
                .values()
                .any(|target| target == &binding);
            let need = Need::Binding {
                package: root,
                binding,
            };
            if exported {
                self.require_root(need);
            } else {
                self.require_internal_root(need);
            }
        }

        while !self.needs.is_empty() {
            self.process_frontier()?;
        }
        let materialized = self
            .encountered
            .iter()
            .copied()
            .filter(|package| !self.packages.is_external(*package))
            .collect::<Vec<_>>();
        for package in materialized {
            let image = self.image(package)?;
            self.namespace_imports(package, &image)?;
        }
        Ok(self)
    }

    fn process_frontier(&mut self) -> Result<()> {
        let frontier = self.needs.len();
        if frontier == 0 {
            return Ok(());
        }

        self.preparse_frontier_bindings(frontier)?;

        for _ in 0..frontier {
            match self.needs.pop() {
                Some(Popped::Started(need)) => self.process_need(need)?,
                Some(Popped::AlreadyStarted) => {}
                None => break,
            }
        }
        Ok(())
    }

    pub(super) fn loaded(&mut self, package: PackageId) -> Result<&mut LoadedPackage> {
        match self.loaded.entry(package) {
            Entry::Occupied(entry) => Ok(entry.into_mut()),
            Entry::Vacant(entry) => {
                let index = self.packages.index(package)?;
                let image = Arc::new(PackageImage {
                    index: Arc::clone(&index),
                    bindings: HashMap::new(),
                    private_environments: HashMap::new(),
                });
                self.objects.merge(package, &image);
                Ok(entry.insert(LoadedPackage {
                    image,
                    namespace: NamespaceBuilder::new(&index),
                    native_bindings: NativeBindingIndex::new(&index),
                }))
            }
        }
    }

    pub(super) fn image(&mut self, package: PackageId) -> Result<Arc<PackageImage>> {
        Ok(Arc::clone(&self.loaded(package)?.image))
    }

    pub(super) fn binding_image(
        &mut self,
        package: PackageId,
        binding: &str,
    ) -> Result<Arc<PackageImage>> {
        let _span = profile::span(Probe::BindingImage);
        let image = self.image(package)?;
        if image.binding(binding).is_some() || !image.index.binding_names.contains(binding) {
            return Ok(image);
        }
        let partial = self.packages.binding_image(package, binding)?;
        self.objects.merge(package, &partial);
        drop(image);
        let loaded = self.loaded(package)?;
        let image = Arc::make_mut(&mut loaded.image);
        image.bindings.extend(
            partial
                .bindings
                .iter()
                .map(|(name, binding)| (name.clone(), binding.clone())),
        );
        for (id, environment) in &partial.private_environments {
            image
                .private_environments
                .entry(id.clone())
                .and_modify(|existing| {
                    existing.bindings.extend(
                        environment
                            .bindings
                            .iter()
                            .map(|(name, binding)| (name.clone(), binding.clone())),
                    );
                })
                .or_insert_with(|| environment.clone());
        }
        Ok(Arc::clone(&loaded.image))
    }

    fn parse_pool(&mut self) -> Result<Option<Arc<rayon::ThreadPool>>> {
        if self.jobs <= 1 {
            return Ok(None);
        }
        if self.parse_pool.is_none() {
            let pool = rayon::ThreadPoolBuilder::new()
                .num_threads(self.jobs)
                .thread_name(|index| format!("slinker-air-{index}"))
                .stack_size(super::ANALYSIS_STACK_BYTES)
                .build()
                .map_err(|error| {
                    Error::Analysis(format!("failed to create Rayon pool: {error}"))
                })?;
            self.parse_pool = Some(Arc::new(pool));
        }
        Ok(self.parse_pool.as_ref().map(Arc::clone))
    }

    fn preparse_frontier_bindings(&mut self, frontier: usize) -> Result<()> {
        let _span = profile::span(Probe::Preparse);
        struct Work {
            key: ParseKey,
            owner: SourceKey,
            source_key: SourceKey,
            owner_node: NodeId,
            source: SourceId,
            text: Arc<str>,
            context: OakParseContext,
        }

        let needs = self.needs.upcoming(frontier).cloned().collect::<Vec<_>>();
        let mut work = Vec::<Work>::new();
        let mut scheduled = HashSet::<ParseKey>::new();

        for need in needs {
            let (id, owner, source_key, closure, owner_node, image) = match need {
                Need::Binding {
                    package: id,
                    binding,
                } => {
                    if self.packages.is_external(id) {
                        continue;
                    }
                    let image = self.binding_image(id, &binding)?;
                    let Some(binding_image) = image.binding(&binding).cloned() else {
                        continue;
                    };
                    let Some(closure) = binding_image.object.closure else {
                        continue;
                    };
                    let owner_node = self.need_node(&Need::Binding {
                        package: id,
                        binding: binding.clone(),
                    });
                    let key = SourceKey::Binding(binding);
                    (id, key.clone(), key, closure, owner_node, image)
                }
                Need::PrivateBinding {
                    package: id,
                    environment,
                    binding,
                } => {
                    if self.packages.is_external(id) {
                        continue;
                    }
                    let image = self.image(id)?;
                    let Some(binding_image) =
                        image.private_binding(&environment, &binding).cloned()
                    else {
                        continue;
                    };
                    let Some(closure) = binding_image.object.closure else {
                        continue;
                    };
                    let owner_node = self.need_node(&Need::PrivateBinding {
                        package: id,
                        environment: environment.clone(),
                        binding: binding.clone(),
                    });
                    let key = SourceKey::private(environment, binding);
                    (id, key.clone(), key, closure, owner_node, image)
                }
                Need::ClosureExecution {
                    package: id,
                    closure,
                } => {
                    if self.packages.is_external(id) {
                        continue;
                    }
                    let image = self.image(id)?;
                    let Some(execution) = self.closure_execution_source(id, closure) else {
                        continue;
                    };
                    let owner_node = self.need_node(&Need::ClosureExecution {
                        package: id,
                        closure,
                    });
                    let source = ClosureSource {
                        source: execution.closure.source,
                        environment: execution.environment,
                    };
                    (
                        id,
                        execution.owner,
                        execution.key,
                        source,
                        owner_node,
                        image,
                    )
                }
                _ => continue,
            };

            let key = (id, source_key.clone());
            if self.parses.contains(&key) || !scheduled.insert(key.clone()) {
                continue;
            }
            let Some(source) =
                self.admit_source(id, &owner, &source_key, owner_node, &closure.source)?
            else {
                continue;
            };
            work.push(Work {
                key,
                owner,
                source_key,
                owner_node,
                source,
                text: Arc::clone(&closure.source),
                context: self.oak_parse_context(id, &image, &closure.environment)?,
            });
        }
        if work.is_empty() {
            return Ok(());
        }

        let parse_all = || {
            work.par_iter()
                .map(|item| {
                    OakParser.parse_binding_with_context(
                        item.source,
                        item.text.as_ref(),
                        &item.context,
                    )
                })
                .collect::<Vec<_>>()
        };
        let results = if work.len() > 1 {
            if let Some(pool) = self.parse_pool()? {
                pool.install(parse_all)
            } else {
                work.iter()
                    .map(|item| {
                        OakParser.parse_binding_with_context(
                            item.source,
                            item.text.as_ref(),
                            &item.context,
                        )
                    })
                    .collect()
            }
        } else {
            work.iter()
                .map(|item| {
                    OakParser.parse_binding_with_context(
                        item.source,
                        item.text.as_ref(),
                        &item.context,
                    )
                })
                .collect()
        };

        for (item, result) in work.into_iter().zip(results) {
            match result {
                Ok(parsed) => {
                    self.parses.store(item.key, Arc::new(parsed));
                }
                Err(error) => {
                    self.handle_air_rejection(
                        item.key.0,
                        &item.owner,
                        &item.source_key,
                        item.owner_node,
                        &error,
                    )?;
                }
            }
        }
        Ok(())
    }

    fn process_need(&mut self, need: Need) -> Result<()> {
        let _span = profile::span(Probe::ProcessNeed);
        profile::count(Counter::NeedsStarted);
        match need {
            Need::Binding { package, binding } => self.process_binding(package, &binding),
            Need::PrivateBinding {
                package,
                environment,
                binding,
            } => self.process_private_binding(package, &environment, &binding),
            Need::ClosureExecution { package, closure } => {
                self.process_closure_execution(package, closure)
            }
            Need::Activation { package } => self.process_activation(package),
            Need::Resource { package, resource } => self.process_resource(package, &resource),
            Need::Dataset { package, dataset } => self.process_dataset(package, &dataset),
            Need::S3Registration {
                package,
                registration,
            } => self.process_s3(package, &registration),
            Need::Native { package, component } => self.process_native(package, &component),
            Need::Lifecycle { package, hook } => {
                self.process_lifecycle(package, hook);
                Ok(())
            }
        }
    }

    fn require_root(&mut self, need: Need) {
        self.record_unclassified(&need);
        self.require_internal_root(need);
    }

    fn require_internal_root(&mut self, need: Need) {
        self.encountered.insert(need.package());
        let node = self.need_node(&need);
        if !self.roots.contains(&node) {
            self.roots.push(node);
        }
        self.needs.schedule(need);
    }

    pub(super) fn require(
        &mut self,
        from: NodeId,
        need: Need,
        kind: EdgeKind,
        reason: impl Into<String>,
    ) {
        self.require_at(from, need, kind, reason, None);
    }

    pub(super) fn require_at(
        &mut self,
        from: NodeId,
        need: Need,
        kind: EdgeKind,
        reason: impl Into<String>,
        span: Option<Span>,
    ) {
        self.record_unclassified(&need);
        self.require_classified_at(from, need, kind, reason, span);
    }

    pub(super) fn record_unclassified(&mut self, need: &Need) {
        if let Need::Binding { package, binding } = need {
            self.invocations.record_unclassified(CallableId {
                package: *package,
                binding: binding.clone(),
            });
        }
    }

    pub(super) fn require_classified_at(
        &mut self,
        from: NodeId,
        need: Need,
        kind: EdgeKind,
        reason: impl Into<String>,
        span: Option<Span>,
    ) {
        self.encountered.insert(need.package());
        let to = self.need_node(&need);
        self.depend(from, to, kind, reason, span);
        self.needs.schedule(need);
    }

    pub(super) fn depend(
        &mut self,
        from: NodeId,
        to: NodeId,
        kind: EdgeKind,
        reason: impl Into<String>,
        span: Option<Span>,
    ) {
        self.dependencies.entry(from).or_default().insert(to);
        if self.provenance {
            self.graph.add_edge(from, to, kind, reason, span);
        }
    }

    pub(super) fn external_binding(
        &mut self,
        package: PackageId,
        name: &str,
        access: ExternalBindingAccess,
        span: Option<Span>,
    ) -> NodeId {
        let recorded = self
            .external_bindings
            .entry((package, BindingName::from(name)))
            .or_insert(access);
        if access == ExternalBindingAccess::Internal {
            *recorded = access;
        }
        self.graph.add_node(
            self.packages.name(package).clone(),
            NodeKind::ExternalBinding {
                name: BindingName::from(name),
            },
            span,
        )
    }

    pub(super) fn need_node(&mut self, need: &Need) -> NodeId {
        let package = self.packages.name(need.package()).clone();
        let kind = match need {
            Need::Binding { binding, .. } => NodeKind::Binding {
                name: binding.clone(),
            },
            Need::PrivateBinding {
                environment,
                binding,
                ..
            } => NodeKind::PrivateBinding {
                environment: environment.clone(),
                name: binding.clone(),
            },
            Need::ClosureExecution { package, closure } => {
                let execution = self
                    .closure_execution_source(*package, *closure)
                    .expect("closure execution need references the package object graph");
                NodeKind::ClosureObject {
                    owner: execution.owner.clone(),
                    path: execution.closure.provenance.path.clone(),
                    enclosure: execution.environment.clone(),
                    derived: execution.closure.derived_from.is_some(),
                }
            }
            Need::Activation { .. } => NodeKind::Activation,
            Need::Resource { resource, .. } => NodeKind::Resource {
                path: resource.clone(),
            },
            Need::Dataset { dataset, .. } => NodeKind::Dataset {
                name: dataset.clone(),
            },
            Need::S3Registration { registration, .. } => NodeKind::S3Registration {
                generic: self.generic_label(&registration.generic),
                class: registration.class.clone(),
            },
            Need::Native { component, .. } => NodeKind::NativeComponent {
                name: component.clone(),
            },
            Need::Lifecycle { hook, .. } => NodeKind::Lifecycle {
                hook: hook.binding(),
            },
        };
        self.graph.add_node(package, kind, None)
    }

    pub(super) fn diagnostic(
        &mut self,
        node: NodeId,
        package: PackageId,
        binding: Option<&str>,
        code: RejectCode,
        message: impl Into<String>,
        span: Option<Span>,
    ) {
        let diagnostic = self.new_diagnostic(node, package, binding, code, message.into(), span);
        self.diagnostics.record(node, diagnostic);
    }

    fn new_diagnostic(
        &self,
        node: NodeId,
        package: PackageId,
        binding: Option<&str>,
        code: RejectCode,
        message: String,
        span: Option<Span>,
    ) -> Diagnostic {
        Diagnostic {
            package: self.packages.name(package).clone(),
            binding: binding.map(BindingName::from),
            code,
            message,
            span,
            node,
            evidence: Vec::new(),
        }
    }

    pub(super) fn generic_label(&self, generic: &GenericId) -> GenericLabel {
        match generic.package {
            Some(package) => format!("{}::{}", self.packages.name(package), generic.name).into(),
            None => generic.name.to_string().into(),
        }
    }

    pub(super) fn is_root(&self, id: PackageId) -> bool {
        self.root == id
    }

    pub(super) fn record_missing_package(
        &mut self,
        from: NodeId,
        requester: PackageId,
        missing: &str,
        kind: EdgeKind,
        reason: impl Into<String>,
        span: Option<Span>,
    ) {
        let reason = reason.into();
        let node = self.graph.add_node(
            PackageName::from(missing),
            NodeKind::MissingPackage,
            span.clone(),
        );
        self.graph
            .add_edge(from, node, kind, reason.clone(), span.clone());
        let primary = self.new_diagnostic(
            node,
            requester,
            None,
            RejectCode::MissingDependency,
            format!(
                "required package `{missing}` is absent from the selected target library universe"
            ),
            None,
        );
        let evidence = Evidence {
            package: self.packages.name(requester).clone(),
            binding: self.node_binding(from),
            span,
            detail: reason,
        };
        self.diagnostics.record_derived(
            Cause::MissingPackage(PackageName::from(missing)),
            Diagnostic {
                package: PackageName::from(missing),
                ..primary
            },
            evidence,
        );
    }

    fn node_binding(&self, node: NodeId) -> Option<BindingName> {
        match &self.graph.nodes[node.0].kind {
            NodeKind::Binding { name } | NodeKind::PrivateBinding { name, .. } => {
                Some(name.clone())
            }
            _ => None,
        }
    }
}
