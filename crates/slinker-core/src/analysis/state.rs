use super::diagnostic::{Cause, DiagnosticSink, Evidence};
use super::dynamic_names::DynamicNames;
use super::execute::{AbstractValue, ConstructionCallKey};
use super::guarded::Guarded;
use super::guards::DeclaredDependencies;
use super::invocation::InvocationModel;
use super::namespace::NamespaceBuilder;
use super::native::NativeBindingIndex;
use super::need::{Schedule, WorkKey};
use super::object_world::ObjectWorld;
use super::parse_cache::ParseCache;
use super::reflection::ReflectionFacts;
use super::relocation::RelocationPlan;
use super::s3::{CallableId, S3Model};
use super::scheduler::{Claim, Machine};
use super::summary::{FrameRecord, SummaryTable};
use crate::analysis::{Diagnostic, EdgeKind, GenericId, Graph, Need, NodeId, NodeKind, RejectCode};
use crate::ir::ExternalBindingAccess;
use crate::package::ObjectImage;
use crate::package::{
    BindingName, ComponentName, EnvironmentLabel, GenericLabel, PackageId, PackageImage,
    PackageName, PackageProvider, TargetUniverse,
};
use crate::profile::{self, Counter, Probe};
use crate::syntax::{CallSite, NamespaceImports, ParsedRFile, SourceKey, Span};
use crate::{Error, Result};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::atomic::AtomicUsize;
use std::sync::{Arc, RwLock};

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
    pub(super) source_key: &'a SourceKey,
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
    pub(super) schedule: Schedule,
    pub(super) provenance: bool,
    pub(super) linked_packages: HashSet<PackageName>,
    pub(super) explicit_external_packages: HashSet<PackageName>,
    pub(super) root_description: Option<Arc<str>>,
}

type ShadowBase = (usize, Arc<BTreeSet<BindingName>>);

pub(crate) struct AnalyzerState<P: PackageProvider> {
    pub(super) packages: TargetUniverse<P>,
    pub(super) linked_packages: HashSet<PackageName>,
    pub(super) explicit_external_packages: HashSet<PackageName>,
    pub(super) jobs: usize,
    pub(super) schedule: Schedule,
    pub(super) graph: Guarded<Graph>,
    pub(super) roots: Guarded<Vec<NodeId>>,
    pub(super) work: Machine<WorkKey>,
    pub(super) failure: Guarded<Option<Error>>,
    pub(super) encountered: Guarded<HashSet<PackageId>>,
    pub(super) external: Guarded<HashSet<PackageId>>,
    pub(super) parses: Guarded<ParseCache>,
    pub(super) loaded: RwLock<HashMap<PackageId, Arc<LoadedPackage>>>,
    load_gate: Guarded<()>,
    pub(super) objects: ObjectWorld,
    pub(super) diagnostics: Guarded<DiagnosticSink>,
    pub(super) relocations: Guarded<RelocationPlan>,
    pub(super) s3: Guarded<S3Model>,
    pub(super) invocations: Guarded<InvocationModel>,
    pub(super) value_closures: Guarded<HashSet<NodeId>>,
    pub(super) construction_calls:
        Guarded<HashMap<ConstructionCallKey, (AbstractValue, FrameRecord)>>,
    pub(super) summaries: SummaryTable,
    pub(super) construction_evaluations: AtomicUsize,
    pub(super) reflection: Guarded<ReflectionFacts>,
    pub(super) dynamic_names: Guarded<DynamicNames>,
    pub(super) dependencies: Guarded<HashMap<NodeId, HashSet<NodeId>>>,
    pub(super) external_bindings:
        Guarded<BTreeMap<(PackageId, BindingName), ExternalBindingAccess>>,
    pub(super) provenance: bool,
    pub(super) root: PackageId,
    pub(super) declared_dependencies: Guarded<HashMap<PackageId, Arc<DeclaredDependencies>>>,
    pub(super) namespace_imports: Guarded<HashMap<PackageId, Arc<NamespaceImports>>>,
    pub(super) non_returning_bindings: Guarded<HashMap<PackageId, Arc<BTreeSet<BindingName>>>>,
    pub(super) shadow_bases: Guarded<HashMap<PackageId, ShadowBase>>,
    pub(super) root_description: Option<Arc<str>>,
}

pub(super) struct LoadedPackage {
    pub(super) image: RwLock<Arc<PackageImage>>,
    pub(super) namespace: Guarded<NamespaceBuilder>,
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
        let packages = TargetUniverse::new(
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
            schedule: options.schedule,
            graph: Guarded::default(),
            roots: Guarded::default(),
            work: Machine::default(),
            failure: Guarded::default(),
            encountered: Guarded::new(HashSet::from([root])),
            external: Guarded::default(),
            parses: Guarded::default(),
            loaded: RwLock::new(HashMap::new()),
            load_gate: Guarded::default(),
            objects: ObjectWorld::default(),
            diagnostics: Guarded::default(),
            relocations: Guarded::default(),
            s3: Guarded::default(),
            invocations: Guarded::default(),
            value_closures: Guarded::default(),
            construction_calls: Guarded::default(),
            summaries: SummaryTable::default(),
            construction_evaluations: AtomicUsize::new(0),
            reflection: Guarded::default(),
            dynamic_names: Guarded::default(),
            dependencies: Guarded::default(),
            external_bindings: Guarded::default(),
            provenance: options.provenance,
            root,
            declared_dependencies: Guarded::default(),
            namespace_imports: Guarded::default(),
            non_returning_bindings: Guarded::default(),
            shadow_bases: Guarded::default(),
            root_description: options.root_description,
        })
    }

    pub(super) fn run(self) -> Result<Self> {
        let root = self.root;
        let root_image = self.image(root)?;

        self.require_root(Need::Activation { package: root });
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

        self.settle()?;
        self.report_dynamic_namespace_operations();
        let materialized = self
            .encountered
            .lock()
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

    fn settle(&self) -> Result<()> {
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(self.jobs)
            .thread_name(|index| format!("slinker-analysis-{index}"))
            .stack_size(super::ANALYSIS_STACK_BYTES)
            .build()
            .map_err(|error| Error::Analysis(format!("failed to create Rayon pool: {error}")))?;
        loop {
            pool.install(|| rayon::scope(|scope| self.spawn_injected(scope)));
            if let Some(error) = self.failure.lock().take() {
                return Err(error);
            }
            debug_assert!(self.work.is_quiescent());
            if !self.settle_namespace_operations()? && self.work.is_quiescent() {
                return Ok(());
            }
        }
    }

    fn spawn_injected<'scope>(&'scope self, scope: &rayon::Scope<'scope>) {
        let mut keys = self
            .work
            .take_injected()
            .into_iter()
            .filter_map(|key| match key {
                WorkKey::Need(need) => Some(need),
                WorkKey::Seal(_) => None,
            })
            .collect::<Vec<_>>();
        if keys.is_empty() {
            return;
        }
        self.schedule.arrange(&mut keys);
        profile::max(Counter::QueueDepthMax, self.work.pending() as u64);
        let origin = rayon::current_thread_index();
        for key in keys {
            scope.spawn(move |scope| {
                if origin != rayon::current_thread_index() {
                    profile::count(Counter::TaskSteals);
                }
                self.run_work(key, scope);
            });
        }
    }

    fn run_work<'scope>(&'scope self, need: Need, scope: &rayon::Scope<'scope>) {
        let started = std::time::Instant::now();
        let key = WorkKey::Need(need.clone());
        if self.work.begin(&key) {
            self.summaries.reset_thread();
            if self.failure.lock().is_none()
                && let Err(error) = self.process_need(need)
            {
                self.failure.lock().get_or_insert(error);
            }
            self.work.publish_staged();
            self.spawn_injected(scope);
            self.work.release_claim(&key);
        }
        self.spawn_injected(scope);
        profile::add(
            Counter::WorkerBusyMicros,
            u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX),
        );
    }

    pub(super) fn loaded(&self, package: PackageId) -> Result<Arc<LoadedPackage>> {
        if let Some(loaded) = self.loaded.read().expect("loaded packages").get(&package) {
            return Ok(Arc::clone(loaded));
        }
        let _loading = self.load_gate.lock();
        if let Some(loaded) = self.loaded.read().expect("loaded packages").get(&package) {
            return Ok(Arc::clone(loaded));
        }
        let index = self.packages.index(package)?;
        let image = Arc::new(PackageImage {
            index: Arc::clone(&index),
            bindings: HashMap::new(),
            private_environments: HashMap::new(),
        });
        self.objects.merge(package, &image);
        let loaded = Arc::new(LoadedPackage {
            image: RwLock::new(image),
            namespace: Guarded::new(NamespaceBuilder::new(&index)),
            native_bindings: NativeBindingIndex::new(&index),
        });
        Ok(Arc::clone(
            self.loaded
                .write()
                .expect("loaded packages")
                .entry(package)
                .or_insert(loaded),
        ))
    }

    pub(super) fn seal_namespace(&self, package: PackageId) -> Result<()> {
        let key = WorkKey::Seal(package);
        match self.work.claim(&key) {
            Claim::Mine => {
                let analyzed = self.ensure_on_load_analyzed(package);
                self.work.release_claim(&key);
                analyzed
            }
            Claim::AlreadyDone => Ok(()),
            Claim::Wait => {
                self.work.wait_done(&key);
                Ok(())
            }
        }
    }

    pub(super) fn image(&self, package: PackageId) -> Result<Arc<PackageImage>> {
        Ok(Arc::clone(
            &self.loaded(package)?.image.read().expect("package image"),
        ))
    }

    pub(super) fn binding_image(
        &self,
        package: PackageId,
        binding: &str,
        reason: Counter,
    ) -> Result<Arc<PackageImage>> {
        let _span = profile::span(Probe::BindingImage);
        let image = self.image(package)?;
        if image.binding(binding).is_some() || !image.index.binding_names.contains(binding) {
            return Ok(image);
        }
        profile::count(reason);
        let partial = {
            let _store = profile::span(Probe::StoreBindingImage);
            self.packages.binding_image(package, binding)?
        };
        {
            let _merge = profile::span(Probe::ObjectsMerge);
            self.objects.merge(package, &partial);
        }
        let sources = partial
            .bindings
            .values()
            .map(|binding| &binding.object)
            .chain(
                partial
                    .private_environments
                    .values()
                    .flat_map(|environment| {
                        environment.bindings.values().map(|binding| &binding.object)
                    }),
            )
            .flat_map(ObjectImage::closure_sources)
            .collect::<Vec<_>>();
        self.packages.prefetch_canonical_syntax(&sources)?;
        drop(image);
        let _extend = profile::span(Probe::ImageExtend);
        let loaded = self.loaded(package)?;
        let mut guard = loaded.image.write().expect("package image");
        let image = Arc::make_mut(&mut guard);
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
                .or_insert_with(|| Arc::clone(environment));
        }
        Ok(Arc::clone(&guard))
    }

    fn process_need(&self, need: Need) -> Result<()> {
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

    fn require_root(&self, need: Need) {
        self.record_unclassified(&need);
        self.require_internal_root(need);
    }

    fn require_internal_root(&self, need: Need) {
        self.encountered.lock().insert(need.package());
        let node = self.need_node(&need);
        {
            let mut roots = self.roots.lock();
            if !roots.contains(&node) {
                roots.push(node);
            }
        }
        self.work.request(WorkKey::Need(need));
    }

    pub(super) fn require(
        &self,
        from: NodeId,
        need: Need,
        kind: EdgeKind,
        reason: impl Into<String>,
    ) {
        self.require_at(from, need, kind, reason, None);
    }

    pub(super) fn require_at(
        &self,
        from: NodeId,
        need: Need,
        kind: EdgeKind,
        reason: impl Into<String>,
        span: Option<Span>,
    ) {
        self.record_unclassified(&need);
        self.require_classified_at(from, need, kind, reason, span);
    }

    pub(super) fn record_unclassified(&self, need: &Need) {
        if let Need::Binding { package, binding } = need {
            self.invocations.lock().record_unclassified(CallableId {
                package: *package,
                binding: binding.clone(),
            });
        }
    }

    pub(super) fn require_classified_at(
        &self,
        from: NodeId,
        need: Need,
        kind: EdgeKind,
        reason: impl Into<String>,
        span: Option<Span>,
    ) {
        self.encountered.lock().insert(need.package());
        let to = self.need_node(&need);
        self.depend(from, to, kind, reason, span);
        self.work.request(WorkKey::Need(need));
    }

    pub(super) fn depend(
        &self,
        from: NodeId,
        to: NodeId,
        kind: EdgeKind,
        reason: impl Into<String>,
        span: Option<Span>,
    ) {
        self.dependencies.lock().entry(from).or_default().insert(to);
        if self.provenance {
            self.graph.lock().add_edge(from, to, kind, reason, span);
        }
    }

    pub(super) fn external_binding(
        &self,
        package: PackageId,
        name: &str,
        access: ExternalBindingAccess,
        span: Option<Span>,
    ) -> NodeId {
        {
            let mut bindings = self.external_bindings.lock();
            let recorded = bindings
                .entry((package, BindingName::from(name)))
                .or_insert(access);
            if access == ExternalBindingAccess::Internal {
                *recorded = access;
            }
        }
        self.graph.lock().add_node(
            self.packages.name(package),
            NodeKind::ExternalBinding {
                name: BindingName::from(name),
            },
            span,
        )
    }

    pub(super) fn need_node(&self, need: &Need) -> NodeId {
        let package = self.packages.name(need.package());
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
        self.graph.lock().add_node(package, kind, None)
    }

    pub(super) fn diagnostic(
        &self,
        node: NodeId,
        package: PackageId,
        binding: Option<&str>,
        code: RejectCode,
        message: impl Into<String>,
        span: Option<Span>,
    ) {
        let diagnostic = self.new_diagnostic(node, package, binding, code, message.into(), span);
        self.diagnostics.lock().record(node, diagnostic);
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
            package: self.packages.name(package),
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
        &self,
        from: NodeId,
        requester: PackageId,
        missing: &str,
        kind: EdgeKind,
        reason: impl Into<String>,
        span: Option<Span>,
    ) {
        let reason = reason.into();
        let node = {
            let mut graph = self.graph.lock();
            let node = graph.add_node(
                PackageName::from(missing),
                NodeKind::MissingPackage,
                span.clone(),
            );
            graph.add_edge(from, node, kind, reason.clone(), span.clone());
            node
        };
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
            package: self.packages.name(requester),
            binding: self.node_binding(from),
            span,
            detail: reason,
        };
        self.diagnostics.lock().record_derived(
            Cause::MissingPackage(PackageName::from(missing)),
            Diagnostic {
                package: PackageName::from(missing),
                ..primary
            },
            evidence,
        );
    }

    fn node_binding(&self, node: NodeId) -> Option<BindingName> {
        match &self.graph.lock().nodes[node.0].kind {
            NodeKind::Binding { name } | NodeKind::PrivateBinding { name, .. } => {
                Some(name.clone())
            }
            _ => None,
        }
    }
}

impl<P: PackageProvider> AnalyzerState<P> {
    pub(super) fn namespace_declares(&self, package: PackageId, name: &str) -> bool {
        self.loaded
            .read()
            .expect("loaded packages")
            .get(&package)
            .is_some_and(|loaded| loaded.namespace.lock().contains(name))
    }

    pub(super) fn loaded_packages(&self) -> Vec<PackageId> {
        self.loaded
            .read()
            .expect("loaded packages")
            .keys()
            .copied()
            .collect()
    }
}
