use super::arguments::{
    declared_strings, matched_call_arg_index, matched_static_arg, namespace_formals,
    namespace_target, native_selector_span, only_package_argument, reflective_name_formals,
    static_package_arg, static_string_arg,
};
use super::diagnostic::{Cause, DiagnosticSink, Evidence};
use super::dynamic_names::{CreatedName, DynamicNames, NameCreator};
use super::execute::{AbstractValue, ConstructionCallKey, ExecutionContext};
use super::invocation::{InvocationModel, PinnedUse};
use super::namespace::{NamespaceBuilder, OptionalRegistration};
use super::need::{NeedQueue, Popped};
use super::object_world::{ClosureId, ObjectId, ObjectWorld};
use super::parse_cache::{ParseCache, ParseKey, ParseState};
use super::reflection::ReflectionFacts;
use super::relocation::{NamespaceCall, PendingRelocation, RelocationPlan, SyntaxObservation};
use super::resolution::{BindingTarget, OpenReason, ReferenceUse, Resolution};
use super::s3::{CallableId, S3Model, callable_target};
use crate::analysis::{
    Diagnostic, EdgeKind, GenericId, Graph, LifecycleHook, Need, NodeId, NodeKind, RejectCode, S3Id,
};
use crate::ir::ExternalBindingAccess;
use crate::ir::NamespaceOperation;
use crate::metadata::{RelationField, relations};
use crate::package::{
    BindingImage, BindingName, BindingRepresentation, CanonicalSyntax, ClosureSource,
    ComponentName, DatasetName, Digest, ImportSpec, NativeLibrary, NativeSafety, ObjectKind,
    PackageId, PackageImage, PackageProvider, PrivateBindingImage, ResourcePath, SyntaxValidation,
    TargetUniverse,
};
use crate::syntax::{
    ActiveBindingDef, CallSite, CalleeKind, NameRefKind, NamespaceImports, NamespaceInfoReceiver,
    OakParseContext, OakParser, PackageGuard, ParsedExpression, ParsedRFile, ResourcePackage,
    SemanticIssueKind, SourceId, SourceKey, Span, StaticArg, StaticEnvironment, SyntaxEffect,
    SyntaxEffectKind,
};
use crate::{Error, Result};
use rayon::prelude::*;
use std::borrow::Cow;
use std::collections::hash_map::Entry;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::Arc;

#[derive(Clone, Copy)]
pub(super) struct ParsedSite<'a> {
    pub(super) node: NodeId,
    pub(super) package: PackageId,
    pub(super) image: &'a PackageImage,
    pub(super) binding: &'a str,
    pub(super) lexical_environment: &'a str,
}

#[derive(Clone, Copy)]
pub(super) struct ParseRequest<'a> {
    pub(super) owner_binding: &'a str,
    pub(super) source_key: &'a SourceKey,
    pub(super) owner_node: NodeId,
}

#[derive(Clone, Copy)]
pub(super) struct NativeCallbackContext<'a> {
    pub(super) owner: NodeId,
    pub(super) package: PackageId,
    pub(super) image: &'a PackageImage,
    pub(super) binding: &'a str,
    pub(super) lexical_environment: &'a str,
    pub(super) component: &'a str,
    pub(super) parsed: &'a ParsedRFile,
    pub(super) call: &'a CallSite,
}

pub(super) struct AnalysisOptions {
    pub(super) jobs: usize,
    pub(super) provenance: bool,
    pub(super) linked_packages: HashSet<String>,
    pub(super) explicit_external_packages: HashSet<String>,
    pub(super) root_description: Option<Arc<str>>,
}

pub(crate) struct AnalyzerState<P: PackageProvider> {
    pub(super) packages: TargetUniverse<P>,
    pub(super) linked_packages: HashSet<String>,
    pub(super) explicit_external_packages: HashSet<String>,
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
    declared_dependencies: HashMap<PackageId, DeclaredDependencies>,
    pub(super) namespace_imports: HashMap<PackageId, NamespaceImports>,
    pub(super) non_returning_bindings: HashMap<PackageId, BTreeSet<String>>,
    pub(super) root_description: Option<Arc<str>>,
}

pub(super) struct LoadedPackage {
    pub(super) image: Arc<PackageImage>,
    pub(super) namespace: NamespaceBuilder,
}

pub(super) struct NativeCallTarget {
    pub(super) component: String,
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

    pub(super) fn process_frontier(&mut self) -> Result<()> {
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
        let image = self.image(package)?;
        if image.binding(binding).is_some()
            || !image.index.binding_names.iter().any(|name| name == binding)
        {
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

    pub(super) fn parse_pool(&mut self) -> Result<Option<Arc<rayon::ThreadPool>>> {
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

    pub(super) fn preparse_frontier_bindings(&mut self, frontier: usize) -> Result<()> {
        struct Work {
            key: ParseKey,
            owner_binding: String,
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
            let (id, owner_binding, source_key, closure, owner_node, image) = match need {
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
                    let Some(closure) = binding_image.closure else {
                        continue;
                    };
                    let owner_node = self.need_node(&Need::Binding {
                        package: id,
                        binding: binding.clone(),
                    });
                    (
                        id,
                        binding.to_string(),
                        SourceKey::Binding(binding.into_string()),
                        closure,
                        owner_node,
                        image,
                    )
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
                    let Some(closure) = binding_image.closure else {
                        continue;
                    };
                    let owner_node = self.need_node(&Need::PrivateBinding {
                        package: id,
                        environment: environment.clone(),
                        binding: binding.clone(),
                    });
                    let source_key = Self::private_source_key(&environment, &binding);
                    (
                        id,
                        source_key.to_string(),
                        source_key,
                        closure,
                        owner_node,
                        image,
                    )
                }
                Need::ClosureExecution {
                    package: id,
                    closure,
                } => {
                    if self.packages.is_external(id) {
                        continue;
                    }
                    let image = self.image(id)?;
                    let Some((closure_object, owner_source, source_key, environment)) =
                        self.closure_execution_source(id, closure)
                    else {
                        continue;
                    };
                    let owner_node = self.need_node(&Need::ClosureExecution {
                        package: id,
                        closure,
                    });
                    (
                        id,
                        owner_source.to_string(),
                        source_key,
                        ClosureSource {
                            source: closure_object.source,
                            environment,
                        },
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
                self.admit_source(id, &owner_binding, &source_key, owner_node, &closure.source)?
            else {
                continue;
            };
            work.push(Work {
                key,
                owner_binding,
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
                        &item.owner_binding,
                        &item.source_key,
                        item.owner_node,
                        &error,
                    )?;
                }
            }
        }
        Ok(())
    }

    pub(super) fn process_need(&mut self, need: Need) -> Result<()> {
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

    pub(super) fn process_closure_execution(
        &mut self,
        id: PackageId,
        closure: ClosureId,
    ) -> Result<()> {
        let need = Need::ClosureExecution {
            package: id,
            closure,
        };
        let node = self.need_node(&need);
        if self.packages.is_external(id) {
            self.external.insert(id);
            return Ok(());
        }
        let image = self.image(id)?;
        let Some((closure_object, owner_source, source_key, environment)) =
            self.closure_execution_source(id, closure)
        else {
            self.diagnostic(
                node,
                id,
                None,
                RejectCode::UnsupportedObject,
                "executable closure is missing from the package object graph",
                None,
            );
            return Ok(());
        };
        let owner_name = owner_source.to_string();

        if environment.starts_with("unsupported:") {
            self.diagnostic(
                node,
                id,
                Some(&owner_name),
                RejectCode::UnknownClosureEnclosure,
                format!("executable closure has unknown enclosure `{environment}`"),
                None,
            );
        }
        if let Some(parsed) = self.parsed_source(
            id,
            &closure_object.source,
            &image,
            &environment,
            ParseRequest {
                owner_binding: &owner_name,
                source_key: &source_key,
                owner_node: node,
            },
        )? {
            let image =
                self.prepare_construction_image(id, &image, &environment, parsed.as_ref())?;
            self.process_parsed(node, id, &image, &owner_name, &environment, parsed.as_ref())?;
        }
        Ok(())
    }

    pub(super) fn process_binding(&mut self, id: PackageId, binding: &BindingName) -> Result<()> {
        let node = self.need_node(&Need::Binding {
            package: id,
            binding: binding.clone(),
        });
        if self.packages.is_external(id) {
            self.external.insert(id);
            return Ok(());
        }
        let image = self.binding_image(id, binding)?;
        let Some(binding_image) = image.binding(binding).cloned() else {
            return self.process_absent_binding(node, id, &image, binding);
        };
        self.diagnose_binding_object(node, id, binding, &binding_image);
        let object = self.objects.graph(id).namespace_binding(binding);
        self.require_member_closures(node, id, object);
        if let Some(closure) = &binding_image.closure {
            self.process_binding_closure(node, id, &image, binding, &binding_image, closure)?;
        }
        Ok(())
    }

    fn process_absent_binding(
        &mut self,
        node: NodeId,
        id: PackageId,
        image: &Arc<PackageImage>,
        binding: &str,
    ) -> Result<()> {
        if binding != ".onLoad" && image.index.lifecycle.on_load {
            self.ensure_on_load_analyzed(id)?;
        }
        if self
            .loaded
            .get(&id)
            .is_some_and(|loaded| loaded.namespace.contains(binding))
            && !image.index.binding_names.iter().any(|name| name == binding)
        {
            let lifecycle = self.need_node(&Need::Lifecycle {
                package: id,
                hook: LifecycleHook::OnLoad,
            });
            self.depend(
                node,
                lifecycle,
                EdgeKind::Lifecycle,
                format!("activation creates active binding `{binding}`"),
                None,
            );
            return Ok(());
        }
        if self.is_root(id)
            && image
                .index
                .exports
                .values()
                .any(|exported_binding| exported_binding == binding)
            && self.process_root_reexport(node, id, image, binding)?
        {
            return Ok(());
        }
        self.diagnostic(
            node,
            id,
            Some(binding),
            RejectCode::UnresolvedBinding,
            format!("installed namespace has no binding `{binding}`"),
            None,
        );
        Ok(())
    }

    fn process_root_reexport(
        &mut self,
        node: NodeId,
        id: PackageId,
        image: &Arc<PackageImage>,
        binding: &str,
    ) -> Result<bool> {
        let resolved = self.resolve_name(id, image, binding)?;
        match resolved {
            Resolution::Static(BindingTarget::Imported {
                package,
                binding: foreign_binding,
            }) => {
                self.require(
                    node,
                    Need::Activation { package },
                    EdgeKind::Export,
                    format!("root re-export `{binding}` requires namespace activation"),
                );
                self.require(
                    node,
                    Need::Binding { package, binding: foreign_binding.clone() },
                    EdgeKind::Export,
                    format!("root re-export `{binding}` resolves to imported binding `{foreign_binding}`"),
                );
                return Ok(true);
            }
            Resolution::Static(BindingTarget::External {
                package,
                binding: foreign_binding,
            }) => {
                let external = self.external_binding(
                    package,
                    &foreign_binding,
                    ExternalBindingAccess::Exported,
                    None,
                );
                self.depend(
                    node,
                    external,
                    EdgeKind::Export,
                    format!("root re-export `{binding}` resolves to External `{foreign_binding}`"),
                    None,
                );
                return Ok(true);
            }
            Resolution::Static(BindingTarget::Native {
                package,
                component,
                binding: native_binding,
            }) => {
                self.require(
                    node,
                    Need::Native { package, component: component.clone().into() },
                    EdgeKind::Export,
                    format!("root export `{binding}` resolves to registered native symbol `{native_binding}` in `{component}`"),
                );
                return Ok(true);
            }
            Resolution::Static(BindingTarget::Base | BindingTarget::Metadata { .. }) => {
                return Ok(true);
            }
            Resolution::OpenDynamic(OpenReason::MissingPackage {
                package,
                binding: foreign_binding,
            }) => {
                let detail = foreign_binding.as_deref().map_or_else(
                    || format!("root re-export `{binding}` requires missing namespace {package}"),
                    |name| format!("root re-export `{binding}` requires missing {package}::{name}"),
                );
                self.record_missing_package(node, id, &package, EdgeKind::Export, detail, None);
                return Ok(true);
            }
            Resolution::OpenDynamic(OpenReason::Unresolved(name)) => {
                self.diagnostic(
                    node,
                    id,
                    Some(binding),
                    RejectCode::UnresolvedBinding,
                    format!("exported name `{binding}` resolves to unknown binding `{name}`"),
                    None,
                );
                return Ok(true);
            }
            Resolution::Static(
                BindingTarget::Namespace { .. }
                | BindingTarget::Private { .. }
                | BindingTarget::Closure { .. }
                | BindingTarget::Local,
            ) => {}
        }
        Ok(false)
    }

    fn diagnose_binding_object(
        &mut self,
        node: NodeId,
        id: PackageId,
        binding: &str,
        binding_image: &BindingImage,
    ) {
        let object_issues = binding_image
            .issues
            .iter()
            .filter(|issue| {
                !(issue.kind == "environment_identity" && issue.path.ends_with(".environment"))
            })
            .map(|issue| format!("{}: {} ({})", issue.path, issue.kind, issue.detail))
            .collect::<Vec<_>>();
        if !object_issues.is_empty() {
            self.diagnostic(
                node,
                id,
                Some(binding),
                RejectCode::UnsupportedObject,
                object_issues.join("; "),
                None,
            );
        }
        if matches!(
            binding_image.representation,
            BindingRepresentation::ActiveBinding
        ) {
            self.diagnostic(
                node,
                id,
                Some(binding),
                RejectCode::ActiveBinding,
                "active binding is preserved without execution",
                None,
            );
        }
        match &binding_image.object_kind {
            ObjectKind::Other(kind) => self.diagnostic(
                node,
                id,
                Some(binding),
                RejectCode::UnsupportedObject,
                format!("unsupported installed object type `{kind}`"),
                None,
            ),
            ObjectKind::Unavailable => self.diagnostic(
                node,
                id,
                Some(binding),
                RejectCode::UnsupportedObject,
                "installed binding could not be forced",
                None,
            ),
            _ => {}
        }
    }

    fn process_binding_closure(
        &mut self,
        node: NodeId,
        id: PackageId,
        image: &Arc<PackageImage>,
        binding: &str,
        binding_image: &BindingImage,
        closure: &ClosureSource,
    ) -> Result<()> {
        if closure.environment.starts_with("unsupported:") {
            self.diagnostic(
                node,
                id,
                Some(binding),
                RejectCode::UnknownClosureEnclosure,
                format!(
                    "closure enclosure `{}` cannot be modeled",
                    closure.environment
                ),
                None,
            );
        }
        if let Some(parsed) = self.parsed(id, binding, image, binding_image)? {
            if binding == ".onLoad"
                && self.packages.role(id) == crate::package::PackageRole::Linked
                && parsed.expressions.first().is_some_and(|expression| {
                    expression
                        .parameters
                        .first()
                        .is_some_and(|libname| expression.used_parameters.contains(libname))
                })
            {
                self.diagnostic(
                    node,
                    id,
                    Some(binding),
                    RejectCode::UnsupportedLinkedLibname,
                    "Linked .onLoad reads libname, which has no installed library once linked",
                    None,
                );
            }
            let image =
                self.prepare_construction_image(id, image, &closure.environment, parsed.as_ref())?;
            self.process_parsed(
                node,
                id,
                &image,
                binding,
                &closure.environment,
                parsed.as_ref(),
            )?;
        }
        Ok(())
    }

    fn require_member_closures(&mut self, node: NodeId, id: PackageId, object: Option<ObjectId>) {
        let graph = self.objects.graph(id);
        let closures = object
            .and_then(|object| graph.members_of(object))
            .into_iter()
            .flat_map(|members| members.values())
            .filter_map(|member| graph.closure_of(*member))
            .collect::<Vec<_>>();
        for closure in closures {
            let need = Need::ClosureExecution {
                package: id,
                closure,
            };
            let closure_node = self.need_node(&need);
            self.value_closures.insert(closure_node);
            self.require(
                node,
                need,
                EdgeKind::ClosureExecution,
                "a retained value holds an executable closure",
            );
        }
    }

    pub(super) fn process_private_binding(
        &mut self,
        id: PackageId,
        environment: &str,
        binding: &BindingName,
    ) -> Result<()> {
        let node = self.need_node(&Need::PrivateBinding {
            package: id,
            environment: environment.to_owned(),
            binding: binding.clone(),
        });
        if self.packages.is_external(id) {
            self.external.insert(id);
            return Ok(());
        }
        let image = self.image(id)?;
        let Some(binding_image) = image.private_binding(environment, binding).cloned() else {
            self.diagnostic(
                node,
                id,
                Some(binding),
                RejectCode::UnresolvedBinding,
                format!("private environment `{environment}` has no binding `{binding}`"),
                None,
            );
            return Ok(());
        };

        self.diagnose_private_object(node, id, environment, binding, &binding_image);
        let object = {
            let graph = self.objects.graph(id);
            graph.environment_id(environment).and_then(|private| {
                graph
                    .environment(private)
                    .bindings
                    .get(binding.as_str())
                    .copied()
            })
        };
        self.require_member_closures(node, id, object);

        let source_key = Self::private_source_key(environment, binding);
        let source_name = source_key.to_string();
        if let Some(closure) = &binding_image.closure {
            if closure.environment.starts_with("unsupported:") {
                self.diagnostic(
                    node,
                    id,
                    Some(binding),
                    RejectCode::UnknownClosureEnclosure,
                    format!(
                        "private closure enclosure `{}` cannot be modeled",
                        closure.environment
                    ),
                    None,
                );
            }
            if let Some(parsed) = self.parsed_source(
                id,
                &closure.source,
                &image,
                &closure.environment,
                ParseRequest {
                    owner_binding: &source_name,
                    source_key: &source_key,
                    owner_node: node,
                },
            )? {
                let image = self.prepare_construction_image(
                    id,
                    &image,
                    &closure.environment,
                    parsed.as_ref(),
                )?;
                self.process_parsed(
                    node,
                    id,
                    &image,
                    &source_name,
                    &closure.environment,
                    parsed.as_ref(),
                )?;
            }
        }

        Ok(())
    }

    pub(super) fn diagnose_private_object(
        &mut self,
        node: NodeId,
        id: PackageId,
        environment: &str,
        binding: &str,
        image: &PrivateBindingImage,
    ) {
        let object_issues = image
            .issues
            .iter()
            .filter(|issue| {
                !(issue.kind == "environment_identity" && issue.path.ends_with(".environment"))
            })
            .map(|issue| format!("{}: {} ({})", issue.path, issue.kind, issue.detail))
            .collect::<Vec<_>>();
        if !object_issues.is_empty() {
            self.diagnostic(
                node,
                id,
                Some(binding),
                RejectCode::UnsupportedObject,
                format!(
                    "private binding {environment}${binding}: {}",
                    object_issues.join("; ")
                ),
                None,
            );
        }
        if matches!(image.representation, BindingRepresentation::ActiveBinding) {
            self.diagnostic(
                node,
                id,
                Some(binding),
                RejectCode::ActiveBinding,
                format!(
                    "private active binding {environment}${binding} is preserved without execution"
                ),
                None,
            );
        }
        match &image.object_kind {
            ObjectKind::Other(kind) => self.diagnostic(
                node,
                id,
                Some(binding),
                RejectCode::UnsupportedObject,
                format!(
                    "private binding {environment}${binding} has unsupported object type `{kind}`"
                ),
                None,
            ),
            ObjectKind::Unavailable => self.diagnostic(
                node,
                id,
                Some(binding),
                RejectCode::UnsupportedObject,
                format!("private binding {environment}${binding} could not be forced"),
                None,
            ),
            _ => {}
        }
    }

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
                return Ok(GuardVerdict::PrunedByUnselectedOptional(package.to_owned()));
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

    pub(super) fn process_parsed(
        &mut self,
        node: NodeId,
        package: PackageId,
        image: &PackageImage,
        binding: &str,
        lexical_environment: &str,
        parsed: &ParsedRFile,
    ) -> Result<()> {
        let site = ParsedSite {
            node,
            package,
            image,
            binding,
            lexical_environment,
        };
        self.report_semantic_issues(site, parsed);
        for expression in &parsed.expressions {
            self.execute_construction(
                ExecutionContext {
                    node,
                    package,
                    image,
                    lexical_environment,
                    depth: 0,
                    specialized: false,
                },
                &expression.construction,
            )?;
            self.register_active_bindings(site, expression)?;
            let consumed_native_selectors = self.consumed_native_selectors(site, expression)?;
            self.process_references(site, parsed, expression, &consumed_native_selectors)?;
            self.process_package_refs(site, expression)?;
            for resource in &expression.resource_refs {
                if !self.guards_active(site, &resource.guards, &resource.span)? {
                    continue;
                }
                self.resource_access(site, parsed, expression, resource)?;
            }
            self.record_non_reflective_namespace_uses(expression);
            self.process_calls(site, parsed, expression)?;
            self.process_declared_callable_calls(site, parsed, expression)?;
            self.process_namespace_info_reads(site, expression);
            self.process_namespace_enumerations(site, expression);
            self.process_effects(site, expression)?;
        }
        Ok(())
    }

    fn report_semantic_issues(&mut self, site: ParsedSite<'_>, parsed: &ParsedRFile) {
        for issue in &parsed.issues {
            let code = match issue.kind {
                SemanticIssueKind::AmbiguousEffect => RejectCode::SemanticAmbiguity,
                SemanticIssueKind::AmbiguousAttachOrder => RejectCode::SemanticAmbiguity,
                SemanticIssueKind::UninstalledPackage => RejectCode::MissingDependency,
                SemanticIssueKind::SourceCycle => RejectCode::SemanticAmbiguity,
                SemanticIssueKind::InvalidDeclaration => RejectCode::InvalidDeclaration,
            };
            self.diagnostic(
                site.node,
                site.package,
                Some(site.binding),
                code,
                issue.message.clone(),
                issue.span.clone(),
            );
        }
    }

    fn register_active_bindings(
        &mut self,
        site: ParsedSite<'_>,
        expression: &ParsedExpression,
    ) -> Result<()> {
        for active in &expression.active_bindings {
            if site.binding != ".onLoad" || !active.certain {
                continue;
            }
            if !self.guards_active(site, &active.guards, &active.span)? {
                continue;
            }
            if self.active_binding_targets_current_namespace(
                site.package,
                site.image,
                site.lexical_environment,
                active,
            )? && self
                .loaded(site.package)?
                .namespace
                .add_binding(active.name.clone().into())
            {
                self.non_returning_bindings.remove(&site.package);
            }
        }
        Ok(())
    }

    fn consumed_native_selectors(
        &mut self,
        site: ParsedSite<'_>,
        expression: &ParsedExpression,
    ) -> Result<Vec<Span>> {
        let mut consumed_native_selectors = Vec::new();
        for call in &expression.calls {
            if !self.guards_active(site, &call.guards, &call.span)?
                || !matches!(
                    call.callee.as_str(),
                    ".Call" | ".External" | ".C" | ".Fortran"
                )
                || !self.call_resolves_definitely_to_base(
                    site.package,
                    site.image,
                    site.lexical_environment,
                    call,
                )?
            {
                continue;
            }
            let Some(target) = self.native_component_for_call(
                site.package,
                site.image,
                site.lexical_environment,
                call,
            )?
            else {
                continue;
            };
            if target.consumes_selector
                && let Some(span) = native_selector_span(call)
            {
                consumed_native_selectors.push(span.clone());
            }
        }
        Ok(consumed_native_selectors)
    }

    fn process_references(
        &mut self,
        site: ParsedSite<'_>,
        parsed: &ParsedRFile,
        expression: &ParsedExpression,
        consumed_native_selectors: &[Span],
    ) -> Result<()> {
        let enclosure_known = !site.lexical_environment.starts_with("unsupported:");
        for reference in &expression.references {
            if !self.guards_active(site, &reference.guards, &reference.span)? {
                continue;
            }
            if consumed_native_selectors.contains(&reference.span) {
                continue;
            }
            if expression.namespace_info_reads.iter().any(|read| {
                read.span == reference.span
                    && read.receiver == NamespaceInfoReceiver::Lexical
                    && reproduces_namespace_info(read.field.as_deref())
            }) {
                continue;
            }
            let resolved = self.resolve_lexical_name(
                site.package,
                site.image,
                site.lexical_environment,
                &reference.name,
            )?;
            if reference.name == "environment<-"
                && matches!(resolved, Resolution::Static(BindingTarget::Base))
            {
                self.dynamic_names.observe_creator(NameCreator {
                    node: site.node,
                    package: site.package,
                    binding: site.binding.to_owned(),
                    operation: "environment<-",
                    name: CreatedName::Any,
                });
            }
            if (!enclosure_known
                || reference.kind != NameRefKind::External
                || self.value_closures.contains(&site.node))
                && matches!(
                    &resolved,
                    Resolution::OpenDynamic(OpenReason::Unresolved(_))
                )
            {
                continue;
            }
            if let Some(callable) = callable_target(&resolved)
                && !expression.calls.iter().any(|call| {
                    call.qualified_package.is_none() && call.span.start == reference.span.start
                })
            {
                let invocation =
                    self.base_apply_invocation(site, parsed, expression, &reference.span)?;
                self.record_use(callable, invocation)?;
            }
            self.require_resolved(
                site.node,
                site.package,
                Some(site.binding),
                resolved,
                reference.span.clone(),
                ReferenceUse::Recorded,
            );
        }
        Ok(())
    }

    fn process_package_refs(
        &mut self,
        site: ParsedSite<'_>,
        expression: &ParsedExpression,
    ) -> Result<()> {
        for reference in &expression.package_refs {
            if !self.guards_active(site, &reference.guards, &reference.span)? {
                continue;
            }
            self.namespace_access(site.node, site.package, reference)?;
            if let Some(foreign) = self.known_package(&reference.package)
                && !expression.calls.iter().any(|call| {
                    call.qualified_package.is_some() && call.span.start == reference.span.start
                })
            {
                self.record_escape(CallableId {
                    package: foreign,
                    binding: reference.symbol.clone().into(),
                })?;
            }
        }
        Ok(())
    }

    fn record_non_reflective_namespace_uses(&mut self, expression: &ParsedExpression) {
        let uses = expression
            .calls
            .iter()
            .filter(|call| {
                call.callee == "registerS3method"
                    || (call.callee == "exists"
                        && self.argument_text(call, "inherits") == Some("FALSE"))
            })
            .flat_map(|call| call.arg_names.iter().zip(&call.arg_spans))
            .filter(|(name, _)| name.as_deref() == Some("envir"))
            .filter_map(|(_, span)| span.clone())
            .collect();
        self.reflection.set_non_reflective_namespace_uses(uses);
    }

    fn process_calls(
        &mut self,
        site: ParsedSite<'_>,
        parsed: &ParsedRFile,
        expression: &ParsedExpression,
    ) -> Result<()> {
        for call in &expression.calls {
            if !self.guards_active(site, &call.guards, &call.span)? {
                continue;
            }
            if let Some(callable) =
                self.call_target(site.package, site.image, site.lexical_environment, call)?
            {
                self.record_invocation(parsed, callable, call)?;
            }
            self.retain_lexical_s3_methods(site, call)?;
            if matches!(call.callee.as_str(), "UseMethod" | "NextMethod")
                && call.qualified_package.is_none()
                && matches!(
                    self.resolve_lexical_name(
                        site.package,
                        site.image,
                        site.lexical_environment,
                        &call.callee,
                    )?,
                    Resolution::Static(BindingTarget::Base)
                )
            {
                self.s3_dispatch(site, parsed, Some(&expression.parameters), call)?;
                continue;
            }
            self.semantic_call(site, parsed, call)?;
        }
        Ok(())
    }

    fn process_namespace_info_reads(
        &mut self,
        site: ParsedSite<'_>,
        expression: &ParsedExpression,
    ) {
        for read in &expression.namespace_info_reads {
            if reproduces_namespace_info(read.field.as_deref()) {
                continue;
            }
            let field = read.field.as_deref().unwrap_or("<whole>");
            match &read.receiver {
                NamespaceInfoReceiver::Lexical => {}
                NamespaceInfoReceiver::Namespace(name) => {
                    let linked = self.known_package(name).is_some_and(|package| {
                        self.packages.role(package) == crate::package::PackageRole::Linked
                    });
                    if linked {
                        self.diagnostic(
                            site.node,
                            site.package,
                            Some(site.binding),
                            RejectCode::UnsupportedRootTransformation,
                            format!("reads `.__NAMESPACE__.` field `{field}`, which the synthetic `{name}` namespace does not reproduce"),
                            Some(read.span.clone()),
                        );
                    }
                }
                NamespaceInfoReceiver::Computed => {
                    self.reflection.defer_computed_namespace_info_read(
                        site.node,
                        site.package,
                        site.binding,
                        field,
                        read.span.clone(),
                    );
                }
            }
        }
    }

    fn process_namespace_enumerations(
        &mut self,
        site: ParsedSite<'_>,
        expression: &ParsedExpression,
    ) {
        for enumeration in &expression.namespace_enumerations {
            let linked = self
                .known_package(&enumeration.package)
                .is_some_and(|package| {
                    self.packages.role(package) == crate::package::PackageRole::Linked
                });
            if linked {
                self.diagnostic(
                    site.node,
                    site.package,
                    Some(site.binding),
                    RejectCode::UnsupportedRootTransformation,
                    format!(
                        "{}() reads every binding of the synthetic `{}` namespace, which holds stubs for bindings the build never reached",
                        enumeration.callee, enumeration.package
                    ),
                    Some(enumeration.span.clone()),
                );
            }
        }
    }

    fn process_effects(
        &mut self,
        site: ParsedSite<'_>,
        expression: &ParsedExpression,
    ) -> Result<()> {
        let enclosure_known = !site.lexical_environment.starts_with("unsupported:");
        for effect in &expression.effects {
            if !self.guards_active(site, &effect.guards, &effect.span)? {
                continue;
            }
            match effect.kind {
                SyntaxEffectKind::SuperAssignment => {
                    if !enclosure_known {
                        self.dynamic_names.observe_creator(NameCreator {
                            node: site.node,
                            package: site.package,
                            binding: site.binding.to_owned(),
                            operation: "<<-",
                            name: CreatedName::Any,
                        });
                        continue;
                    }
                    self.handle_superassignment(
                        site.node,
                        site.package,
                        site.image,
                        site.binding,
                        site.lexical_environment,
                        effect,
                    )?;
                }
                SyntaxEffectKind::IndirectPackageWrite
                | SyntaxEffectKind::UnsupportedAssignmentTarget => {
                    self.diagnostic(
                        site.node,
                        site.package,
                        Some(site.binding),
                        RejectCode::UnsupportedTopLevelEffect,
                        format!("unsupported R effect: {:?}", effect.kind),
                        Some(effect.span.clone()),
                    );
                }
            }
        }
        Ok(())
    }

    pub(super) fn parsed(
        &mut self,
        id: PackageId,
        binding: &str,
        package_image: &PackageImage,
        image: &BindingImage,
    ) -> Result<Option<Arc<ParsedRFile>>> {
        let closure = image.closure.as_ref().ok_or_else(|| {
            Error::Analysis(format!(
                "closure binding {}::{binding} has no source",
                self.packages.name(id)
            ))
        })?;
        let node = self.need_node(&Need::Binding {
            package: id,
            binding: binding.to_owned().into(),
        });
        self.parsed_source(
            id,
            &closure.source,
            package_image,
            &closure.environment,
            ParseRequest {
                owner_binding: binding,
                source_key: &SourceKey::Binding(binding.to_owned()),
                owner_node: node,
            },
        )
    }

    pub(super) fn parsed_source(
        &mut self,
        id: PackageId,
        source_text: &Arc<str>,
        image: &PackageImage,
        lexical_environment: &str,
        request: ParseRequest<'_>,
    ) -> Result<Option<Arc<ParsedRFile>>> {
        let ParseRequest {
            owner_binding,
            source_key,
            owner_node,
        } = request;
        let key = (id, source_key.clone());
        if let Some(state) = self.parses.state(&key) {
            return Ok(match state {
                ParseState::Parsed(parsed) => Some(Arc::clone(parsed)),
                ParseState::Blocked => None,
            });
        }
        let Some(source) =
            self.admit_source(id, owner_binding, source_key, owner_node, source_text)?
        else {
            return Ok(None);
        };
        let context = self.oak_parse_context(id, image, lexical_environment)?;
        match OakParser.parse_binding_with_context(source, source_text.as_ref(), &context) {
            Ok(parsed) => {
                let parsed = Arc::new(parsed);
                self.parses.store(key, Arc::clone(&parsed));
                Ok(Some(parsed))
            }
            Err(error) => {
                self.handle_air_rejection(id, owner_binding, source_key, owner_node, &error)?;
                Ok(None)
            }
        }
    }

    fn admit_source(
        &mut self,
        id: PackageId,
        owner_binding: &str,
        source_key: &SourceKey,
        owner_node: NodeId,
        text: &Arc<str>,
    ) -> Result<Option<SourceId>> {
        let key = (id, source_key.clone());
        let source = self
            .parses
            .register(key.clone(), self.packages.name(id), text);
        let CanonicalSyntax::Stable(normalized) = self.packages.canonical_syntax(text)? else {
            self.diagnostic(
                owner_node,
                id,
                Some(owner_binding),
                RejectCode::InvalidInstalledRepresentation,
                format!(
                    "target-R canonical source for {source_key} is not stable across parse/deparse"
                ),
                Some(Span::new(source, 0, text.len())),
            );
            self.parses.block(key);
            return Ok(None);
        };
        self.parses.record_shape(key, Digest::of(&normalized));
        Ok(Some(source))
    }

    pub(super) fn handle_air_rejection(
        &mut self,
        id: PackageId,
        owner_binding: &str,
        source_key: &SourceKey,
        owner_node: NodeId,
        air_error: &str,
    ) -> Result<()> {
        let key = (id, source_key.clone());
        let (source_id, source_text) = self.parses.registered(&key).ok_or_else(|| {
            Error::Analysis(format!(
                "missing virtual source for {}::{source_key}",
                self.packages.name(id)
            ))
        })?;
        let validation = self.packages.validate_syntax(source_text.as_ref())?;
        let span = Some(Span::new(source_id, 0, source_text.len()));
        match validation {
            SyntaxValidation::Accepted => self.diagnostic(
                owner_node,
                id,
                Some(owner_binding),
                RejectCode::AirUnsupportedSyntax,
                format!(
                    "target R accepts {source_key}; Air {air_error}; analysis of this retained closure is conservatively blocked"
                ),
                span,
            ),
            SyntaxValidation::Rejected(r_error) => self.diagnostic(
                owner_node,
                id,
                Some(owner_binding),
                RejectCode::InvalidInstalledRepresentation,
                format!(
                    "Air rejects generated source for {source_key} ({air_error}); target R also rejects it ({r_error})"
                ),
                span,
            ),
        }
        self.parses.block(key);
        Ok(())
    }

    pub(super) fn ensure_on_load_analyzed(&mut self, id: PackageId) -> Result<()> {
        if self.packages.is_external(id) || !self.image(id)?.index.lifecycle.on_load {
            return Ok(());
        }

        let lifecycle = Need::Lifecycle {
            package: id,
            hook: LifecycleHook::OnLoad,
        };
        if self.needs.start(&lifecycle) {
            self.process_lifecycle(id, LifecycleHook::OnLoad);
        }

        let hook = Need::Binding {
            package: id,
            binding: ".onLoad".into(),
        };
        if self.needs.start(&hook) {
            self.process_binding(id, &LifecycleHook::OnLoad.binding())?;
        }
        Ok(())
    }

    pub(super) fn active_binding_targets_current_namespace(
        &mut self,
        package: PackageId,
        image: &PackageImage,
        lexical_environment: &str,
        active: &ActiveBindingDef,
    ) -> Result<bool> {
        let expected = format!("namespace:{}", self.packages.name(package));
        Ok(match &active.target {
            StaticEnvironment::Namespace(name) => name == self.packages.name(package),
            StaticEnvironment::ClosureBinding(name) => {
                match self.resolve_lexical_name(package, image, lexical_environment, name)? {
                    Resolution::Static(BindingTarget::Namespace {
                        package: owner,
                        binding,
                    }) if owner == package => image
                        .binding(&binding)
                        .and_then(|binding| binding.closure.as_ref())
                        .is_some_and(|closure| closure.environment == expected),
                    Resolution::Static(BindingTarget::Private {
                        package: owner,
                        environment,
                        binding,
                    }) if owner == package => image
                        .private_binding(&environment, &binding)
                        .and_then(|binding| binding.closure.as_ref())
                        .is_some_and(|closure| closure.environment == expected),
                    _ => false,
                }
            }
        })
    }

    pub(super) fn process_activation(&mut self, id: PackageId) -> Result<()> {
        if self.packages.is_external(id) {
            self.external.insert(id);
            return Ok(());
        }
        let image = self.image(id)?;
        let index = Arc::new(image.index.clone());
        let node = self.need_node(&Need::Activation { package: id });

        for registration in &index.s3 {
            if let Some(package_name) = registration.generic.package.as_deref()
                && self.package_is_suggested_only(id, package_name)?
                && !self.optional_package_selected(package_name)
            {
                self.loaded(id)?
                    .namespace
                    .optional_registrations
                    .push(OptionalRegistration {
                        package: package_name.into(),
                        generic: registration.generic.name.clone(),
                        class: registration.class.clone(),
                        method: registration.method.clone(),
                    });
                self.require(
                    node,
                    Need::Binding {
                        package: id,
                        binding: registration.method.clone(),
                    },
                    EdgeKind::S3Registration,
                    format!(
                        "loading optional `{package_name}` registers {}/{}",
                        registration.generic, registration.class
                    ),
                );
                continue;
            }
            let generic_package = match registration.generic.package.as_deref() {
                Some(name) => match self.packages.resolve(name)? {
                    Some(id) => Some(id),
                    None => {
                        self.record_missing_package(
                            node,
                            id,
                            name,
                            EdgeKind::S3Registration,
                            format!(
                                "S3 registration requires generic `{}`",
                                registration.generic
                            ),
                            None,
                        );
                        continue;
                    }
                },
                None => None,
            };
            let registration_id = S3Id {
                generic: GenericId {
                    package: generic_package,
                    name: registration.generic.name.clone(),
                },
                class: registration.class.clone(),
                method: registration.method.clone(),
            };
            self.loaded(id)?
                .namespace
                .registrations
                .push(registration_id.clone());
            self.require(
                node,
                Need::S3Registration {
                    package: id,
                    registration: registration_id,
                },
                EdgeKind::S3Registration,
                format!(
                    "namespace activation registers {}/{}",
                    registration.generic, registration.class
                ),
            );
        }

        self.retain_s3_methods_on_activation(id)?;

        for native in &index.dynlibs {
            self.require(
                node,
                Need::Native {
                    package: id,
                    component: native.name.clone().into(),
                },
                EdgeKind::Native,
                format!("effective useDynLib requires {}", native.name),
            );
        }
        if index.lifecycle.on_load {
            self.require(
                node,
                Need::Lifecycle {
                    package: id,
                    hook: LifecycleHook::OnLoad,
                },
                EdgeKind::Lifecycle,
                "namespace activation requires .onLoad",
            );
        }
        Ok(())
    }

    pub(super) fn process_resource(
        &mut self,
        id: PackageId,
        resource: &ResourcePath,
    ) -> Result<()> {
        if self.packages.is_external(id) {
            self.external.insert(id);
            return Ok(());
        }
        let _ = self.image(id)?;
        let _present = self.packages.resource_exists(id, resource)?;
        Ok(())
    }

    pub(super) fn process_dataset(&mut self, id: PackageId, dataset: &DatasetName) -> Result<()> {
        if self.packages.is_external(id) {
            self.external.insert(id);
            return Ok(());
        }
        let image = self.image(id)?;
        let node = self.need_node(&Need::Dataset {
            package: id,
            dataset: dataset.clone(),
        });
        if !image.index.data.defines(dataset) {
            self.diagnostic(
                node,
                id,
                None,
                RejectCode::UnresolvedBinding,
                format!("dataset `{dataset}` is absent from installed image"),
                None,
            );
        }
        Ok(())
    }

    pub(super) fn process_s3(&mut self, id: PackageId, registration: &S3Id) -> Result<()> {
        let node = self.need_node(&Need::S3Registration {
            package: id,
            registration: registration.clone(),
        });
        if let Some(generic_package) = &registration.generic.package {
            self.require(
                node,
                Need::Activation {
                    package: *generic_package,
                },
                EdgeKind::S3Registration,
                format!(
                    "S3 generic `{}` requires its namespace",
                    self.generic_label(&registration.generic)
                ),
            );
        }
        self.require(
            node,
            Need::Binding {
                package: id,
                binding: registration.method.clone(),
            },
            EdgeKind::S3Registration,
            format!(
                "runtime dispatch can reach registered method `{}`",
                registration.method
            ),
        );
        if registration.generic.package.is_none() {
            let image = self.image(id)?;
            let reason = format!(
                "registering `{}` looks up its generic in the namespace",
                registration.method
            );
            match self.resolve_name(id, &image, &registration.generic.name)? {
                Resolution::Static(
                    BindingTarget::Namespace { package, binding }
                    | BindingTarget::Imported { package, binding },
                ) => {
                    if package != id {
                        self.require(
                            node,
                            Need::Activation { package },
                            EdgeKind::S3Registration,
                            reason.clone(),
                        );
                    }
                    self.require(
                        node,
                        Need::Binding { package, binding },
                        EdgeKind::S3Registration,
                        reason,
                    );
                }
                Resolution::Static(BindingTarget::External { package, binding }) => {
                    let external = self.external_binding(
                        package,
                        &binding,
                        ExternalBindingAccess::Exported,
                        None,
                    );
                    self.depend(node, external, EdgeKind::S3Registration, reason, None);
                }
                _ => {}
            }
        }
        Ok(())
    }

    pub(super) fn process_native(
        &mut self,
        id: PackageId,
        component: &ComponentName,
    ) -> Result<()> {
        if self.packages.is_external(id) {
            self.external.insert(id);
            return Ok(());
        }
        let index = self.packages.index(id)?;
        let node = self.need_node(&Need::Native {
            package: id,
            component: component.clone(),
        });
        if let Some(native) = index
            .dynlibs
            .iter()
            .find(|native| *component == native.name)
        {
            if let NativeLibrary::Unloadable { error, .. } = &native.library {
                self.diagnostic(
                    node,
                    id,
                    None,
                    RejectCode::NativeLoadFailure,
                    format!("native component `{component}` failed to load in the worker, so its registered routines are unknown: {error}"),
                    None,
                );
            }
            if !self.is_root(id) {
                match native.library.path() {
                    Some(library) => self.require(
                        node,
                        Need::Resource {
                            package: id,
                            resource: library.to_owned().into(),
                        },
                        EdgeKind::Native,
                        format!("native component `{component}` ships its compiled library"),
                    ),
                    None => self.diagnostic(
                        node,
                        id,
                        None,
                        RejectCode::MissingResource,
                        format!("native component `{component}` has no compiled library in the installed image"),
                        None,
                    ),
                }
            }

            match &native.safety {
                NativeSafety::Unanalyzed => {
                    let identity = self.packages.identity(id);
                    let message = format!(
                        "native component `{component}` has unanalyzed C-to-R callbacks; an audited SLINKER_NATIVE_SUMMARIES entry for package `{}` version `{}` image `{}` makes it analyzable",
                        identity.name, identity.version, identity.image_fingerprint.0
                    );
                    self.diagnostic(
                        node,
                        id,
                        None,
                        RejectCode::UnknownNativeEffects,
                        message,
                        None,
                    );
                }
                NativeSafety::Safe(facts) => {
                    for callback in &facts.callbacks {
                        self.require(
                            node,
                            Need::Binding {
                                package: id,
                                binding: callback.clone().into(),
                            },
                            EdgeKind::Callback,
                            format!("native component `{component}` calls R binding `{callback}`"),
                        );
                    }
                }
                NativeSafety::Summarized(_) => {}
                NativeSafety::Unsupported(issues) => self.diagnostic(
                    node,
                    id,
                    None,
                    RejectCode::UnknownNativeEffects,
                    format!(
                        "native component `{component}` has unsupported runtime effects: {}",
                        issues.join("; ")
                    ),
                    None,
                ),
            }
        } else {
            self.diagnostic(
                node,
                id,
                None,
                RejectCode::UnknownNativeLookup,
                format!("effective namespace metadata has no native component `{component}`"),
                None,
            );
        }
        Ok(())
    }

    pub(super) fn process_lifecycle(&mut self, id: PackageId, hook: LifecycleHook) {
        let node = self.need_node(&Need::Lifecycle { package: id, hook });
        self.require(
            node,
            Need::Binding {
                package: id,
                binding: hook.binding(),
            },
            EdgeKind::Lifecycle,
            format!("lifecycle hook `{hook}` must be retained"),
        );
    }

    pub(super) fn namespace_access(
        &mut self,
        from: NodeId,
        current: PackageId,
        reference: &crate::syntax::PackageRef,
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
                BindingName::from(reference.symbol.clone())
            } else {
                let index = self.packages.index(foreign)?;
                index
                    .exports
                    .get(&reference.symbol)
                    .cloned()
                    .unwrap_or_else(|| BindingName::from(reference.symbol.clone()))
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
            self.external.insert(foreign);
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
            && !index.exports.contains_key(&reference.symbol)
            && index.data.defines(&reference.symbol)
        {
            self.dataset_access(from, foreign, reference);
            return Ok(());
        }
        let binding = if reference.internal {
            BindingName::from(reference.symbol.clone())
        } else {
            index
                .exports
                .get(&reference.symbol)
                .cloned()
                .unwrap_or_else(|| BindingName::from(reference.symbol.clone()))
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
        self.relocations.push(PendingRelocation::NamespaceAccess {
            source: reference.span.clone(),
            package: foreign,
            binding,
            internal: reference.internal,
        });
        Ok(())
    }

    pub(super) fn resource_access(
        &mut self,
        site: ParsedSite<'_>,
        parsed: &ParsedRFile,
        expression: &ParsedExpression,
        resource: &crate::syntax::ResourceRef,
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
                        self.relocations.defer_dynamic_resource_lookup(
                            from,
                            current,
                            resource.span.clone(),
                        );
                        return Ok(());
                    };
                    self.invocations.pin_default(PinnedUse {
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
                    let names_linked = self.known_package(&pinned.value).is_some_and(|package| {
                        self.packages.role(package) == crate::package::PackageRole::Linked
                    });
                    if !names_linked {
                        return Ok(());
                    }
                    return self.literal_resource_access(from, current, resource, &pinned.value);
                };
                for name in names {
                    let linked = self
                        .resource_package(from, current, resource, &name)?
                        .is_some_and(|package| {
                            self.packages.role(package) == crate::package::PackageRole::Linked
                        });
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
        &mut self,
        from: NodeId,
        current: PackageId,
        resource: &crate::syntax::ResourceRef,
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
        self.relocations.push(PendingRelocation::ResourceAccess {
            source: resource.span.clone(),
            package: foreign,
            resource: path.clone(),
        });
        Ok(())
    }

    fn resource_package(
        &mut self,
        from: NodeId,
        current: PackageId,
        resource: &crate::syntax::ResourceRef,
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
            self.external.insert(foreign);
            return Ok(None);
        }
        Ok(Some(foreign))
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

    fn package_is_required(&mut self, package: PackageId, name: &str) -> Result<bool> {
        Ok(self.declared_dependencies(package)?.required.contains(name))
    }

    fn declared_dependencies(&mut self, package: PackageId) -> Result<&DeclaredDependencies> {
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

    pub(super) fn handle_superassignment(
        &mut self,
        from: NodeId,
        package: PackageId,
        image: &PackageImage,
        binding: &str,
        lexical_environment: &str,
        effect: &SyntaxEffect,
    ) -> Result<()> {
        if let Some(value) = &effect.value_symbol
            && !is_r_constant(value)
        {
            let resolved = self.resolve_lexical_name(package, image, lexical_environment, value)?;
            let opaque_native = (binding == ".onLoad"
                && matches!(resolved, Resolution::OpenDynamic(OpenReason::Unresolved(_))))
            .then(|| Self::sole_opaque_registered_native_component(&image.index))
            .flatten();
            match opaque_native {
                Some(component) => self.require_at(
                    from,
                    Need::Native {
                        package,
                        component: component.to_owned().into(),
                    },
                    EdgeKind::Native,
                    format!(
                        ".onLoad may receive registered native symbol `{value}` from `{component}`"
                    ),
                    Some(effect.span.clone()),
                ),
                None => self.require_resolved(
                    from,
                    package,
                    Some(binding),
                    resolved,
                    effect.span.clone(),
                    ReferenceUse::Unrecorded,
                ),
            }
        }

        if effect.target_enclosing_local {
            return Ok(());
        }

        let Some(target) = &effect.target else {
            self.diagnostic(
                from,
                package,
                Some(binding),
                RejectCode::EnvironmentMutation,
                "dynamic superassignment target cannot be resolved",
                Some(effect.span.clone()),
            );
            return Ok(());
        };

        match self.resolve_lexical_name(package, image, lexical_environment, target)? {
            Resolution::Static(BindingTarget::Namespace {
                package: owner,
                binding: target_binding,
            }) => self.require_at(
                from,
                Need::Binding {
                    package: owner,
                    binding: target_binding.clone(),
                },
                EdgeKind::Effect,
                format!("superassignment mutates enclosing binding `{target_binding}`"),
                Some(effect.span.clone()),
            ),
            Resolution::Static(BindingTarget::Private {
                package: owner,
                environment,
                binding: target_binding,
            }) => self.require_at(
                from,
                Need::PrivateBinding {
                    package: owner,
                    environment: environment.clone(),
                    binding: target_binding.clone(),
                },
                EdgeKind::Effect,
                format!(
                    "superassignment mutates private binding `{target_binding}` in {environment}"
                ),
                Some(effect.span.clone()),
            ),
            Resolution::Static(BindingTarget::Local)
                if lexical_environment.starts_with("derived:") => {}
            _ => self.diagnostic(
                from,
                package,
                Some(binding),
                RejectCode::EnvironmentMutation,
                format!(
                    "superassignment target `{target}` does not resolve to a mutable enclosing lexical/package/private binding"
                ),
                Some(effect.span.clone()),
            ),
        }
        Ok(())
    }

    fn namespace_info_query(
        &mut self,
        from: NodeId,
        current: PackageId,
        binding: &str,
        call: &CallSite,
    ) -> Result<()> {
        if !matches!(
            matched_static_arg(call, &["ns", "which"], "which"),
            Some(StaticArg::String(field)) if field == "path"
        ) {
            return self.namespace_argument(from, current, binding, call, &["ns", "which"], "ns");
        }
        match matched_static_arg(call, &["ns", "which"], "ns") {
            Some(StaticArg::String(name)) => {
                if self.known_package(name).is_some_and(|package| {
                    self.packages.role(package) == crate::package::PackageRole::Linked
                }) {
                    self.diagnostic(
                        from,
                        current,
                        Some(binding),
                        RejectCode::UnsupportedRootTransformation,
                        format!("{}() reads the installed path, which the synthetic `{name}` namespace does not have", call.callee),
                        Some(call.span.clone()),
                    );
                }
            }
            _ => self.diagnostic(
                from,
                current,
                Some(binding),
                RejectCode::DynamicLookup,
                format!(
                    "{}() reads namespace metadata of a dynamic namespace",
                    call.callee
                ),
                Some(call.span.clone()),
            ),
        }
        Ok(())
    }

    fn namespace_argument(
        &mut self,
        from: NodeId,
        current: PackageId,
        binding: &str,
        call: &CallSite,
        formals: &[&str],
        target: &str,
    ) -> Result<()> {
        let Some(index) = matched_call_arg_index(call, formals, target) else {
            return Ok(());
        };
        let Some(StaticArg::String(name)) = call.args.get(index).and_then(Option::as_ref) else {
            self.dynamic_package_name(from, current, binding, call);
            return Ok(());
        };
        let name = name.clone();
        match self.discovered_package(from, current, call, &name)? {
            Discovered::Linked(package) => {
                let Some(source) = call.arg_spans.get(index).cloned().flatten() else {
                    self.unrewritable_package_call(from, current, call, &name);
                    return Ok(());
                };
                if package != current {
                    self.require_at(
                        from,
                        Need::Activation { package },
                        EdgeKind::Discovery,
                        format!("{}() names Linked `{name}`", call.callee),
                        Some(call.span.clone()),
                    );
                }
                self.relocations
                    .push(PendingRelocation::NamespaceArgument { source, package });
            }
            Discovered::Missing => self.missing_package_call(from, current, call, &name),
            Discovered::Settled | Discovered::Optional => {}
        }
        Ok(())
    }

    fn installed_package_query(
        &mut self,
        from: NodeId,
        current: PackageId,
        binding: &str,
        call: &CallSite,
        formals: &[&str],
        target: &str,
    ) -> Result<()> {
        let Some(index) = matched_call_arg_index(call, formals, target) else {
            return Ok(());
        };
        let Some(StaticArg::String(name)) = call.args.get(index).and_then(Option::as_ref) else {
            self.dynamic_package_name(from, current, binding, call);
            return Ok(());
        };
        let name = name.clone();
        match self.discovered_package(from, current, call, &name)? {
            Discovered::Linked(_) => self.unrewritable_package_call(from, current, call, &name),
            Discovered::Missing => self.missing_package_call(from, current, call, &name),
            Discovered::Settled | Discovered::Optional => {}
        }
        Ok(())
    }

    fn package_description(
        &mut self,
        from: NodeId,
        current: PackageId,
        binding: &str,
        call: &CallSite,
    ) -> Result<()> {
        let formals = ["pkg", "lib.loc", "fields", "drop", "encoding"];
        let Some(index) = matched_call_arg_index(call, &formals, "pkg") else {
            return Ok(());
        };
        let Some(StaticArg::String(name)) = call.args.get(index).and_then(Option::as_ref) else {
            self.dynamic_package_name(from, current, binding, call);
            return Ok(());
        };
        let name = name.clone();
        match self.discovered_package(from, current, call, &name)? {
            Discovered::Linked(package) => match call.arg_spans.get(index).cloned().flatten() {
                Some(source) if only_package_argument(call, &["fields", "drop", "encoding"]) => {
                    self.relocations
                        .push(PendingRelocation::DescriptionArgument { source, package });
                }
                _ => self.unrewritable_package_call(from, current, call, &name),
            },
            Discovered::Missing => self.missing_package_call(from, current, call, &name),
            Discovered::Settled | Discovered::Optional => {}
        }
        Ok(())
    }

    fn loaded_query(
        &mut self,
        from: NodeId,
        current: PackageId,
        binding: &str,
        call: &CallSite,
        name: Option<&str>,
    ) -> Result<()> {
        let Some(name) = name else {
            self.dynamic_package_name(from, current, binding, call);
            return Ok(());
        };
        let imported = self
            .image(current)?
            .index
            .imports
            .iter()
            .any(|import| match import {
                ImportSpec::All { package, .. } | ImportSpec::From { package, .. } => {
                    package == name
                }
            });
        let package = if name == self.packages.name(current) {
            (!self.is_root(current)).then_some(current)
        } else if imported {
            self.packages.resolve(name)?.filter(|package| {
                self.packages.role(*package) == crate::package::PackageRole::Linked
            })
        } else {
            None
        };
        if let Some(package) = package {
            if package != current {
                self.require_at(
                    from,
                    Need::Activation { package },
                    EdgeKind::Discovery,
                    format!("{}() asks whether imported `{name}` is loaded", call.callee),
                    Some(call.span.clone()),
                );
            }
            self.relocations.push(PendingRelocation::LoadedQuery {
                source: call.span.clone(),
                package,
            });
        }
        Ok(())
    }

    fn loaded_membership(
        &mut self,
        current: PackageId,
        image: &PackageImage,
        lexical_environment: &str,
        call: &CallSite,
    ) -> Result<Option<String>> {
        let ([Some(StaticArg::String(name)), _], [None, None], [_, Some(set)]) = (
            call.args.as_slice(),
            call.arg_names.as_slice(),
            call.arg_spans.as_slice(),
        ) else {
            return Ok(None);
        };
        let base = match self.parses.text(set).unwrap_or_default().trim() {
            "base::loadedNamespaces()" => true,
            "loadedNamespaces()" => matches!(
                self.resolve_lexical_name(current, image, lexical_environment, "loadedNamespaces")?,
                Resolution::Static(BindingTarget::Base)
            ),
            _ => false,
        };
        Ok(base.then(|| name.clone()))
    }

    pub(super) fn dynamic_package_name(
        &mut self,
        from: NodeId,
        current: PackageId,
        binding: &str,
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

    fn unrewritable_package_call(
        &mut self,
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
        &mut self,
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

    fn external_callee(
        &mut self,
        current: PackageId,
        image: &PackageImage,
        lexical_environment: &str,
        call: &CallSite,
    ) -> Result<Option<PackageId>> {
        if call.callee_kind != CalleeKind::DefinitelyExternal {
            return Ok(None);
        }
        Ok(match call.qualified_package.as_deref() {
            Some(package) => self
                .known_package(package)
                .filter(|package| self.packages.is_external(*package)),
            None => match self.resolve_lexical_name(
                current,
                image,
                lexical_environment,
                &call.callee,
            )? {
                Resolution::Static(BindingTarget::External { package, binding })
                    if binding == call.callee.as_str() =>
                {
                    Some(package)
                }
                _ => None,
            },
        })
    }

    fn rlang_call(
        &mut self,
        from: NodeId,
        current: PackageId,
        binding: &str,
        call: &CallSite,
    ) -> Result<()> {
        let (rewritable, formals, target) = match call.callee.as_str() {
            "ns_env" | "ns_imports_env" => {
                return self.namespace_argument(from, current, binding, call, &["x"], "x");
            }
            "ns_exports" => {
                return self.namespace_argument(from, current, binding, call, &["ns"], "ns");
            }
            "is_installed" => (&[][..], &["pkg", "...", "version", "compare"][..], "pkg"),
            "check_installed" => (
                &["reason", "call"][..],
                &[
                    "pkg", "reason", "...", "version", "compare", "action", "call",
                ][..],
                "pkg",
            ),
            _ => return Ok(()),
        };
        let Some(index) = matched_call_arg_index(call, formals, target) else {
            return Ok(());
        };
        let Some(StaticArg::String(name)) = call.args.get(index).and_then(Option::as_ref) else {
            self.dynamic_package_name(from, current, binding, call);
            return Ok(());
        };
        let name = name.clone();
        let Some(package) = self.declared_linked_package(current, &name)? else {
            return Ok(());
        };
        if !only_package_argument(call, rewritable) {
            self.unrewritable_package_call(from, current, call, &name);
            return Ok(());
        }
        self.relocations.push(PendingRelocation::InstalledQuery {
            source: call.span.clone(),
            package,
            check: call.callee == "check_installed",
        });
        Ok(())
    }

    fn declared_linked_package(
        &mut self,
        current: PackageId,
        name: &str,
    ) -> Result<Option<PackageId>> {
        if name == self.packages.name(current) {
            return Ok((!self.is_root(current)).then_some(current));
        }
        if !self.optional_package_selected(name) && !self.package_is_required(current, name)? {
            return Ok(None);
        }
        Ok(self
            .packages
            .resolve(name)?
            .filter(|package| self.packages.role(*package) == crate::package::PackageRole::Linked))
    }
    fn utils_call(
        &mut self,
        from: NodeId,
        current: PackageId,
        binding: &str,
        call: &CallSite,
    ) -> Result<()> {
        match call.callee.as_str() {
            "packageVersion" => self.identity_query(from, current, call, true),
            "packageDescription" => self.package_description(from, current, binding, call),
            "getFromNamespace" => self.namespace_argument(
                from,
                current,
                binding,
                call,
                &["x", "ns", "pos", "envir"],
                "ns",
            ),
            "assignInNamespace" => self.namespace_argument(
                from,
                current,
                binding,
                call,
                &["x", "value", "ns", "pos", "envir"],
                "ns",
            ),
            "citation" => {
                self.installed_package_query(from, current, binding, call, &["package"], "package")
            }
            "vignette" | "help" => self.installed_package_query(
                from,
                current,
                binding,
                call,
                &["topic", "package"],
                "package",
            ),
            "data" => self.data_call(from, current, binding, call),
            _ => Ok(()),
        }
    }

    fn reflective_lookup(
        &mut self,
        site: ParsedSite<'_>,
        parsed: &ParsedRFile,
        call: &CallSite,
        formals: &[&str],
        target: &str,
    ) -> Result<()> {
        let ParsedSite {
            node: from,
            package: current,
            image,
            binding,
            lexical_environment,
        } = site;
        if call.callee == "exists"
            && self.argument_text(call, "inherits") == Some("FALSE")
            && self.argument_span(call, "envir").is_some_and(|span| {
                self.reflection.is_non_reflective_namespace_use(span)
                    && self.parses.text(span).is_some_and(|text| {
                        let text = text.trim_start_matches("base::");
                        text.starts_with("asNamespace(") || text.starts_with("getNamespace(")
                    })
            })
        {
            return Ok(());
        }
        let computed_environment = call
            .arg_names
            .iter()
            .flatten()
            .any(|name| matches!(name.as_str(), "envir" | "pos" | "where" | "frame"))
            || call.arg_names.iter().filter(|name| name.is_none()).count() > 1
                && call.callee != "do.call";
        match (
            matched_static_arg(call, formals, target),
            declared_strings(parsed, call, formals, target),
        ) {
            (Some(StaticArg::String(name)), _) if !computed_environment => {
                let name = name.clone();
                self.retain_reflective_name(
                    from,
                    current,
                    image,
                    lexical_environment,
                    &name,
                    &call.span,
                )
            }
            (Some(StaticArg::Symbol(_)), Some(names)) if !computed_environment => {
                for name in names {
                    self.retain_reflective_name(
                        from,
                        current,
                        image,
                        lexical_environment,
                        &name,
                        &call.span,
                    )?;
                }
                Ok(())
            }
            (Some(StaticArg::Symbol(_)) | None, _)
                if matches!(call.callee.as_str(), "match.fun" | "do.call")
                    && !self.builds_function_name(call, formals, target) =>
            {
                Ok(())
            }
            _ => {
                self.diagnostic(
                    from,
                    current,
                    Some(binding),
                    RejectCode::DynamicLookup,
                    format!(
                        "{}() looks up a name that is not a static string in the calling scope",
                        call.callee
                    ),
                    Some(call.span.clone()),
                );
                Ok(())
            }
        }
    }

    fn argument_span<'a>(&self, call: &'a CallSite, name: &str) -> Option<&'a Span> {
        call.arg_names
            .iter()
            .position(|argument| argument.as_deref() == Some(name))
            .and_then(|index| call.arg_spans.get(index)?.as_ref())
    }

    fn argument_text(&self, call: &CallSite, name: &str) -> Option<&str> {
        let span = self.argument_span(call, name)?;
        Some(self.parses.text(span)?.trim())
    }

    fn builds_function_name(&self, call: &CallSite, formals: &[&str], target: &str) -> bool {
        matched_call_arg_index(call, formals, target)
            .and_then(|index| call.arg_spans.get(index)?.as_ref())
            .and_then(|span| {
                let text = self.parses.text(span)?;
                Some(
                    ["paste0(", "paste(", "sprintf(", "as.character("]
                        .iter()
                        .any(|builder| text.starts_with(builder)),
                )
            })
            .unwrap_or(false)
    }

    pub(super) fn retain_reflective_name(
        &mut self,
        from: NodeId,
        current: PackageId,
        image: &PackageImage,
        lexical_environment: &str,
        name: &str,
        span: &Span,
    ) -> Result<()> {
        let resolved = self.resolve_lexical_name(current, image, lexical_environment, name)?;
        if matches!(resolved, Resolution::OpenDynamic(OpenReason::Unresolved(_))) {
            return Ok(());
        }
        self.require_resolved(
            from,
            current,
            None,
            resolved,
            span.clone(),
            ReferenceUse::Unrecorded,
        );
        Ok(())
    }

    pub(super) fn is_slinker_semantic_callee(name: &str) -> bool {
        matches!(
            name,
            "library"
                | "require"
                | "requireNamespace"
                | "loadNamespace"
                | "getNamespace"
                | "asNamespace"
                | "packageVersion"
                | "find.package"
                | "system.file"
                | "getNamespaceImports"
                | "getNamespaceInfo"
                | ".Call"
                | ".External"
                | ".C"
                | ".Fortran"
                | "deparse"
                | "substitute"
                | "match.call"
                | "isNamespaceLoaded"
                | "getNamespaceExports"
                | "getNamespaceName"
                | "getNamespaceVersion"
                | "getExportedValue"
                | "attachNamespace"
                | "unloadNamespace"
                | "path.package"
                | "library.dynam"
                | "setHook"
                | "packageEvent"
                | "makeActiveBinding"
                | "environment"
                | "UseMethod"
                | "NextMethod"
        )
    }

    pub(super) fn semantic_call(
        &mut self,
        site: ParsedSite<'_>,
        parsed: &ParsedRFile,
        call: &CallSite,
    ) -> Result<()> {
        let ParsedSite {
            node: from,
            package: current,
            image,
            binding,
            lexical_environment,
        } = site;
        if !self.semantic_callee_is_base(
            from,
            current,
            image,
            binding,
            lexical_environment,
            call,
        )? {
            if let Some(package) =
                self.external_callee(current, image, lexical_environment, call)?
            {
                match self.packages.name(package) {
                    "utils" => self.utils_call(from, current, binding, call)?,
                    "rlang" => self.rlang_call(from, current, binding, call)?,
                    _ => {}
                }
            } else if self.is_search_path_data_call(current, image, lexical_environment, call)? {
                self.data_call(from, current, binding, call)?;
            }
            return Ok(());
        }
        if let Some((operation, name)) = created_name(call) {
            self.dynamic_names.observe_creator(NameCreator {
                node: site.node,
                package: current,
                binding: binding.to_owned(),
                operation,
                name,
            });
        }
        if let Some((formals, target)) = reflective_name_formals(&call.callee) {
            return self.reflective_lookup(site, parsed, call, formals, target);
        }
        match call.callee.as_str() {
            "library" | "require" => {
                self.attachment_call(from, current, call)?;
            }
            "requireNamespace" => {
                self.namespace_operation(
                    from,
                    current,
                    binding,
                    parsed,
                    call,
                    NamespaceCall::Require,
                )?;
            }
            "loadNamespace" => {
                self.namespace_operation(
                    from,
                    current,
                    binding,
                    parsed,
                    call,
                    NamespaceCall::Operation(NamespaceOperation::Load),
                )?;
            }
            "getNamespace" => {
                self.namespace_operation(
                    from,
                    current,
                    binding,
                    parsed,
                    call,
                    NamespaceCall::Operation(NamespaceOperation::Get),
                )?;
            }
            "asNamespace" => {
                self.namespace_operation(
                    from,
                    current,
                    binding,
                    parsed,
                    call,
                    NamespaceCall::Operation(NamespaceOperation::As),
                )?;
            }
            "getNamespaceImports" => {
                self.namespace_argument(from, current, binding, call, &["ns"], "ns")?;
            }
            "getNamespaceInfo" => {
                self.namespace_info_query(from, current, binding, call)?;
            }
            "getExportedValue" => {
                self.namespace_argument(from, current, binding, call, &["ns", "name"], "ns")?;
            }
            "getNamespaceExports" | "getNamespaceName" | "getNamespaceVersion" => {
                self.namespace_argument(from, current, binding, call, &["ns"], "ns")?;
            }
            "isNamespaceLoaded" => {
                let name = match matched_static_arg(call, &["name"], "name") {
                    Some(StaticArg::String(name)) => Some(name.clone()),
                    _ => None,
                };
                self.loaded_query(from, current, binding, call, name.as_deref())?;
            }
            "%in%" => {
                if let Some(name) =
                    self.loaded_membership(current, image, lexical_environment, call)?
                {
                    self.loaded_query(from, current, binding, call, Some(&name))?;
                }
            }
            "attachNamespace" | "unloadNamespace" => {
                self.installed_package_query(from, current, binding, call, &["ns"], "ns")?;
            }
            "path.package" => {
                self.installed_package_query(
                    from,
                    current,
                    binding,
                    call,
                    &["package"],
                    "package",
                )?;
            }
            "library.dynam" => {
                self.installed_package_query(
                    from,
                    current,
                    binding,
                    call,
                    &["chname", "package"],
                    "package",
                )?;
            }
            "find.package" => self.identity_query(from, current, call, false)?,
            "UseMethod" | "NextMethod" => {
                self.s3_dispatch(site, parsed, None, call)?;
            }
            ".Call" | ".External" | ".C" | ".Fortran" => {
                self.native_call(site, parsed, call)?;
            }
            "getNativeSymbolInfo" if !self.is_root(current) => {
                self.linked_native_symbol_query(from, current, image, binding, call);
            }
            "deparse" | "substitute" | "match.call" => {
                self.relocations.observe(SyntaxObservation {
                    node: from,
                    package: current,
                    span: call.span.clone(),
                    kind: call.callee.clone(),
                });
            }
            _ => {}
        }
        Ok(())
    }

    fn semantic_callee_is_base(
        &mut self,
        from: NodeId,
        current: PackageId,
        image: &PackageImage,
        binding: &str,
        lexical_environment: &str,
        call: &CallSite,
    ) -> Result<bool> {
        if lexical_environment.starts_with("unsupported:") && call.qualified_package.is_none() {
            return Ok(false);
        }
        match call.callee_kind {
            CalleeKind::DefinitelyLexical => return Ok(false),
            CalleeKind::ConditionalFallthrough => {
                if call.qualified_package.is_none()
                    && Self::is_slinker_semantic_callee(&call.callee)
                    && matches!(
                        self.resolve_lexical_name(
                            current,
                            image,
                            lexical_environment,
                            &call.callee
                        )?,
                        Resolution::Static(BindingTarget::Base)
                    )
                {
                    self.diagnostic(
                        from,
                        current,
                        Some(binding),
                        RejectCode::SemanticAmbiguity,
                        format!(
                            "conditionally local callee `{}` can fall through to base; linker-specific effects are path-dependent",
                            call.callee
                        ),
                        Some(call.span.clone()),
                    );
                }
                return Ok(false);
            }
            CalleeKind::DefinitelyExternal => {}
        }
        match call.qualified_package.as_deref() {
            Some("base") => {}
            Some(_) => return Ok(false),
            None => {
                let resolved =
                    self.resolve_lexical_name(current, image, lexical_environment, &call.callee)?;
                if !matches!(resolved, Resolution::Static(BindingTarget::Base)) {
                    return Ok(false);
                }
            }
        }
        Ok(true)
    }

    fn attachment_call(&mut self, from: NodeId, current: PackageId, call: &CallSite) -> Result<()> {
        let package = static_package_arg(call);
        if let Some(name) = package
            && self.package_is_suggested_only(current, name)?
            && !self.optional_package_selected(name)
        {
            return Ok(());
        }
        self.diagnostic(
            from,
            current,
            None,
            RejectCode::PackageAttachmentUnsupported,
            match package {
                Some(name) => {
                    format!("search-path attachment of `{name}` is outside the current contract")
                }
                None => "dynamic package attachment is outside the current contract".into(),
            },
            Some(call.span.clone()),
        );
        Ok(())
    }

    fn native_call(
        &mut self,
        site: ParsedSite<'_>,
        parsed: &ParsedRFile,
        call: &CallSite,
    ) -> Result<()> {
        let ParsedSite {
            node: from,
            package: current,
            image,
            binding,
            lexical_environment,
        } = site;
        if let Some(target) =
            self.native_component_for_call(current, image, lexical_environment, call)?
        {
            let component = target.component;
            self.require_at(
                from,
                Need::Native {
                    package: current,
                    component: component.clone().into(),
                },
                EdgeKind::Native,
                format!(
                    "reachable {} resolves its static native selector through `{component}`",
                    call.callee
                ),
                Some(call.span.clone()),
            );
            self.process_native_routine_callbacks(NativeCallbackContext {
                owner: from,
                package: current,
                image,
                binding,
                lexical_environment,
                component: &component,
                parsed,
                call,
            })?;
            if !self.is_root(current) {
                self.linked_native_selector(from, current, image, call, &component);
            }
        } else {
            self.diagnostic(
                from,
                current,
                None,
                RejectCode::UnknownNativeLookup,
                format!(
                    "{} native selector cannot be resolved to one registered package DLL",
                    call.callee
                ),
                Some(call.span.clone()),
            );
        }
        Ok(())
    }

    fn namespace_operation(
        &mut self,
        from: NodeId,
        current: PackageId,
        binding: &str,
        parsed: &ParsedRFile,
        call: &CallSite,
        operation: NamespaceCall,
    ) -> Result<()> {
        let literal = static_string_arg(call);
        let name = literal.map(Cow::Borrowed).or_else(|| {
            self.reflection
                .contextual_namespace(&call.span)
                .map(|name| Cow::Owned(name.to_owned()))
        });
        let Some(name) = name else {
            if self.reflection.is_non_reflective_namespace_use(&call.span) {
                return Ok(());
            }
            let formals = namespace_formals(&call.callee);
            if let Some(names) =
                declared_strings(parsed, call, formals, namespace_target(&call.callee))
            {
                for name in names {
                    self.declared_namespace_name(from, current, binding, call, operation, &name)?;
                }
                return Ok(());
            }
            self.diagnostic(
                from,
                current,
                Some(binding),
                RejectCode::DynamicPackageDiscovery,
                format!("{}() with a dynamic namespace name", call.callee),
                Some(call.span.clone()),
            );
            return Ok(());
        };
        let target = match self.discovered_package(from, current, call, &name)? {
            Discovered::Linked(target) => target,
            Discovered::Settled => return Ok(()),
            Discovered::Optional => {
                if operation == NamespaceCall::Require {
                    self.optional_availability_blocker(from, current, binding, &name, &call.span);
                }
                return Ok(());
            }
            Discovered::Missing => {
                if operation == NamespaceCall::Require {
                    self.relocations.push(PendingRelocation::RequireNamespace {
                        source: call.span.clone(),
                        loaded: None,
                    });
                } else {
                    self.record_missing_package(
                        from,
                        current,
                        &name,
                        EdgeKind::Discovery,
                        format!("{} requires unavailable namespace {name}", call.callee),
                        Some(call.span.clone()),
                    );
                }
                return Ok(());
            }
        };
        if literal.is_none() {
            self.diagnostic(
                from,
                current,
                Some(binding),
                RejectCode::DynamicPackageDiscovery,
                format!(
                    "{}() names Linked `{name}` through a computed value, which cannot be rewritten to its private namespace",
                    call.callee
                ),
                Some(call.span.clone()),
            );
            return Ok(());
        }
        let rewritable = match operation {
            NamespaceCall::Require => &["quietly"][..],
            NamespaceCall::Operation(NamespaceOperation::As) => &["base.OK"][..],
            NamespaceCall::Operation(NamespaceOperation::Get | NamespaceOperation::Load) => &[],
        };
        if !only_package_argument(call, rewritable) {
            self.diagnostic(
                from,
                current,
                None,
                RejectCode::UnsupportedRootTransformation,
                format!(
                    "{}() on Linked `{name}` passes arguments that its private namespace cannot honor",
                    call.callee
                ),
                Some(call.span.clone()),
            );
            return Ok(());
        }
        if target != current {
            self.require_at(
                from,
                Need::Activation { package: target },
                EdgeKind::Discovery,
                format!("{} names Linked `{name}`", call.callee),
                Some(call.span.clone()),
            );
        }
        self.relocations.push(PendingRelocation::namespace(
            call.span.clone(),
            target,
            operation,
        ));
        Ok(())
    }

    fn declared_namespace_name(
        &mut self,
        from: NodeId,
        current: PackageId,
        binding: &str,
        call: &CallSite,
        operation: NamespaceCall,
        name: &str,
    ) -> Result<()> {
        let problem = match self.discovered_package(from, current, call, name)? {
            Discovered::Settled => return Ok(()),
            Discovered::Optional if operation != NamespaceCall::Require => return Ok(()),
            Discovered::Missing if operation != NamespaceCall::Require => {
                self.record_missing_package(
                    from,
                    current,
                    name,
                    EdgeKind::Discovery,
                    format!("{} requires unavailable namespace {name}", call.callee),
                    Some(call.span.clone()),
                );
                return Ok(());
            }
            Discovered::Linked(_) => format!(
                "{}() can name Linked `{name}` through a declared computed value, which cannot be rewritten to its private namespace",
                call.callee
            ),
            Discovered::Optional | Discovered::Missing => format!(
                "{}() can name `{name}` through a declared computed value, whose installation slinker does not fix",
                call.callee
            ),
        };
        self.diagnostic(
            from,
            current,
            Some(binding),
            RejectCode::DynamicPackageDiscovery,
            problem,
            Some(call.span.clone()),
        );
        Ok(())
    }

    pub(super) fn discovered_package(
        &mut self,
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
            self.external.insert(target);
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

    pub(super) fn call_resolves_definitely_to_base(
        &mut self,
        current: PackageId,
        image: &PackageImage,
        lexical_environment: &str,
        call: &CallSite,
    ) -> Result<bool> {
        if call.callee_kind != CalleeKind::DefinitelyExternal {
            return Ok(false);
        }
        match call.qualified_package.as_deref() {
            Some("base") => Ok(true),
            Some(_) => Ok(false),
            None => Ok(matches!(
                self.resolve_lexical_name(current, image, lexical_environment, &call.callee)?,
                Resolution::Static(BindingTarget::Base)
            )),
        }
    }

    pub(super) fn identity_query(
        &mut self,
        from: NodeId,
        current: PackageId,
        call: &CallSite,
        version: bool,
    ) -> Result<()> {
        let Some(name) = static_string_arg(call) else {
            self.diagnostic(
                from,
                current,
                None,
                RejectCode::DynamicPackageDiscovery,
                "dynamic package identity query",
                Some(call.span.clone()),
            );
            return Ok(());
        };
        let name = name.to_owned();
        let target = match self.discovered_package(from, current, call, &name)? {
            Discovered::Linked(target) => target,
            Discovered::Settled | Discovered::Optional => return Ok(()),
            Discovered::Missing => {
                self.record_missing_package(
                    from,
                    current,
                    &name,
                    EdgeKind::Discovery,
                    format!("{} requires unavailable package {name}", call.callee),
                    Some(call.span.clone()),
                );
                return Ok(());
            }
        };
        if !version {
            self.diagnostic(
                from,
                current,
                None,
                RejectCode::UnsupportedRootTransformation,
                format!("find.package(\"{name}\") has no installed path once `{name}` is Linked"),
                Some(call.span.clone()),
            );
        } else if !only_package_argument(call, &[]) {
            self.diagnostic(
                from,
                current,
                None,
                RejectCode::UnsupportedRootTransformation,
                format!("packageVersion() on Linked `{name}` passes a library location"),
                Some(call.span.clone()),
            );
        } else {
            self.relocations.push(PendingRelocation::PackageVersion {
                source: call.span.clone(),
                version: self.packages.identity(target).version.to_string(),
            });
        }
        Ok(())
    }

    pub(super) fn require_root(&mut self, need: Need) {
        self.record_unclassified(&need);
        self.require_internal_root(need);
    }

    pub(super) fn require_internal_root(&mut self, need: Need) {
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
            self.packages.name(package).to_owned(),
            NodeKind::ExternalBinding {
                name: name.to_owned(),
            },
            span,
        )
    }

    pub(super) fn need_node(&mut self, need: &Need) -> NodeId {
        let package = self.packages.name(need.package()).to_owned();
        let kind = match need {
            Need::Binding { binding, .. } => NodeKind::Binding {
                name: binding.to_string(),
            },
            Need::PrivateBinding {
                environment,
                binding,
                ..
            } => NodeKind::PrivateBinding {
                environment: environment.clone(),
                name: binding.to_string(),
            },
            Need::ClosureExecution { package, closure } => {
                let (closure, owner, _, enclosure) = self
                    .closure_execution_source(*package, *closure)
                    .expect("closure execution need references the package object graph");
                NodeKind::ClosureObject {
                    owner: owner.to_string(),
                    path: closure.provenance.path,
                    enclosure,
                    derived: closure.derived_from.is_some(),
                }
            }
            Need::Activation { .. } => NodeKind::Activation,
            Need::Resource { resource, .. } => NodeKind::Resource {
                path: resource.to_string(),
            },
            Need::Dataset { dataset, .. } => NodeKind::Dataset {
                name: dataset.to_string(),
            },
            Need::S3Registration { registration, .. } => NodeKind::S3Registration {
                generic: self.generic_label(&registration.generic),
                class: registration.class.to_string(),
            },
            Need::Native { component, .. } => NodeKind::NativeComponent {
                name: component.to_string(),
            },
            Need::Lifecycle { hook, .. } => NodeKind::Lifecycle {
                hook: hook.to_string(),
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
            package: self.packages.name(package).to_owned(),
            binding: binding.map(str::to_owned),
            code,
            message,
            span,
            node: Some(node),
            evidence: Vec::new(),
        }
    }

    pub(super) fn generic_label(&self, generic: &GenericId) -> String {
        match generic.package {
            Some(package) => format!("{}::{}", self.packages.name(package), generic.name),
            None => generic.name.to_string(),
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
        let node = self
            .graph
            .add_node(missing, NodeKind::MissingPackage, span.clone());
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
            package: self.packages.name(requester).to_owned(),
            binding: self.node_binding(from),
            span,
            detail: reason,
        };
        self.diagnostics.record_derived(
            Cause::MissingPackage(missing.to_owned()),
            Diagnostic {
                package: missing.to_owned(),
                ..primary
            },
            evidence,
        );
    }

    fn node_binding(&self, node: NodeId) -> Option<String> {
        match &self.graph.nodes[node.0].kind {
            NodeKind::Binding { name } | NodeKind::PrivateBinding { name, .. } => {
                Some(name.clone())
            }
            _ => None,
        }
    }
}

pub(super) fn is_r_constant(name: &str) -> bool {
    matches!(
        name,
        "NULL"
            | "TRUE"
            | "FALSE"
            | "NA"
            | "NaN"
            | "Inf"
            | "NA_integer_"
            | "NA_real_"
            | "NA_complex_"
            | "NA_character_"
    )
}

fn reproduces_namespace_info(field: Option<&str>) -> bool {
    matches!(
        field,
        Some("exports" | "spec" | "imports" | "dynlibs" | "S3methods")
    )
}

fn created_name(call: &CallSite) -> Option<(&'static str, CreatedName)> {
    let (operation, formals, target): (_, &[&str], _) = match call.callee.as_str() {
        "assign" => (
            "assign",
            &["x", "value", "pos", "envir", "inherits", "immediate"],
            "x",
        ),
        "delayedAssign" => (
            "delayedAssign",
            &["x", "value", "eval.env", "assign.env"],
            "x",
        ),
        "makeActiveBinding" => ("makeActiveBinding", &["sym", "fun", "env"], "sym"),
        "list2env" => return Some(("list2env", CreatedName::Any)),
        _ => return None,
    };
    let name = match matched_static_arg(call, formals, target) {
        Some(StaticArg::String(name)) => CreatedName::Named(name.clone()),
        Some(StaticArg::Symbol(_)) | None => CreatedName::Any,
    };
    Some((operation, name))
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum GuardVerdict {
    Active,
    Pruned,
    PrunedByUnselectedOptional(String),
}

pub(super) enum Discovered {
    Linked(PackageId),
    Settled,
    Optional,
    Missing,
}

struct DeclaredDependencies {
    required: HashSet<String>,
    suggested_only: HashSet<String>,
}
