use crate::analysis::{Diagnostic, EdgeKind, Graph, Need, NodeId, NodeKind, RejectCode, S3Id};
use crate::analysis::policy::{DiscoveryPolicy, LinkPolicy};
use crate::build::{PackageOperation, Rewrite};
use crate::metadata::{relations, RelationField};
use crate::package::{BindingImage, ImportSpec, InstalledPackage, ObjectKind, PackageId, PackageImage, PackageIndex, PackageProvider, PrivateBindingImage, SyntaxValidation, NativeSafety};
use crate::syntax::{
    ActiveBindingDef, CalleeKind, CallSite, NameRefKind, NamespaceImportResolution, NamespaceImports,
    OakParseContext, OakParser, PackageGuard,
    ParsedRFile, ResolvedName, SemanticIssueKind, SourceId, Sources, Span, StaticArg,
    StaticEnvironment, SyntaxEffect, SyntaxEffectKind,
};
use crate::{Error, Result};
use rayon::prelude::*;
use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::sync::Arc;

#[derive(Debug)]
pub struct LinkPlan {
    pub graph: Graph,
    pub roots: Vec<NodeId>,
    pub diagnostics: Vec<Diagnostic>,
    pub rewrites: Vec<Rewrite>,
    pub sources: Sources,
    pub retained: HashSet<Need>,
    pub packages: HashSet<PackageId>,
    pub target_provided: HashSet<PackageId>,
    pub parsed_bindings: usize,
    pub inspected_packages: usize,
    pub images: HashMap<PackageId, Arc<PackageImage>>,
}

#[derive(Clone, Debug)]
enum ParseState {
    Parsed(Arc<ParsedRFile>),
    Blocked,
}

pub struct Linker<P: PackageProvider> {
    packages: P,
    policy: LinkPolicy,
    extra_packages: HashSet<String>,
    jobs: usize,
    parse_pool: Option<Arc<rayon::ThreadPool>>,
    graph: Graph,
    roots: Vec<NodeId>,
    pending: VecDeque<Need>,
    queued: HashSet<Need>,
    processed: HashSet<Need>,
    encountered: HashSet<PackageId>,
    target_provided: HashSet<PackageId>,
    parsed_bindings: HashMap<(PackageId, String), ParseState>,
    images: HashMap<PackageId, Arc<PackageImage>>,
    diagnostics: Vec<Diagnostic>,
    rewrites: Vec<Rewrite>,
    sources: Sources,
    source_ids: HashMap<(PackageId, String), SourceId>,
    root: Option<PackageId>,
    suggested_only: HashMap<PackageId, HashSet<String>>,
    namespace_imports: HashMap<PackageId, NamespaceImports>,
    activation_bindings: HashSet<(PackageId, String)>,
    observations: Vec<SyntaxObservation>,
    diagnostic_keys: HashSet<(NodeId, RejectCode, String)>,
}

#[derive(Clone, Debug)]
struct SyntaxObservation {
    node: NodeId,
    package: PackageId,
    span: Span,
    kind: String,
}

impl<P: PackageProvider> Linker<P> {
    pub fn new(packages: P, jobs: usize) -> Self {
        Self {
            packages,
            policy: LinkPolicy::default(),
            extra_packages: HashSet::new(),
            jobs: jobs.max(1),
            parse_pool: None,
            graph: Graph::default(),
            roots: Vec::new(),
            pending: VecDeque::new(),
            queued: HashSet::new(),
            processed: HashSet::new(),
            encountered: HashSet::new(),
            target_provided: HashSet::new(),
            parsed_bindings: HashMap::new(),
            images: HashMap::new(),
            diagnostics: Vec::new(),
            rewrites: Vec::new(),
            sources: Sources::default(),
            source_ids: HashMap::new(),
            root: None,
            suggested_only: HashMap::new(),
            namespace_imports: HashMap::new(),
            activation_bindings: HashSet::new(),
            observations: Vec::new(),
            diagnostic_keys: HashSet::new(),
        }
    }

    pub fn with_policy(mut self, policy: LinkPolicy) -> Self {
        self.policy = policy;
        self
    }

    pub fn with_extra_packages(mut self, packages: impl IntoIterator<Item = String>) -> Self {
        self.extra_packages.extend(packages);
        self
    }

    pub fn analyze(mut self, root_name: &str) -> Result<LinkPlan> {
        let root = self.packages.locate(root_name)?;
        if self.packages.is_target_provided(&root) {
            return Err(Error::Analysis(format!("root package `{root_name}` cannot be target-provided")));
        }
        self.root = Some(root.id.clone());
        self.encountered.insert(root.id.clone());
        let root_image = self.image(&root)?;

        // Package activation and the public/runtime entry points form the root
        // contract. Internal namespace bindings are reached only when retained
        // code, lifecycle hooks, S3 registrations, or native obligations demand
        // them. DESCRIPTION/NAMESPACE dependency metadata informs resolution;
        // it does not make every declared package or every root binding live.
        self.require_root(Need::Activation { package: root.id.clone() });
        while !self.pending.is_empty() {
            self.process_frontier()?;
        }

        let mut entry_bindings = root_image.index.exports.values().cloned().collect::<Vec<_>>();
        entry_bindings.sort();
        entry_bindings.dedup();
        for binding in entry_bindings {
            self.require_root(Need::Binding { package: root.id.clone(), binding });
        }

        while !self.pending.is_empty() {
            self.process_frontier()?;
        }

        self.finalize_syntax_observations();
        let inspected_packages = self.images.len();
        let parsed_bindings = self
            .parsed_bindings
            .values()
            .filter(|state| matches!(state, ParseState::Parsed(_)))
            .count();
        Ok(LinkPlan {
            graph: self.graph,
            roots: self.roots,
            diagnostics: self.diagnostics,
            rewrites: self.rewrites,
            sources: self.sources,
            retained: self.processed,
            packages: self.encountered,
            target_provided: self.target_provided,
            parsed_bindings,
            inspected_packages,
            images: self.images,
        })
    }

    fn process_frontier(&mut self) -> Result<()> {
        let frontier = self.pending.len();
        if frontier == 0 {
            return Ok(());
        }

        // New needs discovered while this frontier is processed are deferred
        // to the next frontier. Every item was already justified by a semantic
        // edge; batching changes scheduling only, never reachability.
        // A full demanded image already contains its PackageIndex. Fetch images
        // first so activation/native needs for the same package do not trigger a
        // redundant metadata-only R inspection in this frontier.
        self.prefetch_frontier_images(frontier)?;
        self.prefetch_frontier_indexes(frontier)?;
        self.preparse_frontier_bindings(frontier)?;

        for _ in 0..frontier {
            let Some(need) = self.pending.pop_front() else { break };
            self.queued.remove(&need);
            if !self.processed.insert(need.clone()) {
                continue;
            }
            self.process_need(need)?;
        }
        Ok(())
    }

    fn image(&mut self, package: &InstalledPackage) -> Result<Arc<PackageImage>> {
        if let Some(image) = self.images.get(&package.id) {
            return Ok(Arc::clone(image));
        }
        let image = self.packages.image(package)?;
        self.images.insert(package.id.clone(), Arc::clone(&image));
        Ok(image)
    }

    fn parse_pool(&mut self) -> Result<Option<Arc<rayon::ThreadPool>>> {
        if self.jobs <= 1 {
            return Ok(None);
        }
        if self.parse_pool.is_none() {
            let pool = rayon::ThreadPoolBuilder::new()
                .num_threads(self.jobs)
                .thread_name(|index| format!("slinker-air-{index}"))
                .build()
                .map_err(|error| Error::Analysis(format!("failed to create Rayon pool: {error}")))?;
            self.parse_pool = Some(Arc::new(pool));
        }
        Ok(self.parse_pool.as_ref().map(Arc::clone))
    }

    fn prefetch_frontier_indexes(&mut self, frontier: usize) -> Result<()> {
        let mut seen = HashSet::new();
        let mut names = Vec::new();
        for need in self.pending.iter().take(frontier) {
            if !matches!(need, Need::Activation { .. } | Need::Native { .. }) {
                continue;
            }
            let id = need.package();
            if seen.insert(id.name.clone()) {
                names.push(id.name.clone());
            }
        }
        let mut packages = self.packages.locate_many(&names, self.jobs)?;
        packages.retain(|package| !self.packages.is_target_provided(package));
        if !packages.is_empty() {
            self.packages.prefetch_indexes(&packages, self.jobs)?;
        }
        Ok(())
    }

    fn prefetch_frontier_images(&mut self, frontier: usize) -> Result<()> {
        let mut seen = HashSet::new();
        let mut names = Vec::new();
        for need in self.pending.iter().take(frontier) {
            if !matches!(need, Need::Binding { .. } | Need::PrivateBinding { .. } | Need::Dataset { .. }) {
                continue;
            }
            let id = need.package();
            if !self.images.contains_key(id) && seen.insert(id.name.clone()) {
                names.push(id.name.clone());
            }
        }
        let mut packages = self.packages.locate_many(&names, self.jobs)?;
        packages.retain(|package| !self.packages.is_target_provided(package));
        if !packages.is_empty() {
            self.packages.prefetch(&packages, self.jobs)?;
        }
        Ok(())
    }

    fn preparse_frontier_bindings(&mut self, frontier: usize) -> Result<()> {
        struct Work {
            key: (PackageId, String),
            owner_binding: String,
            source_key: String,
            owner_node: NodeId,
            source: SourceId,
            text: Arc<str>,
            context: OakParseContext,
        }

        let needs = self.pending.iter().take(frontier).cloned().collect::<Vec<_>>();
        let mut work = Vec::<Work>::new();
        let mut scheduled = HashSet::<(PackageId, String)>::new();

        for need in needs {
            let (id, owner_binding, source_key, closure, embedded, owner_node, image) = match need {
                Need::Binding { package: id, binding } => {
                    let package = self.packages.locate(&id.name)?;
                    if self.packages.is_target_provided(&package) {
                        continue;
                    }
                    let image = self.image(&package)?;
                    let Some(binding_image) = image.binding(&binding).cloned() else { continue };
                    let owner_node = self.need_node(&Need::Binding {
                        package: id.clone(),
                        binding: binding.clone(),
                    });
                    (
                        id,
                        binding.clone(),
                        binding,
                        binding_image.closure,
                        binding_image.embedded_closures,
                        owner_node,
                        image,
                    )
                }
                Need::PrivateBinding { package: id, environment, binding } => {
                    let package = self.packages.locate(&id.name)?;
                    if self.packages.is_target_provided(&package) {
                        continue;
                    }
                    let image = self.image(&package)?;
                    let Some(binding_image) = image.private_binding(&environment, &binding).cloned() else { continue };
                    let owner_node = self.need_node(&Need::PrivateBinding {
                        package: id.clone(),
                        environment: environment.clone(),
                        binding: binding.clone(),
                    });
                    let key = Self::private_source_key(&environment, &binding);
                    (
                        id,
                        key.clone(),
                        key,
                        binding_image.closure,
                        binding_image.embedded_closures,
                        owner_node,
                        image,
                    )
                }
                _ => continue,
            };

            if let Some(closure) = closure {
                let key = (id.clone(), source_key.clone());
                if !self.parsed_bindings.contains_key(&key) && scheduled.insert(key.clone()) {
                    let source = self.sources.add_binding(id.name.clone(), source_key.clone(), Arc::clone(&closure.source));
                    self.source_ids.insert(key.clone(), source.clone());
                    work.push(Work {
                        key,
                        owner_binding: owner_binding.clone(),
                        source_key: source_key.clone(),
                        owner_node,
                        source,
                        text: Arc::clone(&closure.source),
                        context: self.oak_parse_context(&image, &closure.environment)?,
                    });
                }
            }
            for nested in embedded {
                let nested_key = format!("{source_key}{}", nested.path);
                let key = (id.clone(), nested_key.clone());
                if self.parsed_bindings.contains_key(&key) || !scheduled.insert(key.clone()) {
                    continue;
                }
                let source = self.sources.add_binding(id.name.clone(), nested_key.clone(), Arc::clone(&nested.source));
                self.source_ids.insert(key.clone(), source.clone());
                work.push(Work {
                    key,
                    owner_binding: owner_binding.clone(),
                    source_key: nested_key,
                    owner_node,
                    source,
                    text: Arc::clone(&nested.source),
                    context: self.oak_parse_context(&image, &nested.environment)?,
                });
            }
        }
        if work.is_empty() {
            return Ok(());
        }

        let parse_all = || {
            work.par_iter()
                .map(|item| OakParser.parse_binding_with_context(item.source.clone(), item.text.as_ref(), &item.context))
                .collect::<Vec<_>>()
        };
        let results = if work.len() > 1 {
            if let Some(pool) = self.parse_pool()? {
                pool.install(parse_all)
            } else {
                work.iter().map(|item| OakParser.parse_binding_with_context(item.source.clone(), item.text.as_ref(), &item.context)).collect()
            }
        } else {
            work.iter().map(|item| OakParser.parse_binding_with_context(item.source.clone(), item.text.as_ref(), &item.context)).collect()
        };

        for (item, result) in work.into_iter().zip(results) {
            match result {
                Ok(parsed) => {
                    self.parsed_bindings.insert(item.key, ParseState::Parsed(Arc::new(parsed)));
                }
                Err(error) => {
                    self.handle_air_rejection(
                        &item.key.0,
                        &item.owner_binding,
                        &item.source_key,
                        item.owner_node,
                        error,
                    )?;
                }
            }
        }
        Ok(())
    }

    fn process_need(&mut self, need: Need) -> Result<()> {
        match need {
            Need::Binding { package, binding } => self.process_binding(package, binding),
            Need::PrivateBinding { package, environment, binding } => self.process_private_binding(package, environment, binding),
            Need::Activation { package } => self.process_activation(package),
            Need::Resource { package, resource } => self.process_resource(package, resource),
            Need::Dataset { package, dataset } => self.process_dataset(package, dataset),
            Need::S3Registration { package, registration } => self.process_s3(package, registration),
            Need::Native { package, component } => self.process_native(package, component),
            Need::Lifecycle { package, hook } => self.process_lifecycle(package, hook),
        }
    }

    fn process_binding(&mut self, id: PackageId, binding: String) -> Result<()> {
        let package = self.packages.locate(&id.name)?;
        let node = self.need_node(&Need::Binding { package: id.clone(), binding: binding.clone() });
        if self.packages.is_target_provided(&package) {
            self.target_provided.insert(package.id.clone());
            return Ok(());
        }
        let image = self.image(&package)?;
        let Some(binding_image) = image.binding(&binding).cloned() else {
            if binding != ".onLoad" && image.index.lifecycle.on_load {
                self.ensure_on_load_analyzed(&id)?;
            }
            if self.activation_bindings.contains(&(id.clone(), binding.clone())) {
                let lifecycle = self.need_node(&Need::Lifecycle {
                    package: id.clone(),
                    hook: ".onLoad".into(),
                });
                self.graph.add_edge(
                    node,
                    lifecycle,
                    EdgeKind::Lifecycle,
                    format!("activation creates active binding `{binding}`"),
                );
                return Ok(());
            }
            if self.is_root(&id)
                && image.index.exports.values().any(|exported_binding| exported_binding == &binding)
            {
                let resolved = self.resolve_name(&package, &image, &binding)?;
                match resolved {
                    ResolvedName::Imported { package, binding: foreign_binding } => {
                        self.require(
                            node,
                            Need::Activation { package: package.clone() },
                            EdgeKind::Export,
                            format!("root re-export `{binding}` requires namespace activation"),
                        );
                        self.require(
                            node,
                            Need::Binding { package, binding: foreign_binding.clone() },
                            EdgeKind::Export,
                            format!("root re-export `{binding}` resolves to imported binding `{foreign_binding}`"),
                        );
                        return Ok(());
                    }
                    ResolvedName::TargetProvided { package, binding: foreign_binding } => {
                        let external = self.graph.add_node(
                            package.name.clone(),
                            NodeKind::ExternalBinding { name: foreign_binding.clone() },
                            None,
                        );
                        self.graph.add_edge(
                            node,
                            external,
                            EdgeKind::Export,
                            format!("root re-export `{binding}` resolves to target-provided `{foreign_binding}`"),
                        );
                        return Ok(());
                    }
                    ResolvedName::NativeSymbol { package, component, binding: native_binding } => {
                        self.require(
                            node,
                            Need::Native { package, component: component.clone() },
                            EdgeKind::Export,
                            format!("root export `{binding}` resolves to registered native symbol `{native_binding}` in `{component}`"),
                        );
                        return Ok(());
                    }
                    ResolvedName::Base(_) | ResolvedName::PackageMetadata { .. } => return Ok(()),
                    ResolvedName::MissingPackage { package, binding: foreign_binding } => {
                        let detail = foreign_binding
                            .as_deref()
                            .map(|name| format!("root re-export `{binding}` requires missing {package}::{name}"))
                            .unwrap_or_else(|| format!("root re-export `{binding}` requires missing namespace {package}"));
                        self.record_missing_package(node, &id, &package, EdgeKind::Export, detail, None);
                        return Ok(());
                    }
                    ResolvedName::Unknown(name) => {
                        self.diagnostic(
                            node,
                            &id,
                            Some(&binding),
                            RejectCode::UnresolvedBinding,
                            format!("exported name `{binding}` resolves to unknown binding `{name}`"),
                            None,
                        );
                        return Ok(());
                    }
                    ResolvedName::PackageBinding { .. } | ResolvedName::PrivateBinding { .. } | ResolvedName::Local(_) => {}
                }
            }
            self.diagnostic(
                node,
                &id,
                Some(&binding),
                RejectCode::UnresolvedBinding,
                format!("installed namespace has no binding `{binding}`"),
                None,
            );
            return Ok(());
        };

        if !self.is_root(&id) {
            for registration in image.index.s3.iter().filter(|registration| registration.method == binding) {
                if let Some((package_name, _generic)) = registration.generic.split_once("::") {
                    if self.package_is_suggested_only(&image.index, package_name)?
                        && !self.optional_package_selected(package_name)
                    {
                        continue;
                    }
                }
                self.require(
                    node,
                    Need::S3Registration {
                        package: id.clone(),
                        registration: S3Id {
                            generic: registration.generic.clone(),
                            class: registration.class.clone(),
                            method: registration.method.clone(),
                        },
                    },
                    EdgeKind::S3Registration,
                    format!(
                        "retained method `{binding}` requires its {}/{} registration",
                        registration.generic, registration.class
                    ),
                );
            }
        }

        if !binding_image.issues.is_empty() {
            self.diagnostic(
                node,
                &id,
                Some(&binding),
                RejectCode::UnsupportedObject,
                binding_image
                    .issues
                    .iter()
                    .map(|issue| format!("{}: {} ({})", issue.path, issue.kind, issue.detail))
                    .collect::<Vec<_>>()
                    .join("; "),
                None,
            );
        }
        match &binding_image.object_kind {
            ObjectKind::Other(kind) => self.diagnostic(
                node,
                &id,
                Some(&binding),
                RejectCode::UnsupportedObject,
                format!("unsupported installed object type `{kind}`"),
                None,
            ),
            ObjectKind::Unavailable => self.diagnostic(
                node,
                &id,
                Some(&binding),
                RejectCode::UnsupportedObject,
                "installed binding could not be forced",
                None,
            ),
            _ => {}
        }

        if let Some(closure) = &binding_image.closure {
            if closure.environment.starts_with("unsupported:") {
                self.diagnostic(
                    node,
                    &id,
                    Some(&binding),
                    RejectCode::UnsupportedObject,
                    format!("unsupported closure environment `{}`", closure.environment),
                    None,
                );
            }
            if let Some(parsed) = self.parsed(&id, &binding, &image, &binding_image)? {
                self.process_parsed(node, &package, &image, &binding, &closure.environment, parsed.as_ref())?;
            }
        }

        // Retained non-closure objects may contain closures. Analyze every
        // embedded closure lazily while attributing its semantic edges to the
        // retained parent binding. This keeps object reachability recursive
        // without inventing graph nodes for implementation-only object paths.
        for embedded in &binding_image.embedded_closures {
            if embedded.environment.starts_with("unsupported:") {
                self.diagnostic(
                    node,
                    &id,
                    Some(&binding),
                    RejectCode::UnsupportedObject,
                    format!(
                        "embedded closure at {} has unsupported environment `{}`",
                        embedded.path, embedded.environment
                    ),
                    None,
                );
            }
            let label = format!("{binding}{}", embedded.path);
            let context = self.oak_parse_context(&image, &embedded.environment)?;
            if let Some(parsed) = self.parsed_source(
                &id,
                &binding,
                &label,
                node,
                Arc::clone(&embedded.source),
                context,
            )? {
                self.process_parsed(node, &package, &image, &binding, &embedded.environment, parsed.as_ref())?;
            }
        }
        Ok(())
    }

    fn process_private_binding(&mut self, id: PackageId, environment: String, binding: String) -> Result<()> {
        let package = self.packages.locate(&id.name)?;
        let node = self.need_node(&Need::PrivateBinding {
            package: id.clone(),
            environment: environment.clone(),
            binding: binding.clone(),
        });
        if self.packages.is_target_provided(&package) {
            self.target_provided.insert(package.id.clone());
            return Ok(());
        }
        let image = self.image(&package)?;
        let Some(binding_image) = image.private_binding(&environment, &binding).cloned() else {
            self.diagnostic(
                node,
                &id,
                Some(&binding),
                RejectCode::UnresolvedBinding,
                format!("private environment `{environment}` has no binding `{binding}`"),
                None,
            );
            return Ok(());
        };

        self.diagnose_private_object(node, &id, &environment, &binding, &binding_image);

        let source_key = Self::private_source_key(&environment, &binding);
        if let Some(closure) = &binding_image.closure {
            if closure.environment.starts_with("unsupported:") {
                self.diagnostic(
                    node,
                    &id,
                    Some(&binding),
                    RejectCode::UnsupportedObject,
                    format!("unsupported private closure environment `{}`", closure.environment),
                    None,
                );
            }
            let context = self.oak_parse_context(&image, &closure.environment)?;
            if let Some(parsed) = self.parsed_source(
                &id,
                &source_key,
                &source_key,
                node,
                Arc::clone(&closure.source),
                context,
            )? {
                self.process_parsed(
                    node,
                    &package,
                    &image,
                    &source_key,
                    &closure.environment,
                    parsed.as_ref(),
                )?;
            }
        }

        for embedded in &binding_image.embedded_closures {
            if embedded.environment.starts_with("unsupported:") {
                self.diagnostic(
                    node,
                    &id,
                    Some(&binding),
                    RejectCode::UnsupportedObject,
                    format!(
                        "embedded closure at {} in {environment}${binding} has unsupported environment `{}`",
                        embedded.path, embedded.environment
                    ),
                    None,
                );
            }
            let label = format!("{source_key}{}", embedded.path);
            let context = self.oak_parse_context(&image, &embedded.environment)?;
            if let Some(parsed) = self.parsed_source(
                &id,
                &source_key,
                &label,
                node,
                Arc::clone(&embedded.source),
                context,
            )? {
                self.process_parsed(
                    node,
                    &package,
                    &image,
                    &source_key,
                    &embedded.environment,
                    parsed.as_ref(),
                )?;
            }
        }
        Ok(())
    }

    fn diagnose_private_object(
        &mut self,
        node: NodeId,
        id: &PackageId,
        environment: &str,
        binding: &str,
        image: &PrivateBindingImage,
    ) {
        if !image.issues.is_empty() {
            self.diagnostic(
                node,
                id,
                Some(binding),
                RejectCode::UnsupportedObject,
                format!(
                    "private binding {environment}${binding}: {}",
                    image.issues
                        .iter()
                        .map(|issue| format!("{}: {} ({})", issue.path, issue.kind, issue.detail))
                        .collect::<Vec<_>>()
                        .join("; ")
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
                format!("private binding {environment}${binding} has unsupported object type `{kind}`"),
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

    fn guards_active(
        &mut self,
        image: &PackageImage,
        guards: &[PackageGuard],
    ) -> Result<bool> {
        if guards.is_empty() {
            return Ok(true);
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
            // The probe itself is shadowed by a package/importFrom binding, so
            // the static availability interpretation is not sound.
            return Ok(true);
        }

        for guard in guards {
            let package = guard.package();
            if self.optional_package_selected(package) {
                continue;
            }
            if self.package_is_suggested_only(&image.index, package)? {
                return Ok(false);
            }
            let imported = image.index.imports.iter().any(|import| match import {
                ImportSpec::All { package: imported, .. } | ImportSpec::From { package: imported, .. } => imported == package,
            });
            match guard {
                PackageGuard::Selected(_) => return Ok(false),
                PackageGuard::Loaded(_) => {
                    // Imports are activated before `.onLoad`; ambient installed
                    // packages are not assumed to be loaded.
                    if !imported {
                        return Ok(false);
                    }
                }
                PackageGuard::Available(_) => {
                    if imported {
                        continue;
                    }
                    match self.packages.locate_optional(package)? {
                        Some(candidate) if self.packages.is_target_provided(&candidate) => {
                            self.target_provided.insert(candidate.id);
                        }
                        _ => return Ok(false),
                    }
                }
            }
        }
        Ok(true)
    }

    fn process_parsed(
        &mut self,
        node: NodeId,
        package: &InstalledPackage,
        image: &PackageImage,
        binding: &str,
        lexical_environment: &str,
        parsed: &ParsedRFile,
    ) -> Result<()> {
        for issue in &parsed.issues {
            let code = match issue.kind {
                SemanticIssueKind::AmbiguousEffect => RejectCode::SemanticAmbiguity,
                SemanticIssueKind::AmbiguousAttachOrder => RejectCode::SemanticAmbiguity,
                SemanticIssueKind::UninstalledPackage => RejectCode::MissingDependency,
                SemanticIssueKind::SourceCycle => RejectCode::SemanticAmbiguity,
            };
            self.diagnostic(
                node,
                &package.id,
                Some(binding),
                code,
                issue.message.clone(),
                issue.span.clone(),
            );
        }

        for expression in &parsed.expressions {
            for active in &expression.active_bindings {
                if binding != ".onLoad" || !active.certain { continue; }
                if !self.guards_active(image, &active.guards)? { continue; }
                if self.active_binding_targets_current_namespace(package, image, lexical_environment, active)? {
                    self.activation_bindings.insert((package.id.clone(), active.name.clone()));
                }
            }
            for reference in &expression.references {
                if !self.guards_active(image, &reference.guards)? { continue; }
                let resolved = self.resolve_lexical_name(package, image, lexical_environment, &reference.name)?;
                if reference.kind == NameRefKind::ConditionalFallthrough
                    && matches!(&resolved, ResolvedName::Unknown(_))
                {
                    self.diagnostic(
                        node,
                        &package.id,
                        Some(binding),
                        RejectCode::PotentialUnboundLocal,
                        format!(
                            "conditionally local name `{}` can fall through without an enclosing binding",
                            reference.name
                        ),
                        Some(reference.span.clone()),
                    );
                    continue;
                }
                self.require_resolved(node, &package.id, Some(binding), resolved, reference.span.clone())?;
            }
            for reference in &expression.package_refs {
                if !self.guards_active(image, &reference.guards)? { continue; }
                self.namespace_access(node, package, image, reference)?;
            }
            for resource in &expression.resource_refs {
                if !self.guards_active(image, &resource.guards)? { continue; }
                self.resource_access(node, package, image, resource)?;
            }
            for call in &expression.calls {
                if !self.guards_active(image, &call.guards)? { continue; }
                self.semantic_call(node, package, image, binding, call)?;
            }
            for effect in &expression.effects {
                if !self.guards_active(image, &effect.guards)? { continue; }
                match effect.kind {
                    SyntaxEffectKind::SuperAssignment => {
                        self.handle_superassignment(
                            node,
                            package,
                            image,
                            binding,
                            lexical_environment,
                            effect,
                        )?;
                    }
                    SyntaxEffectKind::IndirectPackageWrite | SyntaxEffectKind::UnsupportedAssignmentTarget => {
                        self.diagnostic(
                            node,
                            &package.id,
                            Some(binding),
                            RejectCode::UnsupportedTopLevelEffect,
                            format!("unsupported R effect: {:?}", effect.kind),
                            Some(effect.span.clone()),
                        );
                    }
                }
            }
        }
        Ok(())
    }

    fn parsed(
        &mut self,
        id: &PackageId,
        binding: &str,
        package_image: &PackageImage,
        image: &BindingImage,
    ) -> Result<Option<Arc<ParsedRFile>>> {
        let closure = image.closure.as_ref().ok_or_else(|| {
            Error::Analysis(format!("closure binding {}::{binding} has no source", id.name))
        })?;
        let node = self.need_node(&Need::Binding {
            package: id.clone(),
            binding: binding.to_owned(),
        });
        let context = self.oak_parse_context(package_image, &closure.environment)?;
        self.parsed_source(
            id,
            binding,
            binding,
            node,
            Arc::clone(&closure.source),
            context,
        )
    }

    fn parsed_source(
        &mut self,
        id: &PackageId,
        owner_binding: &str,
        source_key: &str,
        owner_node: NodeId,
        source_text: Arc<str>,
        context: OakParseContext,
    ) -> Result<Option<Arc<ParsedRFile>>> {
        let key = (id.clone(), source_key.to_owned());
        if let Some(state) = self.parsed_bindings.get(&key) {
            return Ok(match state {
                ParseState::Parsed(parsed) => Some(Arc::clone(parsed)),
                ParseState::Blocked => None,
            });
        }
        let source = self.sources.add_binding(
            id.name.clone(),
            source_key.to_owned(),
            Arc::clone(&source_text),
        );
        self.source_ids.insert(key.clone(), source.clone());
        match OakParser.parse_binding_with_context(source, source_text.as_ref(), &context) {
            Ok(parsed) => {
                let parsed = Arc::new(parsed);
                self.parsed_bindings.insert(key, ParseState::Parsed(Arc::clone(&parsed)));
                Ok(Some(parsed))
            }
            Err(error) => {
                self.handle_air_rejection(id, owner_binding, source_key, owner_node, error)?;
                Ok(None)
            }
        }
    }

    fn handle_air_rejection(
        &mut self,
        id: &PackageId,
        owner_binding: &str,
        source_key: &str,
        owner_node: NodeId,
        air_error: String,
    ) -> Result<()> {
        let key = (id.clone(), source_key.to_owned());
        let source_id = self.source_ids.get(&key).cloned().ok_or_else(|| {
            Error::Analysis(format!("missing virtual source for {}::{source_key}", id.name))
        })?;
        let source_text = Arc::clone(
            &self
                .sources
                .get(&source_id)
                .ok_or_else(|| Error::Analysis("missing source entry".into()))?
                .text,
        );
        let validation = self
            .packages
            .validate_syntax(id, source_key, source_text.as_ref())?;
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
        self.parsed_bindings.insert(key, ParseState::Blocked);
        Ok(())
    }

    fn ensure_on_load_analyzed(&mut self, id: &PackageId) -> Result<()> {
        let package = self.packages.locate(&id.name)?;
        if self.packages.is_target_provided(&package) {
            return Ok(());
        }
        let index = self.packages.index(&package)?;
        if !index.lifecycle.on_load {
            return Ok(());
        }

        let lifecycle = Need::Lifecycle {
            package: id.clone(),
            hook: ".onLoad".into(),
        };
        if self.processed.insert(lifecycle.clone()) {
            self.queued.remove(&lifecycle);
            self.process_lifecycle(id.clone(), ".onLoad".into())?;
        }

        let hook = Need::Binding {
            package: id.clone(),
            binding: ".onLoad".into(),
        };
        if self.processed.insert(hook.clone()) {
            self.queued.remove(&hook);
            self.process_binding(id.clone(), ".onLoad".into())?;
        }
        Ok(())
    }

    fn active_binding_targets_current_namespace(
        &mut self,
        package: &InstalledPackage,
        image: &PackageImage,
        lexical_environment: &str,
        active: &ActiveBindingDef,
    ) -> Result<bool> {
        let expected = format!("namespace:{}", package.id.name);
        Ok(match &active.target {
            StaticEnvironment::Namespace(name) => name == &package.id.name,
            StaticEnvironment::ClosureBinding(name) => {
                match self.resolve_lexical_name(package, image, lexical_environment, name)? {
                    ResolvedName::PackageBinding { package: owner, binding } if owner == package.id => {
                        image
                            .binding(&binding)
                            .and_then(|binding| binding.closure.as_ref())
                            .is_some_and(|closure| closure.environment == expected)
                    }
                    ResolvedName::PrivateBinding { package: owner, environment, binding } if owner == package.id => {
                        image
                            .private_binding(&environment, &binding)
                            .and_then(|binding| binding.closure.as_ref())
                            .is_some_and(|closure| closure.environment == expected)
                    }
                    _ => false,
                }
            }
        })
    }

    fn process_activation(&mut self, id: PackageId) -> Result<()> {
        let package = self.packages.locate(&id.name)?;
        if self.packages.is_target_provided(&package) {
            self.target_provided.insert(package.id.clone());
            return Ok(());
        }
        let index = self.packages.index(&package)?;
        let node = self.need_node(&Need::Activation { package: id.clone() });

        // Dependency declarations are lookup metadata, not reachability roots.
        // A retained binding that resolves through an import will demand the
        // exact foreign activation/binding. Unused Imports/Depends stay cold.
        if self.is_root(&id) {
            for registration in &index.s3 {
                if let Some((package_name, _generic)) = registration.generic.split_once("::") {
                    if self.package_is_suggested_only(&index, package_name)?
                        && !self.optional_package_selected(package_name)
                    {
                        continue;
                    }
                }
                self.require(
                    node,
                    Need::S3Registration {
                        package: id.clone(),
                        registration: S3Id {
                            generic: registration.generic.clone(),
                            class: registration.class.clone(),
                            method: registration.method.clone(),
                        },
                    },
                    EdgeKind::S3Registration,
                    format!("root activation registers {}/{}", registration.generic, registration.class),
                );
            }
        }

        for native in &index.dynlibs {
            self.require(
                node,
                Need::Native { package: id.clone(), component: native.name.clone() },
                EdgeKind::Native,
                format!("effective useDynLib requires {}", native.name),
            );
        }
        if index.lifecycle.on_load {
            self.require(
                node,
                Need::Lifecycle { package: id.clone(), hook: ".onLoad".into() },
                EdgeKind::Lifecycle,
                "namespace activation requires .onLoad",
            );
        }
        Ok(())
    }

    fn process_resource(&mut self, id: PackageId, resource: String) -> Result<()> {
        let package = self.packages.locate(&id.name)?;
        if self.packages.is_target_provided(&package) {
            self.target_provided.insert(package.id.clone());
            return Ok(());
        }
        let _present = self.packages.resource_exists(&package, &resource)?;
        // An absent system.file() path is a valid result when mustWork is false
        // (the default). The reference is retained only when the installed
        // image actually contains the requested path.
        Ok(())
    }

    fn process_dataset(&mut self, id: PackageId, dataset: String) -> Result<()> {
        let package = self.packages.locate(&id.name)?;
        if self.packages.is_target_provided(&package) {
            self.target_provided.insert(package.id.clone());
            return Ok(());
        }
        let image = self.image(&package)?;
        let node = self.need_node(&Need::Dataset { package: id.clone(), dataset: dataset.clone() });
        if !image.index.datasets.iter().any(|name| name == &dataset) {
            self.diagnostic(node, &id, None, RejectCode::UnresolvedBinding, format!("dataset `{dataset}` is absent from installed image"), None);
        }
        Ok(())
    }

    fn process_s3(&mut self, id: PackageId, registration: S3Id) -> Result<()> {
        let node = self.need_node(&Need::S3Registration { package: id.clone(), registration: registration.clone() });
        self.require(node, Need::Binding { package: id.clone(), binding: registration.method.clone() }, EdgeKind::S3Registration,
            format!("S3 registration {}/{} requires method {}", registration.generic, registration.class, registration.method));
        if let Some((package_name, _generic)) = registration.generic.split_once("::") {
            match self.packages.locate_optional(package_name)? {
                Some(generic_package) => self.require(
                    node,
                    Need::Activation { package: generic_package.id },
                    EdgeKind::S3Registration,
                    format!("S3 generic `{}` requires its namespace", registration.generic),
                ),
                None => self.record_missing_package(
                    node,
                    &id,
                    package_name,
                    EdgeKind::S3Registration,
                    format!("S3 registration requires generic `{}`", registration.generic),
                    None,
                ),
            }
        }
        Ok(())
    }

    fn process_native(&mut self, id: PackageId, component: String) -> Result<()> {
        let package = self.packages.locate(&id.name)?;
        if self.packages.is_target_provided(&package) {
            self.target_provided.insert(package.id.clone());
            return Ok(());
        }
        let index = self.packages.index(&package)?;
        let node = self.need_node(&Need::Native { package: id.clone(), component: component.clone() });
        if let Some(native) = index.dynlibs.iter().find(|native| native.name == component) {
            match &native.safety {
                NativeSafety::Unanalyzed => self.diagnostic(
                    node,
                    &id,
                    None,
                    RejectCode::UnknownNativeLookup,
                    format!("native component `{component}` is demanded but has not been analyzed for R callbacks/dynamic lookup"),
                    None,
                ),
                NativeSafety::Safe(facts) => {
                    for callback in &facts.callbacks {
                        self.require(
                            node,
                            Need::Binding { package: id.clone(), binding: callback.clone() },
                            EdgeKind::Callback,
                            format!("native component `{component}` calls R binding `{callback}`"),
                        );
                    }
                }
                NativeSafety::Unsupported(issues) => self.diagnostic(
                    node,
                    &id,
                    None,
                    RejectCode::UnknownNativeLookup,
                    format!("native component `{component}` is unsupported: {}", issues.join("; ")),
                    None,
                ),
            }
        } else {
            self.diagnostic(
                node,
                &id,
                None,
                RejectCode::UnknownNativeLookup,
                format!("effective namespace metadata has no native component `{component}`"),
                None,
            );
        }
        Ok(())
    }

    fn process_lifecycle(&mut self, id: PackageId, hook: String) -> Result<()> {
        let node = self.need_node(&Need::Lifecycle { package: id.clone(), hook: hook.clone() });
        self.require(node, Need::Binding { package: id, binding: hook.clone() }, EdgeKind::Lifecycle,
            format!("lifecycle hook `{hook}` must be retained"));
        Ok(())
    }

    fn namespace_access(
        &mut self,
        from: NodeId,
        current: &InstalledPackage,
        image: &PackageImage,
        reference: &crate::syntax::PackageRef,
    ) -> Result<()> {
        if reference.package == "base" {
            return Ok(());
        }
        if self.package_is_suggested_only(&image.index, &reference.package)?
            && !self.optional_package_selected(&reference.package)
        {
            // Suggests-only packages are deliberately outside the selected
            // link universe. Leave the optional operation in the retained R
            // source, but do not discover, inspect, internalize, or rewrite
            // that package unless the user enables it with --extra-pkgs.
            return Ok(());
        }
        let Some(foreign) = self.packages.locate_optional(&reference.package)? else {
            self.record_missing_package(
                from,
                &current.id,
                &reference.package,
                EdgeKind::PackageQualified,
                format!("reachable {}{}{} access", reference.package, if reference.internal { ":::" } else { "::" }, reference.symbol),
                Some(reference.span.clone()),
            );
            return Ok(());
        };
        if self.is_root(&current.id) && foreign.id == current.id {
            let binding = if reference.internal {
                reference.symbol.clone()
            } else {
                let index = self.packages.index(&foreign)?;
                index.exports.get(&reference.symbol).cloned().unwrap_or_else(|| reference.symbol.clone())
            };
            self.require_at(
                from,
                Need::Binding { package: current.id.clone(), binding: binding.clone() },
                EdgeKind::PackageQualified,
                format!("root self access {}{}{} resolves to `{binding}`", reference.package, if reference.internal { ":::" } else { "::" }, reference.symbol),
                Some(reference.span.clone()),
            );
            return Ok(());
        }
        if self.packages.is_target_provided(&foreign) {
            self.target_provided.insert(foreign.id.clone());
            let external = self.graph.add_node(&foreign.id.name, NodeKind::ExternalBinding { name: reference.symbol.clone() }, Some(reference.span.clone()));
            self.graph.add_edge_at(
                from,
                external,
                EdgeKind::PackageQualified,
                format!("{} access to target-provided binding", if reference.internal { ":::" } else { "::" }),
                Some(reference.span.clone()),
            );
            return Ok(());
        }
        let index = self.packages.index(&foreign)?;
        let binding = if reference.internal {
            reference.symbol.clone()
        } else {
            index.exports.get(&reference.symbol).cloned().unwrap_or_else(|| reference.symbol.clone())
        };
        self.require_at(
            from,
            Need::Activation { package: foreign.id.clone() },
            EdgeKind::NamespaceLoad,
            "qualified namespace access requires activation",
            Some(reference.span.clone()),
        );
        self.require_at(
            from,
            Need::Binding { package: foreign.id.clone(), binding: binding.clone() },
            EdgeKind::PackageQualified,
            format!("{}{}{} resolves to `{binding}`", reference.package, if reference.internal { ":::" } else { "::" }, reference.symbol),
            Some(reference.span.clone()),
        );
        self.rewrites.push(Rewrite::NamespaceAccess {
            source: reference.span.clone(),
            package: foreign.id,
            binding,
            internal: reference.internal,
        });
        Ok(())
    }

    fn resource_access(
        &mut self,
        from: NodeId,
        current: &InstalledPackage,
        image: &PackageImage,
        resource: &crate::syntax::ResourceRef,
    ) -> Result<()> {
        let Some(package_name) = &resource.package else {
            if resource.path.is_none() {
                self.diagnostic(from, &current.id, None, RejectCode::DynamicLookup, "dynamic system.file() resource path", Some(resource.span.clone()));
            }
            return Ok(());
        };

        // The root remains a real installed package. Preserve its own package
        // path/help/Meta semantics exactly; no synthetic resource rewrite is
        // required for system.file(..., package = <root>).
        if self.is_root(&current.id) && package_name == &current.id.name {
            return Ok(());
        }

        if self.package_is_suggested_only(&image.index, package_name)?
            && !self.optional_package_selected(package_name)
        {
            return Ok(());
        }

        let Some(foreign) = self.packages.locate_optional(package_name)? else {
            self.record_missing_package(
                from,
                &current.id,
                package_name,
                EdgeKind::Resource,
                format!("system.file references package {package_name}"),
                Some(resource.span.clone()),
            );
            return Ok(());
        };
        if self.packages.is_target_provided(&foreign) {
            self.target_provided.insert(foreign.id.clone());
            return Ok(());
        }
        let Some(path) = &resource.path else {
            self.diagnostic(from, &current.id, None, RejectCode::DynamicLookup, format!("dynamic system.file() path for package {package_name}"), Some(resource.span.clone()));
            return Ok(());
        };
        if !self.packages.resource_exists(&foreign, path)? {
            match resource.must_work {
                Some(false) => return Ok(()),
                Some(true) => {
                    self.diagnostic(
                        from,
                        &current.id,
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
                        &current.id,
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
            Need::Resource { package: foreign.id.clone(), resource: path.clone() },
            EdgeKind::Resource,
            format!("system.file requires {package_name}/{path}"),
            Some(resource.span.clone()),
        );
        self.rewrites.push(Rewrite::ResourceAccess { source: resource.span.clone(), package: foreign.id, resource: path.clone() });
        Ok(())
    }

    fn optional_package_selected(&self, name: &str) -> bool {
        self.extra_packages.contains(name)
    }

    fn package_is_suggested_only(&mut self, index: &PackageIndex, name: &str) -> Result<bool> {
        if !self.suggested_only.contains_key(&index.package.id) {
            let mut required = HashSet::new();
            for import in &index.imports {
                let package = match import {
                    ImportSpec::All { package, .. } | ImportSpec::From { package, .. } => package,
                };
                required.insert(package.clone());
            }
            for dependency in relations(&index.description, RelationField::Imports)
                .map_err(Error::Analysis)?
                .into_iter()
                .chain(relations(&index.description, RelationField::Depends).map_err(Error::Analysis)?)
            {
                required.insert(dependency.package().to_owned());
            }

            let suggested = relations(&index.description, RelationField::Suggests)
                .map_err(Error::Analysis)?
                .into_iter()
                .filter_map(|dependency| {
                    let name = dependency.package().to_owned();
                    (!required.contains(&name)).then_some(name)
                })
                .collect::<HashSet<_>>();
            self.suggested_only.insert(index.package.id.clone(), suggested);
        }
        Ok(self
            .suggested_only
            .get(&index.package.id)
            .is_some_and(|packages| packages.contains(name)))
    }

    fn native_component_for_binding<'a>(index: &'a PackageIndex, name: &str) -> Option<&'a str> {
        // Only explicit routine bindings are evidence. registrationFixes tells
        // us how R names registered routines, but not which routines exist.
        // Treating an arbitrary prefix/suffix match as native would hide real
        // unresolved R names.
        let mut matches = index
            .dynlibs
            .iter()
            .filter(|native| native.symbols.iter().any(|symbol| symbol.binding == name));
        let first = matches.next()?;
        if matches.next().is_some() {
            None
        } else {
            Some(first.name.as_str())
        }
    }

    fn native_component_for_call<'a>(index: &'a PackageIndex, call: &CallSite) -> Option<&'a str> {
        let selector = call.args.first()?.as_ref()?;
        let name = match selector {
            StaticArg::Symbol(name) | StaticArg::String(name) => name,
        };
        if let Some(component) = Self::native_component_for_binding(index, name) {
            return Some(component);
        }

        // With .registration=TRUE, R creates RegisteredNativeSymbol variables
        // for every routine reported by the loaded DLL. nsInfo.rds records the
        // registration policy but not that runtime routine table. A static
        // symbol selector can therefore be associated with the sole registered
        // package DLL even when its exact routine name is unavailable until DLL
        // load. String selectors do not have that lexical binding guarantee.
        if !matches!(selector, StaticArg::Symbol(_)) {
            return None;
        }
        let mut registered = index
            .dynlibs
            .iter()
            .filter(|native| native.registration.is_some() && matches!(native.safety, NativeSafety::Unanalyzed));
        let first = registered.next()?;
        if registered.next().is_some() {
            None
        } else {
            Some(first.name.as_str())
        }
    }

    fn sole_opaque_registered_native_component(index: &PackageIndex) -> Option<&str> {
        let mut registered = index
            .dynlibs
            .iter()
            .filter(|native| native.registration.is_some() && matches!(native.safety, NativeSafety::Unanalyzed));
        let first = registered.next()?;
        if registered.next().is_some() {
            None
        } else {
            Some(first.name.as_str())
        }
    }

    fn handle_superassignment(
        &mut self,
        from: NodeId,
        package: &InstalledPackage,
        image: &PackageImage,
        binding: &str,
        lexical_environment: &str,
        effect: &SyntaxEffect,
    ) -> Result<()> {
        if let Some(value) = &effect.value_symbol {
            let resolved = self.resolve_lexical_name(package, image, lexical_environment, value)?;
            if binding == ".onLoad" && matches!(resolved, ResolvedName::Unknown(_)) {
                if let Some(component) = Self::sole_opaque_registered_native_component(&image.index) {
                    // With .registration=TRUE the loader creates native-symbol
                    // variables before .onLoad, but nsInfo.rds does not contain
                    // the runtime routine table. If native safety is still
                    // opaque, associate this otherwise-unresolved .onLoad RHS
                    // with the sole registered DLL. The component remains a
                    // blocker until native analysis proves its behavior, so
                    // this cannot turn an unknown symbol into an accepted link.
                    self.require_at(
                        from,
                        Need::Native { package: package.id.clone(), component: component.to_owned() },
                        EdgeKind::Native,
                        format!(".onLoad may receive registered native symbol `{value}` from `{component}`"),
                        Some(effect.span.clone()),
                    );
                } else {
                    self.require_resolved(
                        from,
                        &package.id,
                        Some(binding),
                        resolved,
                        effect.span.clone(),
                    )?;
                }
            } else {
                self.require_resolved(
                    from,
                    &package.id,
                    Some(binding),
                    resolved,
                    effect.span.clone(),
                )?;
            }
        }

        if effect.target_enclosing_local {
            return Ok(());
        }

        let Some(target) = &effect.target else {
            self.diagnostic(
                from,
                &package.id,
                Some(binding),
                RejectCode::EnvironmentMutation,
                "dynamic superassignment target cannot be resolved",
                Some(effect.span.clone()),
            );
            return Ok(());
        };

        match self.resolve_lexical_name(package, image, lexical_environment, target)? {
            ResolvedName::PackageBinding { package: owner, binding: target_binding } => self.require_at(
                from,
                Need::Binding { package: owner, binding: target_binding.clone() },
                EdgeKind::Effect,
                format!("superassignment mutates enclosing binding `{target_binding}`"),
                Some(effect.span.clone()),
            ),
            ResolvedName::PrivateBinding { package: owner, environment, binding: target_binding } => self.require_at(
                from,
                Need::PrivateBinding { package: owner, environment: environment.clone(), binding: target_binding.clone() },
                EdgeKind::Effect,
                format!("superassignment mutates private binding `{target_binding}` in {environment}"),
                Some(effect.span.clone()),
            ),
            ResolvedName::NativeSymbol { .. }
            | ResolvedName::Imported { .. }
            | ResolvedName::TargetProvided { .. }
            | ResolvedName::PackageMetadata { .. }
            | ResolvedName::MissingPackage { .. }
            | ResolvedName::Base(_)
            | ResolvedName::Local(_)
            | ResolvedName::Unknown(_) => self.diagnostic(
                from,
                &package.id,
                Some(binding),
                RejectCode::EnvironmentMutation,
                format!("superassignment target `{target}` does not resolve to a mutable enclosing package/private binding"),
                Some(effect.span.clone()),
            ),
        }
        Ok(())
    }

    fn is_slinker_semantic_callee(name: &str) -> bool {
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
                | ".Call"
                | ".External"
                | ".C"
                | ".Fortran"
                | "deparse"
                | "substitute"
                | "match.call"
                | "isNamespaceLoaded"
                | "getNamespaceExports"
                | "setHook"
                | "packageEvent"
                | "makeActiveBinding"
                | "environment"
        )
    }

    fn semantic_call(
        &mut self,
        from: NodeId,
        current: &InstalledPackage,
        image: &PackageImage,
        binding: &str,
        call: &CallSite,
    ) -> Result<()> {
        match call.callee_kind {
            CalleeKind::DefinitelyLexical => return Ok(()),
            CalleeKind::ConditionalFallthrough => {
                // Oak found at least one reaching lexical definition, but also
                // a path that falls through to the installed namespace. Slinker
                // must not apply linker-specific effects unless the callee
                // identity is path-invariant. If the fallthrough target is the
                // base primitive/function that slinker specializes, block the
                // rewrite instead of pretending either branch is definitive.
                if call.qualified_package.is_none()
                    && Self::is_slinker_semantic_callee(&call.callee)
                    && matches!(self.resolve_name(current, image, &call.callee)?, ResolvedName::Base(_))
                {
                    self.diagnostic(
                        from,
                        &current.id,
                        Some(binding),
                        RejectCode::SemanticAmbiguity,
                        format!(
                            "conditionally local callee `{}` can fall through to base; linker-specific effects are path-dependent",
                            call.callee
                        ),
                        Some(call.span.clone()),
                    );
                }
                return Ok(());
            }
            CalleeKind::DefinitelyExternal => {}
        }
        match call.qualified_package.as_deref() {
            Some("base") => {}
            Some(_) => return Ok(()),
            None => {
                let resolved = self.resolve_name(current, image, &call.callee)?;
                if !matches!(resolved, ResolvedName::Base(_)) {
                    return Ok(());
                }
            }
        }
        match call.callee.as_str() {
            "library" | "require" => {
                let package = static_package_arg(call);
                if let Some(name) = package {
                    if self.package_is_suggested_only(&image.index, name)?
                        && !self.optional_package_selected(name)
                    {
                        return Ok(());
                    }
                }
                self.diagnostic(from, &current.id, None, RejectCode::PackageAttachmentUnsupported,
                    match package { Some(name) => format!("search-path attachment of `{name}` is outside the current contract"), None => "dynamic package attachment is outside the current contract".into() },
                    Some(call.span.clone()));
            }
            "requireNamespace" | "loadNamespace" | "getNamespace" | "asNamespace" => {
                let Some(name) = static_string_arg(call) else {
                    self.diagnostic(
                        from,
                        &current.id,
                        None,
                        RejectCode::DynamicPackageDiscovery,
                        "dynamic namespace discovery",
                        Some(call.span.clone()),
                    );
                    return Ok(());
                };
                if self.is_root(&current.id) && name == current.id.name {
                    return Ok(());
                }
                let operation = match call.callee.as_str() {
                    "requireNamespace" => PackageOperation::RequireNamespace { result: true },
                    "loadNamespace" => PackageOperation::LoadNamespace,
                    "getNamespace" => PackageOperation::GetNamespace,
                    "asNamespace" => PackageOperation::AsNamespace,
                    _ => unreachable!(),
                };
                let suggested = self.package_is_suggested_only(&image.index, name)?;
                let discovery_policy = if self.optional_package_selected(name) {
                    DiscoveryPolicy::Internalize
                } else if suggested && call.callee == "requireNamespace" {
                    self.rewrites.push(Rewrite::PackageOperation {
                        source: call.span.clone(),
                        package: None,
                        operation: PackageOperation::RequireNamespace { result: false },
                    });
                    return Ok(());
                } else if suggested {
                    return Ok(());
                } else {
                    self.policy.namespace_discovery
                };
                match discovery_policy {
                    DiscoveryPolicy::Reject => self.diagnostic(
                        from,
                        &current.id,
                        None,
                        RejectCode::DynamicPackageDiscovery,
                        format!("reachable {} for `{name}` is not specialized by policy", call.callee),
                        Some(call.span.clone()),
                    ),
                    DiscoveryPolicy::TargetProvidedOnly => match self.packages.locate_optional(name)? {
                        Some(foreign) if self.packages.is_target_provided(&foreign) => {
                            self.target_provided.insert(foreign.id);
                        }
                        Some(_) => self.diagnostic(
                            from,
                            &current.id,
                            None,
                            RejectCode::DynamicPackageDiscovery,
                            format!("`{name}` is installed but is not an exact target-provided namespace"),
                            Some(call.span.clone()),
                        ),
                        None if call.callee == "requireNamespace" => {
                            self.rewrites.push(Rewrite::PackageOperation {
                                source: call.span.clone(),
                                package: None,
                                operation: PackageOperation::RequireNamespace { result: false },
                            });
                        }
                        None => self.record_missing_package(
                            from,
                            &current.id,
                            name,
                            EdgeKind::Discovery,
                            format!("{} requires unavailable namespace {name}", call.callee),
                            Some(call.span.clone()),
                        ),
                    },
                    DiscoveryPolicy::Internalize => match self.packages.locate_optional(name)? {
                        Some(foreign) if self.packages.is_target_provided(&foreign) => {
                            self.target_provided.insert(foreign.id);
                        }
                        Some(foreign) => {
                            self.require_at(
                                from,
                                Need::Activation { package: foreign.id.clone() },
                                EdgeKind::Discovery,
                                format!("specialized {} requires `{name}`", call.callee),
                                Some(call.span.clone()),
                            );
                            self.rewrites.push(Rewrite::PackageOperation {
                                source: call.span.clone(),
                                package: Some(foreign.id),
                                operation,
                            });
                        }
                        None if call.callee == "requireNamespace" && !self.optional_package_selected(name) => {
                            self.rewrites.push(Rewrite::PackageOperation {
                                source: call.span.clone(),
                                package: None,
                                operation: PackageOperation::RequireNamespace { result: false },
                            });
                        }
                        None => self.record_missing_package(
                            from,
                            &current.id,
                            name,
                            EdgeKind::Discovery,
                            format!("{} requires unavailable namespace {name}", call.callee),
                            Some(call.span.clone()),
                        ),
                    },
                }
            }
            "packageVersion" => self.identity_query(from, current, image, call, true)?,
            "find.package" => self.identity_query(from, current, image, call, false)?,
            ".Call" | ".External" | ".C" | ".Fortran" => {
                if let Some(component) = Self::native_component_for_call(&image.index, call) {
                    self.require_at(
                        from,
                        Need::Native { package: current.id.clone(), component: component.to_owned() },
                        EdgeKind::Native,
                        format!("reachable {} resolves its static native selector through `{component}`", call.callee),
                        Some(call.span.clone()),
                    );
                } else {
                    self.diagnostic(
                        from,
                        &current.id,
                        None,
                        RejectCode::UnknownNativeLookup,
                        format!("{} native selector cannot be resolved to one registered package DLL", call.callee),
                        Some(call.span.clone()),
                    );
                }
            }
            "deparse" | "substitute" | "match.call" => self.observations.push(SyntaxObservation {
                node: from,
                package: current.id.clone(),
                span: call.span.clone(),
                kind: call.callee.clone(),
            }),
            _ => {}
        }
        Ok(())
    }

    fn identity_query(
        &mut self,
        from: NodeId,
        current: &InstalledPackage,
        image: &PackageImage,
        call: &CallSite,
        version: bool,
    ) -> Result<()> {
        let Some(name) = static_string_arg(call) else {
            self.diagnostic(
                from,
                &current.id,
                None,
                RejectCode::DynamicPackageDiscovery,
                "dynamic package identity query",
                Some(call.span.clone()),
            );
            return Ok(());
        };
        if self.is_root(&current.id) && name == current.id.name {
            return Ok(());
        }
        if self.package_is_suggested_only(&image.index, name)? && !self.optional_package_selected(name) {
            return Ok(());
        }
        let discovery_policy = if self.optional_package_selected(name) {
            DiscoveryPolicy::Internalize
        } else {
            self.policy.namespace_discovery
        };
        match discovery_policy {
            DiscoveryPolicy::Reject => self.diagnostic(
                from,
                &current.id,
                None,
                RejectCode::DynamicPackageDiscovery,
                format!("reachable package identity query for `{name}` is not specialized by policy"),
                Some(call.span.clone()),
            ),
            DiscoveryPolicy::TargetProvidedOnly => match self.packages.locate_optional(name)? {
                Some(foreign) if self.packages.is_target_provided(&foreign) => {
                    self.target_provided.insert(foreign.id);
                }
                Some(_) => self.diagnostic(
                    from,
                    &current.id,
                    None,
                    RejectCode::DynamicPackageDiscovery,
                    format!("package identity query for `{name}` is not target-provided"),
                    Some(call.span.clone()),
                ),
                None => self.record_missing_package(
                    from,
                    &current.id,
                    name,
                    EdgeKind::Discovery,
                    format!("{} requires unavailable package {name}", call.callee),
                    Some(call.span.clone()),
                ),
            },
            DiscoveryPolicy::Internalize => match self.packages.locate_optional(name)? {
                Some(foreign) if self.packages.is_target_provided(&foreign) => {
                    self.target_provided.insert(foreign.id);
                }
                Some(foreign) => {
                    let operation = if version {
                        PackageOperation::PackageVersion { version: foreign.id.version.to_string() }
                    } else {
                        PackageOperation::FindPackage
                    };
                    self.rewrites.push(Rewrite::PackageOperation {
                        source: call.span.clone(),
                        package: Some(foreign.id),
                        operation,
                    });
                }
                None => self.record_missing_package(
                    from,
                    &current.id,
                    name,
                    EdgeKind::Discovery,
                    format!("{} requires unavailable package {name}", call.callee),
                    Some(call.span.clone()),
                ),
            },
        }
        Ok(())
    }

    fn namespace_imports(&mut self, image: &PackageImage) -> Result<NamespaceImports> {
        if let Some(imports) = self.namespace_imports.get(&image.index.package.id) {
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
            let exports = match self.packages.locate_optional(package_name)? {
                Some(foreign) => Some(self.packages.index(&foreign)?.exports.clone()),
                None => None,
            };
            imports.add_import_all(package_name, exports, except.iter().cloned());
        }

        self.namespace_imports
            .insert(image.index.package.id.clone(), imports.clone());
        Ok(imports)
    }

    fn oak_parse_context(&mut self, image: &PackageImage, lexical_environment: &str) -> Result<OakParseContext> {
        let mut shadowed = BTreeSet::new();
        shadowed.extend(image.bindings.keys().cloned());
        shadowed.extend(
            self.activation_bindings
                .iter()
                .filter(|(package, _)| package == &image.index.package.id)
                .map(|(_, binding)| binding.clone()),
        );
        for component in &image.index.dynlibs {
            shadowed.extend(component.symbols.iter().map(|symbol| symbol.binding.clone()));
        }

        let mut environment = lexical_environment.to_owned();
        let mut seen = HashSet::new();
        while seen.insert(environment.clone()) {
            let Some(private) = image.private_environment(&environment) else { break };
            shadowed.extend(private.bindings.keys().cloned());
            environment = private.parent.clone();
        }

        let imports = self.namespace_imports(image)?;
        Ok(OakParseContext::with_imports(shadowed, imports))
    }

    fn private_source_key(environment: &str, binding: &str) -> String {
        format!("{environment}${binding}")
    }

    fn resolve_lexical_name(
        &mut self,
        current: &InstalledPackage,
        image: &PackageImage,
        lexical_environment: &str,
        name: &str,
    ) -> Result<ResolvedName> {
        let mut environment = lexical_environment.to_owned();
        let mut seen = HashSet::new();
        loop {
            if !seen.insert(environment.clone()) {
                return Ok(ResolvedName::Unknown(name.to_owned()));
            }
            if let Some(private) = image.private_environment(&environment) {
                if private.bindings.contains_key(name) {
                    return Ok(ResolvedName::PrivateBinding {
                        package: current.id.clone(),
                        environment: private.id.clone(),
                        binding: name.to_owned(),
                    });
                }
                environment = private.parent.clone();
                continue;
            }
            if environment == format!("namespace:{}", current.id.name) {
                return self.resolve_name(current, image, name);
            }
            if let Some(namespace) = environment.strip_prefix("namespace:") {
                let Some(foreign) = self.packages.locate_optional(namespace)? else {
                    return Ok(ResolvedName::MissingPackage {
                        package: namespace.to_owned(),
                        binding: Some(name.to_owned()),
                    });
                };
                if self.packages.is_target_provided(&foreign) {
                    self.target_provided.insert(foreign.id.clone());
                    return Ok(ResolvedName::TargetProvided {
                        package: foreign.id,
                        binding: name.to_owned(),
                    });
                }
                let foreign_image = self.image(&foreign)?;
                return self.resolve_name(&foreign, &foreign_image, name);
            }
            if environment == "base:base" || environment == "base:empty" {
                return Ok(if self.packages.is_base_binding(name) {
                    ResolvedName::Base(name.to_owned())
                } else {
                    ResolvedName::Unknown(name.to_owned())
                });
            }
            return Ok(ResolvedName::Unknown(name.to_owned()));
        }
    }

    fn resolve_name(&mut self, current: &InstalledPackage, image: &PackageImage, name: &str) -> Result<ResolvedName> {
        if matches!(name, ".packageName" | ".__S3MethodsTable__.") {
            return Ok(ResolvedName::PackageMetadata { package: current.id.clone(), name: name.to_owned() });
        }
        if name == ".__NAMESPACE__." {
            return Ok(ResolvedName::PackageMetadata { package: current.id.clone(), name: name.to_owned() });
        }
        if image.bindings.contains_key(name)
            || self.activation_bindings.contains(&(current.id.clone(), name.to_owned()))
        {
            return Ok(ResolvedName::PackageBinding { package: current.id.clone(), binding: name.to_owned() });
        }

        if let Some(component) = Self::native_component_for_binding(&image.index, name) {
            return Ok(ResolvedName::NativeSymbol {
                package: current.id.clone(),
                component: component.to_owned(),
                binding: name.to_owned(),
            });
        }

        match self.namespace_imports(image)?.resolve(name) {
            NamespaceImportResolution::Imported {
                package: package_name,
                binding,
                ..
            } => {
                let Some(foreign) = self.packages.locate_optional(&package_name)? else {
                    return Ok(ResolvedName::MissingPackage {
                        package: package_name,
                        binding: Some(binding),
                    });
                };
                return Ok(if self.packages.is_target_provided(&foreign) {
                    self.target_provided.insert(foreign.id.clone());
                    ResolvedName::TargetProvided {
                        package: foreign.id,
                        binding,
                    }
                } else {
                    ResolvedName::Imported {
                        package: foreign.id,
                        binding,
                    }
                });
            }
            NamespaceImportResolution::MissingImportAll { package, binding } => {
                return Ok(ResolvedName::MissingPackage {
                    package,
                    binding: Some(binding),
                });
            }
            NamespaceImportResolution::BaseFallback => {}
        }

        if self.packages.is_base_binding(name) {
            Ok(ResolvedName::Base(name.to_owned()))
        } else {
            Ok(ResolvedName::Unknown(name.to_owned()))
        }
    }

    fn require_resolved(
        &mut self,
        from: NodeId,
        requester: &PackageId,
        binding: Option<&str>,
        resolved: ResolvedName,
        span: Span,
    ) -> Result<()> {
        match resolved {
            ResolvedName::PackageBinding { package, binding } => self.require_at(
                from,
                Need::Binding { package, binding: binding.clone() },
                EdgeKind::Lexical,
                format!("lexical reference `{binding}`"),
                Some(span.clone()),
            ),
            ResolvedName::PrivateBinding { package, environment, binding } => self.require_at(
                from,
                Need::PrivateBinding { package, environment: environment.clone(), binding: binding.clone() },
                EdgeKind::Lexical,
                format!("lexical private reference `{binding}` in {environment}"),
                Some(span.clone()),
            ),
            ResolvedName::NativeSymbol { package, component, binding: native_binding } => self.require_at(
                from,
                Need::Native { package, component: component.clone() },
                EdgeKind::Native,
                format!("registered native symbol `{native_binding}` is provided by `{component}`"),
                Some(span.clone()),
            ),
            ResolvedName::Imported { package, binding } => {
                self.require_at(
                    from,
                    Need::Activation { package: package.clone() },
                    EdgeKind::Import,
                    "imported binding requires namespace activation",
                    Some(span.clone()),
                );
                self.require_at(
                    from,
                    Need::Binding { package, binding: binding.clone() },
                    EdgeKind::Import,
                    format!("imported binding `{binding}`"),
                    Some(span.clone()),
                );
            }
            ResolvedName::TargetProvided { package, binding } => {
                let node = self.graph.add_node(
                    package.name.clone(),
                    NodeKind::ExternalBinding { name: binding.clone() },
                    Some(span.clone()),
                );
                self.graph.add_edge_at(
                    from,
                    node,
                    EdgeKind::Import,
                    format!("target-provided imported binding `{binding}`"),
                    Some(span),
                );
            }
            ResolvedName::PackageMetadata { package, name } => {
                let node = self.graph.add_node(
                    package.name.clone(),
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
                if name == ".__NAMESPACE__." && !self.is_root(&package) {
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
            ResolvedName::MissingPackage { package, binding } => {
                let detail = binding.as_deref().map(|name| format!("imported binding `{name}` requires missing namespace {package}"))
                    .unwrap_or_else(|| format!("reachable reference requires missing namespace {package}"));
                self.record_missing_package(from, requester, &package, EdgeKind::Import, detail, Some(span));
            }
            ResolvedName::Local(_) | ResolvedName::Base(_) => {}
            ResolvedName::Unknown(name) => {
                self.diagnostic(from, requester, binding, RejectCode::UnresolvedBinding, format!("unresolved name `{name}`"), Some(span));
            }
        }
        Ok(())
    }

    fn require_root(&mut self, need: Need) {
        self.encountered.insert(need.package().clone());
        let node = self.need_node(&need);
        if !self.roots.contains(&node) {
            self.roots.push(node);
        }
        if !self.processed.contains(&need) && self.queued.insert(need.clone()) {
            self.pending.push_back(need);
        }
    }

    fn require(&mut self, from: NodeId, need: Need, kind: EdgeKind, reason: impl Into<String>) {
        self.require_at(from, need, kind, reason, None);
    }

    fn require_at(
        &mut self,
        from: NodeId,
        need: Need,
        kind: EdgeKind,
        reason: impl Into<String>,
        span: Option<Span>,
    ) {
        self.encountered.insert(need.package().clone());
        let to = self.need_node(&need);
        self.graph.add_edge_at(from, to, kind, reason, span);
        if !self.processed.contains(&need) && self.queued.insert(need.clone()) {
            self.pending.push_back(need);
        }
    }

    fn need_node(&mut self, need: &Need) -> NodeId {
        let package = &need.package().name;
        let kind = match need {
            Need::Binding { binding, .. } => NodeKind::Binding { name: binding.clone() },
            Need::PrivateBinding { environment, binding, .. } => NodeKind::PrivateBinding { environment: environment.clone(), name: binding.clone() },
            Need::Activation { .. } => NodeKind::Activation,
            Need::Resource { resource, .. } => NodeKind::Resource { path: resource.clone() },
            Need::Dataset { dataset, .. } => NodeKind::Dataset { name: dataset.clone() },
            Need::S3Registration { registration, .. } => NodeKind::S3Registration { generic: registration.generic.clone(), class: registration.class.clone() },
            Need::Native { component, .. } => NodeKind::NativeComponent { name: component.clone() },
            Need::Lifecycle { hook, .. } => NodeKind::Lifecycle { hook: hook.clone() },
        };
        self.graph.add_node(package, kind, None)
    }

    fn diagnostic(
        &mut self,
        node: NodeId,
        package: &PackageId,
        binding: Option<&str>,
        code: RejectCode,
        message: impl Into<String>,
        span: Option<Span>,
    ) {
        let message = message.into();
        if !self.diagnostic_keys.insert((node, code, message.clone())) {
            return;
        }
        self.diagnostics.push(Diagnostic {
            package: package.name.clone(),
            binding: binding.map(str::to_owned),
            code,
            message,
            span,
            node: Some(node),
            reachable: true,
        });
    }

    fn is_root(&self, id: &PackageId) -> bool {
        self.root.as_ref().is_some_and(|root| root == id)
    }

    fn record_missing_package(
        &mut self,
        from: NodeId,
        requester: &PackageId,
        missing: &str,
        kind: EdgeKind,
        reason: impl Into<String>,
        span: Option<Span>,
    ) {
        let reason = reason.into();
        let node = self.graph.add_node(missing, NodeKind::MissingPackage, span.clone());
        self.graph.add_edge_at(from, node, kind, reason.clone(), span.clone());
        self.diagnostic(
            node,
            requester,
            None,
            RejectCode::MissingDependency,
            format!("required package `{missing}` is absent from the selected target library universe ({reason})"),
            span,
        );
    }

    fn finalize_syntax_observations(&mut self) {
        if self.observations.is_empty() || self.rewrites.is_empty() {
            return;
        }
        let rewrites = self.rewrites.iter().map(rewrite_span).cloned().collect::<Vec<_>>();
        for observation in self.observations.clone() {
            if rewrites.iter().any(|rewrite| spans_overlap(&observation.span, rewrite)) {
                self.diagnostic(
                    observation.node,
                    &observation.package,
                    None,
                    RejectCode::SyntaxObservation,
                    format!("{} can observe syntax changed by a planned rewrite", observation.kind),
                    Some(observation.span),
                );
            }
        }
    }
}

fn rewrite_span(rewrite: &Rewrite) -> &Span {
    match rewrite {
        Rewrite::NamespaceAccess { source, .. }
        | Rewrite::ResourceAccess { source, .. }
        | Rewrite::PackageOperation { source, .. } => source,
    }
}

fn spans_overlap(left: &Span, right: &Span) -> bool {
    left.source == right.source && left.start < right.end && right.start < left.end
}

fn static_string_arg(call: &CallSite) -> Option<&str> {
    match call.args.first()?.as_ref()? {
        StaticArg::String(value) => Some(value),
        StaticArg::Symbol(_) => None,
    }
}

fn static_package_arg(call: &CallSite) -> Option<&str> {
    match call.args.first()?.as_ref()? {
        StaticArg::String(value) | StaticArg::Symbol(value) => Some(value),
    }
}
