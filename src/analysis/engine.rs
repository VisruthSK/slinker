use crate::analysis::policy::{DiscoveryPolicy, LinkPolicy};
use crate::analysis::{Diagnostic, EdgeKind, Graph, Need, NodeId, NodeKind, RejectCode, S3Id};
use crate::build::Rewrite;
use crate::package::{
    BindingImage, ImportSpec, InstalledPackage, ObjectKind, PackageId, PackageImage,
    PackageProvider, SyntaxValidation,
};
use crate::syntax::{
    AirParser, CallSite, ParsedRFile, ResolvedName, SourceId, Sources, Span, StaticArg,
    SyntaxEffectKind,
};
use crate::{Error, Result};
use rayon::prelude::*;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;

#[derive(Debug)]
pub struct LinkPlan {
    pub graph: Graph,
    pub roots: Vec<NodeId>,
    pub diagnostics: Vec<Diagnostic>,
    pub rewrites: Vec<Rewrite>,
    pub sources: Sources,
    pub retained: HashSet<Need>,
    pub parsed_bindings: usize,
    pub inspected_packages: usize,
    pub images: HashMap<PackageId, Arc<PackageImage>>,
}

#[derive(Clone, Debug)]
enum ParseState {
    Parsed(ParsedRFile),
    Blocked,
}

pub struct Linker<P: PackageProvider> {
    packages: P,
    policy: LinkPolicy,
    jobs: usize,
    graph: Graph,
    roots: Vec<NodeId>,
    pending: VecDeque<Need>,
    processed: HashSet<Need>,
    parsed_bindings: HashMap<(PackageId, String), ParseState>,
    images: HashMap<PackageId, Arc<PackageImage>>,
    diagnostics: Vec<Diagnostic>,
    rewrites: Vec<Rewrite>,
    sources: Sources,
    source_ids: HashMap<(PackageId, String), SourceId>,
}

impl<P: PackageProvider> Linker<P> {
    pub fn new(packages: P, jobs: usize) -> Self {
        Self {
            packages,
            policy: LinkPolicy::default(),
            jobs: jobs.max(1),
            graph: Graph::default(),
            roots: Vec::new(),
            pending: VecDeque::new(),
            processed: HashSet::new(),
            parsed_bindings: HashMap::new(),
            images: HashMap::new(),
            diagnostics: Vec::new(),
            rewrites: Vec::new(),
            sources: Sources::default(),
            source_ids: HashMap::new(),
        }
    }

    pub fn with_policy(mut self, policy: LinkPolicy) -> Self {
        self.policy = policy;
        self
    }

    pub fn analyze(mut self, root_name: &str) -> Result<LinkPlan> {
        let root = self.packages.locate(root_name)?;
        if self.packages.is_target_provided(&root) {
            return Err(Error::Analysis(format!(
                "root package `{root_name}` cannot be target-provided"
            )));
        }
        let root_image = self.image(&root)?;
        self.preparse_root(&root, &root_image)?;

        self.require_root(Need::Activation {
            package: root.id.clone(),
        });
        for binding in &root_image.index.binding_names {
            self.require_root(Need::Binding {
                package: root.id.clone(),
                binding: binding.clone(),
            });
        }

        while !self.pending.is_empty() {
            self.prefetch_pending_images()?;
            let need = self
                .pending
                .pop_front()
                .expect("pending queue is non-empty");
            if !self.processed.insert(need.clone()) {
                continue;
            }
            self.process_need(need)?;
        }

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
            parsed_bindings,
            inspected_packages,
            images: self.images,
        })
    }

    fn image(&mut self, package: &InstalledPackage) -> Result<Arc<PackageImage>> {
        if let Some(image) = self.images.get(&package.id) {
            return Ok(Arc::clone(image));
        }
        let image = self.packages.image(package)?;
        self.images.insert(package.id.clone(), Arc::clone(&image));
        Ok(image)
    }

    fn preparse_root(&mut self, root: &InstalledPackage, image: &PackageImage) -> Result<()> {
        let mut work = Vec::new();
        for binding in image.bindings.values() {
            let Some(closure) = &binding.closure else {
                continue;
            };
            let key = (root.id.clone(), binding.name.clone());
            let source_id = self.sources.add_binding(
                root.id.name.clone(),
                binding.name.clone(),
                Arc::clone(&closure.source),
            );
            self.source_ids.insert(key.clone(), source_id.clone());
            work.push((key, source_id, Arc::clone(&closure.source)));
        }
        if work.is_empty() {
            return Ok(());
        }

        let parse = || {
            work.par_iter()
                .map(|(key, source, text)| {
                    (
                        key.clone(),
                        AirParser.parse_binding(source.clone(), text.as_ref()),
                    )
                })
                .collect::<Vec<_>>()
        };
        let results = if self.jobs > 1 && work.len() > 1 {
            rayon::ThreadPoolBuilder::new()
                .num_threads(self.jobs.min(work.len()))
                .build()
                .map_err(|error| Error::Analysis(format!("failed to create Rayon pool: {error}")))?
                .install(parse)
        } else {
            work.iter()
                .map(|(key, source, text)| {
                    (
                        key.clone(),
                        AirParser.parse_binding(source.clone(), text.as_ref()),
                    )
                })
                .collect()
        };

        for (key, result) in results {
            match result {
                Ok(parsed) => {
                    self.parsed_bindings.insert(key, ParseState::Parsed(parsed));
                }
                Err(error) => {
                    self.handle_air_rejection(&key.0, &key.1, error)?;
                }
            }
        }
        Ok(())
    }

    fn prefetch_pending_images(&mut self) -> Result<()> {
        if self.jobs <= 1 {
            return Ok(());
        }

        let mut seen = HashSet::new();
        let mut packages = Vec::new();
        for need in &self.pending {
            let id = need.package();
            if self.images.contains_key(id) || !seen.insert(id.clone()) {
                continue;
            }
            let package = self.packages.locate(&id.name)?;
            if !self.packages.is_target_provided(&package) {
                packages.push(package);
            }
        }
        if packages.len() > 1 {
            self.packages.prefetch(&packages, self.jobs)?;
        }
        Ok(())
    }

    fn process_need(&mut self, need: Need) -> Result<()> {
        match need {
            Need::Binding { package, binding } => self.process_binding(package, binding),
            Need::Activation { package } => self.process_activation(package),
            Need::Resource { package, resource } => self.process_resource(package, resource),
            Need::Dataset { package, dataset } => self.process_dataset(package, dataset),
            Need::S3Registration {
                package,
                registration,
            } => self.process_s3(package, registration),
            Need::Native { package, component } => self.process_native(package, component),
            Need::Lifecycle { package, hook } => self.process_lifecycle(package, hook),
        }
    }

    fn process_binding(&mut self, id: PackageId, binding: String) -> Result<()> {
        eprintln!("[hrm] analyze {}::{binding}", id.name);
        let package = self.packages.locate(&id.name)?;
        let node = self.need_node(&Need::Binding {
            package: id.clone(),
            binding: binding.clone(),
        });
        if self.packages.is_target_provided(&package) {
            return Ok(());
        }
        let image = self.image(&package)?;
        let Some(binding_image) = image.binding(&binding).cloned() else {
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
        }
        if !binding_image.object_kind.needs_air() {
            return Ok(());
        }
        let Some(parsed) = self.parsed(&id, &binding, &binding_image)? else {
            return Ok(());
        };
        for expression in &parsed.expressions {
            for reference in &expression.references {
                let resolved = self.resolve_name(&package, &image, &reference.name)?;
                self.require_resolved(node, resolved, reference.span.clone())?;
            }
            for reference in &expression.package_refs {
                self.namespace_access(node, &package, reference)?;
            }
            for resource in &expression.resource_refs {
                self.resource_access(node, &package, resource)?;
            }
            for call in &expression.calls {
                self.semantic_call(node, &package, &image, call)?;
            }
            for effect in &expression.effects {
                let code = match effect.kind {
                    SyntaxEffectKind::SuperAssignment => RejectCode::EnvironmentMutation,
                    SyntaxEffectKind::IndirectPackageWrite
                    | SyntaxEffectKind::UnsupportedAssignmentTarget => {
                        RejectCode::UnsupportedTopLevelEffect
                    }
                };
                self.diagnostic(
                    node,
                    &id,
                    Some(&binding),
                    code,
                    format!("unsupported R effect: {:?}", effect.kind),
                    Some(effect.span.clone()),
                );
            }
        }
        Ok(())
    }

    fn parsed(
        &mut self,
        id: &PackageId,
        binding: &str,
        image: &BindingImage,
    ) -> Result<Option<ParsedRFile>> {
        let key = (id.clone(), binding.to_owned());
        if let Some(state) = self.parsed_bindings.get(&key) {
            return Ok(match state {
                ParseState::Parsed(parsed) => Some(parsed.clone()),
                ParseState::Blocked => None,
            });
        }
        let closure = image.closure.as_ref().ok_or_else(|| {
            Error::Analysis(format!(
                "closure binding {}::{binding} has no source",
                id.name
            ))
        })?;
        let source = self.sources.add_binding(
            id.name.clone(),
            binding.to_owned(),
            Arc::clone(&closure.source),
        );
        self.source_ids.insert(key.clone(), source.clone());
        match AirParser.parse_binding(source, closure.source.as_ref()) {
            Ok(parsed) => {
                self.parsed_bindings
                    .insert(key, ParseState::Parsed(parsed.clone()));
                Ok(Some(parsed))
            }
            Err(error) => {
                self.handle_air_rejection(id, binding, error)?;
                Ok(None)
            }
        }
    }

    fn handle_air_rejection(
        &mut self,
        id: &PackageId,
        binding: &str,
        air_error: String,
    ) -> Result<()> {
        let key = (id.clone(), binding.to_owned());
        let source_id = self.source_ids.get(&key).cloned().ok_or_else(|| {
            Error::Analysis(format!("missing virtual source for {}::{binding}", id.name))
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
            .validate_syntax(id, binding, source_text.as_ref())?;
        let node = self.need_node(&Need::Binding {
            package: id.clone(),
            binding: binding.to_owned(),
        });
        let span = Some(Span::new(source_id, 0, source_text.len()));
        match validation {
            SyntaxValidation::Accepted => self.diagnostic(
                node,
                id,
                Some(binding),
                RejectCode::AirUnsupportedSyntax,
                format!("target R accepts this binding; Air {air_error}; analysis of this binding is conservatively blocked"),
                span,
            ),
            SyntaxValidation::Rejected(r_error) => self.diagnostic(
                node,
                id,
                Some(binding),
                RejectCode::InvalidInstalledRepresentation,
                format!("Air rejects generated binding source ({air_error}); target R also rejects it ({r_error})"),
                span,
            ),
        }
        self.parsed_bindings.insert(key, ParseState::Blocked);
        Ok(())
    }

    fn process_activation(&mut self, id: PackageId) -> Result<()> {
        let package = self.packages.locate(&id.name)?;
        if self.packages.is_target_provided(&package) {
            return Ok(());
        }
        let image = self.image(&package)?;
        let node = self.need_node(&Need::Activation {
            package: id.clone(),
        });

        for dependency in image
            .index
            .description
            .depends()
            .map_err(|error| Error::Analysis(error.to_string()))?
        {
            let dependency_package = match self.packages.locate(&dependency.name) {
                Ok(package) => package,
                Err(_) => {
                    self.diagnostic(
                        node,
                        &id,
                        None,
                        RejectCode::MissingDependency,
                        format!("Depends package `{}` is not installed", dependency.name),
                        None,
                    );
                    continue;
                }
            };
            if !self.packages.is_target_provided(&dependency_package) {
                self.diagnostic(
                    node,
                    &id,
                    None,
                    RejectCode::DependsAttachmentUnsupported,
                    format!(
                        "internalized package has non-target-provided Depends: {}",
                        dependency.name
                    ),
                    None,
                );
            }
        }

        for import in &image.index.imports {
            let name = match import {
                ImportSpec::All { package, .. } | ImportSpec::From { package, .. } => package,
            };
            let foreign = self.packages.locate(name)?;
            self.require(
                node,
                Need::Activation {
                    package: foreign.id,
                },
                EdgeKind::Import,
                format!(
                    "activation of {} requires imported namespace {name}",
                    id.name
                ),
            );
        }
        for registration in &image.index.s3 {
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
                    "activation registers {}/{}",
                    registration.generic, registration.class
                ),
            );
        }
        for native in &image.index.dynlibs {
            self.require(
                node,
                Need::Native {
                    package: id.clone(),
                    component: native.name.clone(),
                },
                EdgeKind::Native,
                format!("effective useDynLib requires {}", native.name),
            );
        }
        if image.index.lifecycle.on_load {
            self.require(
                node,
                Need::Lifecycle {
                    package: id.clone(),
                    hook: ".onLoad".into(),
                },
                EdgeKind::Lifecycle,
                "namespace activation requires .onLoad",
            );
        }
        Ok(())
    }

    fn process_resource(&mut self, id: PackageId, resource: String) -> Result<()> {
        let package = self.packages.locate(&id.name)?;
        if self.packages.is_target_provided(&package) {
            return Ok(());
        }
        let image = self.image(&package)?;
        let node = self.need_node(&Need::Resource {
            package: id.clone(),
            resource: resource.clone(),
        });
        if !image
            .index
            .resources
            .iter()
            .any(|candidate| candidate.path == resource)
        {
            self.diagnostic(
                node,
                &id,
                None,
                RejectCode::UnresolvedBinding,
                format!("resource `{resource}` is absent from installed image"),
                None,
            );
        }
        Ok(())
    }

    fn process_dataset(&mut self, id: PackageId, dataset: String) -> Result<()> {
        let package = self.packages.locate(&id.name)?;
        if self.packages.is_target_provided(&package) {
            return Ok(());
        }
        let image = self.image(&package)?;
        let node = self.need_node(&Need::Dataset {
            package: id.clone(),
            dataset: dataset.clone(),
        });
        if !image.index.datasets.iter().any(|name| name == &dataset) {
            self.diagnostic(
                node,
                &id,
                None,
                RejectCode::UnresolvedBinding,
                format!("dataset `{dataset}` is absent from installed image"),
                None,
            );
        }
        Ok(())
    }

    fn process_s3(&mut self, id: PackageId, registration: S3Id) -> Result<()> {
        let node = self.need_node(&Need::S3Registration {
            package: id.clone(),
            registration: registration.clone(),
        });
        self.require(
            node,
            Need::Binding {
                package: id.clone(),
                binding: registration.method.clone(),
            },
            EdgeKind::S3Registration,
            format!(
                "S3 registration {}/{} requires method {}",
                registration.generic, registration.class, registration.method
            ),
        );
        if let Some((package_name, _generic)) = registration.generic.split_once("::") {
            if let Ok(generic_package) = self.packages.locate(package_name) {
                self.require(
                    node,
                    Need::Activation {
                        package: generic_package.id,
                    },
                    EdgeKind::S3Registration,
                    format!(
                        "S3 generic `{}` requires its namespace",
                        registration.generic
                    ),
                );
            }
        }
        Ok(())
    }

    fn process_native(&mut self, id: PackageId, component: String) -> Result<()> {
        let package = self.packages.locate(&id.name)?;
        if self.packages.is_target_provided(&package) {
            return Ok(());
        }
        let image = self.image(&package)?;
        let node = self.need_node(&Need::Native {
            package: id.clone(),
            component: component.clone(),
        });
        if let Some(native) = image
            .index
            .dynlibs
            .iter()
            .find(|native| native.name == component)
        {
            for callback in &native.callbacks {
                self.require(
                    node,
                    Need::Binding {
                        package: id.clone(),
                        binding: callback.clone(),
                    },
                    EdgeKind::Callback,
                    format!("native component `{component}` calls R binding `{callback}`"),
                );
            }
            if native.opaque_r_lookup {
                self.diagnostic(
                    node,
                    &id,
                    None,
                    RejectCode::UnknownNativeLookup,
                    format!("native component `{component}` can recover unknown R bindings"),
                    None,
                );
            }
        }
        Ok(())
    }

    fn process_lifecycle(&mut self, id: PackageId, hook: String) -> Result<()> {
        let node = self.need_node(&Need::Lifecycle {
            package: id.clone(),
            hook: hook.clone(),
        });
        self.require(
            node,
            Need::Binding {
                package: id,
                binding: hook.clone(),
            },
            EdgeKind::Lifecycle,
            format!("lifecycle hook `{hook}` must be retained"),
        );
        Ok(())
    }

    fn namespace_access(
        &mut self,
        from: NodeId,
        current: &InstalledPackage,
        reference: &crate::syntax::PackageRef,
    ) -> Result<()> {
        let foreign = self.packages.locate(&reference.package)?;
        eprintln!("[hrm] require {}::{}", reference.package, reference.symbol);
        if self.packages.is_target_provided(&foreign) {
            let external = self.graph.add_node(
                &foreign.id.name,
                NodeKind::ExternalBinding {
                    name: reference.symbol.clone(),
                },
                Some(reference.span.clone()),
            );
            self.graph.add_edge(
                from,
                external,
                EdgeKind::PackageQualified,
                format!(
                    "{} access to target-provided binding",
                    if reference.internal { ":::" } else { "::" }
                ),
            );
            return Ok(());
        }
        let image = self.image(&foreign)?;
        let binding = if reference.internal {
            reference.symbol.clone()
        } else {
            image
                .index
                .exports
                .get(&reference.symbol)
                .cloned()
                .unwrap_or_else(|| reference.symbol.clone())
        };
        self.require(
            from,
            Need::Activation {
                package: foreign.id.clone(),
            },
            EdgeKind::NamespaceLoad,
            "qualified namespace access requires activation",
        );
        self.require(
            from,
            Need::Binding {
                package: foreign.id.clone(),
                binding: binding.clone(),
            },
            EdgeKind::PackageQualified,
            format!(
                "{}::{} resolves to `{binding}`",
                reference.package, reference.symbol
            ),
        );
        self.rewrites.push(Rewrite::NamespaceAccess {
            source: reference.span.clone(),
            package: foreign.id,
            binding,
            internal: reference.internal,
        });
        let _ = current;
        Ok(())
    }

    fn resource_access(
        &mut self,
        from: NodeId,
        current: &InstalledPackage,
        resource: &crate::syntax::ResourceRef,
    ) -> Result<()> {
        let Some(package_name) = &resource.package else {
            if resource.path.is_none() {
                self.diagnostic(
                    from,
                    &current.id,
                    None,
                    RejectCode::DynamicLookup,
                    "dynamic system.file() resource path",
                    Some(resource.span.clone()),
                );
            }
            return Ok(());
        };
        let foreign = self.packages.locate(package_name)?;
        if self.packages.is_target_provided(&foreign) {
            return Ok(());
        }
        let Some(path) = &resource.path else {
            self.diagnostic(
                from,
                &current.id,
                None,
                RejectCode::DynamicLookup,
                format!("dynamic system.file() path for package {package_name}"),
                Some(resource.span.clone()),
            );
            return Ok(());
        };
        self.require(
            from,
            Need::Resource {
                package: foreign.id.clone(),
                resource: path.clone(),
            },
            EdgeKind::Resource,
            format!("system.file requires {package_name}/{path}"),
        );
        self.rewrites.push(Rewrite::ResourceAccess {
            source: resource.span.clone(),
            package: foreign.id,
            resource: path.clone(),
        });
        Ok(())
    }

    fn semantic_call(
        &mut self,
        from: NodeId,
        current: &InstalledPackage,
        image: &PackageImage,
        call: &CallSite,
    ) -> Result<()> {
        if call.callee_local {
            return Ok(());
        }
        let resolved = self.resolve_name(current, image, &call.callee)?;
        if !matches!(resolved, ResolvedName::Base(_)) {
            return Ok(());
        }
        match call.callee.as_str() {
            "library" | "require" => {
                let package = static_package_arg(call);
                self.diagnostic(
                    from,
                    &current.id,
                    None,
                    RejectCode::PackageAttachmentUnsupported,
                    match package {
                        Some(name) => format!(
                            "search-path attachment of `{name}` is outside the current contract"
                        ),
                        None => "dynamic package attachment is outside the current contract".into(),
                    },
                    Some(call.span.clone()),
                );
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
                match self.policy.namespace_discovery {
                    DiscoveryPolicy::Reject => self.diagnostic(
                        from, &current.id, None, RejectCode::DynamicPackageDiscovery,
                        format!("reachable namespace discovery for `{name}` is not specialized by policy"), Some(call.span.clone())),
                    DiscoveryPolicy::TargetProvidedOnly => {
                        match self.packages.locate(name) {
                            Ok(foreign) if self.packages.is_target_provided(&foreign) => {},
                            Ok(_) => self.diagnostic(from, &current.id, None, RejectCode::DynamicPackageDiscovery,
                                format!("`{name}` is installed but is not an exact target-provided namespace"), Some(call.span.clone())),
                            Err(_) => self.diagnostic(from, &current.id, None, RejectCode::MissingDependency,
                                format!("namespace discovery references unavailable `{name}`"), Some(call.span.clone())),
                        }
                    }
                    DiscoveryPolicy::Internalize => {
                        let foreign = self.packages.locate(name)?;
                        if !self.packages.is_target_provided(&foreign) {
                            self.require(from, Need::Activation { package: foreign.id.clone() }, EdgeKind::Discovery,
                                format!("specialized namespace discovery requires `{name}`"));
                            self.rewrites.push(Rewrite::SpecializedDiscovery { source: call.span.clone(), package: foreign.id, result: true });
                        }
                    }
                }
            }
            "packageVersion" => self.identity_query(from, current, call, true)?,
            "find.package" => self.identity_query(from, current, call, false)?,
            ".Call" | ".External" | ".C" | ".Fortran" => {
                if image.index.dynlibs.is_empty() {
                    self.diagnostic(
                        from,
                        &current.id,
                        None,
                        RejectCode::UnknownNativeLookup,
                        format!("{} has no statically associated package DLL", call.callee),
                        Some(call.span.clone()),
                    );
                } else {
                    for native in &image.index.dynlibs {
                        self.require(
                            from,
                            Need::Native {
                                package: current.id.clone(),
                                component: native.name.clone(),
                            },
                            EdgeKind::Native,
                            format!(
                                "reachable {} requires package native component {}",
                                call.callee, native.name
                            ),
                        );
                    }
                }
            }
            "deparse" | "substitute" | "match.call" => self.diagnostic(
                from,
                &current.id,
                None,
                RejectCode::SyntaxObservation,
                format!("reachable syntax observation through {}", call.callee),
                Some(call.span.clone()),
            ),
            _ => {}
        }
        Ok(())
    }

    fn identity_query(
        &mut self,
        from: NodeId,
        current: &InstalledPackage,
        call: &CallSite,
        _version: bool,
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
        match self.policy.namespace_discovery {
            DiscoveryPolicy::Reject => self.diagnostic(
                from,
                &current.id,
                None,
                RejectCode::DynamicPackageDiscovery,
                format!(
                    "reachable package identity query for `{name}` is not specialized by policy"
                ),
                Some(call.span.clone()),
            ),
            DiscoveryPolicy::TargetProvidedOnly => match self.packages.locate(name) {
                Ok(foreign) if self.packages.is_target_provided(&foreign) => {}
                Ok(_) => self.diagnostic(
                    from,
                    &current.id,
                    None,
                    RejectCode::DynamicPackageDiscovery,
                    format!("package identity query for `{name}` is not target-provided"),
                    Some(call.span.clone()),
                ),
                Err(_) => self.diagnostic(
                    from,
                    &current.id,
                    None,
                    RejectCode::MissingDependency,
                    format!("package identity query references unavailable `{name}`"),
                    Some(call.span.clone()),
                ),
            },
            DiscoveryPolicy::Internalize => {
                let foreign = self.packages.locate(name)?;
                if !self.packages.is_target_provided(&foreign) {
                    self.rewrites.push(Rewrite::SpecializedDiscovery {
                        source: call.span.clone(),
                        package: foreign.id,
                        result: true,
                    });
                }
            }
        }
        Ok(())
    }

    fn resolve_name(
        &mut self,
        current: &InstalledPackage,
        image: &PackageImage,
        name: &str,
    ) -> Result<ResolvedName> {
        if image.bindings.contains_key(name) {
            return Ok(ResolvedName::PackageBinding {
                package: current.id.clone(),
                binding: name.to_owned(),
            });
        }
        if let Some((package_name, binding)) = image.index.import_from(name) {
            let foreign = self.packages.locate(package_name)?;
            return Ok(if self.packages.is_target_provided(&foreign) {
                ResolvedName::TargetProvided {
                    package: foreign.id,
                    binding: binding.to_owned(),
                }
            } else {
                ResolvedName::Imported {
                    package: foreign.id,
                    binding: binding.to_owned(),
                }
            });
        }
        for import in &image.index.imports {
            let ImportSpec::All {
                package: package_name,
                except,
            } = import
            else {
                continue;
            };
            if except.iter().any(|excluded| excluded == name) {
                continue;
            }
            let foreign = self.packages.locate(package_name)?;
            if self.packages.is_target_provided(&foreign) {
                // Installed nsInfo establishes the import relationship. For an
                // external real namespace, exact target identity terminates
                // internal traversal; ordinary lookup remains runtime R lookup.
                return Ok(ResolvedName::TargetProvided {
                    package: foreign.id,
                    binding: name.to_owned(),
                });
            }
            let foreign_image = self.image(&foreign)?;
            if let Some(binding) = foreign_image.index.exports.get(name) {
                return Ok(ResolvedName::Imported {
                    package: foreign.id,
                    binding: binding.clone(),
                });
            }
        }
        Ok(ResolvedName::Base(name.to_owned()))
    }

    fn require_resolved(&mut self, from: NodeId, resolved: ResolvedName, span: Span) -> Result<()> {
        match resolved {
            ResolvedName::PackageBinding { package, binding } => self.require(
                from,
                Need::Binding {
                    package,
                    binding: binding.clone(),
                },
                EdgeKind::Lexical,
                format!("lexical reference `{binding}`"),
            ),
            ResolvedName::Imported { package, binding } => {
                self.require(
                    from,
                    Need::Activation {
                        package: package.clone(),
                    },
                    EdgeKind::Import,
                    "imported binding requires namespace activation",
                );
                self.require(
                    from,
                    Need::Binding {
                        package,
                        binding: binding.clone(),
                    },
                    EdgeKind::Import,
                    format!("imported binding `{binding}`"),
                );
            }
            ResolvedName::TargetProvided { package, binding } => {
                let node = self.graph.add_node(
                    package.name,
                    NodeKind::ExternalBinding {
                        name: binding.clone(),
                    },
                    Some(span),
                );
                self.graph.add_edge(
                    from,
                    node,
                    EdgeKind::Import,
                    format!("target-provided imported binding `{binding}`"),
                );
            }
            ResolvedName::Local(_) | ResolvedName::Base(_) => {}
            ResolvedName::Unknown(name) => {
                let package = self.graph.nodes[from.0].package.clone();
                let fake = PackageId {
                    name: package.clone(),
                    version: String::new(),
                    library: Default::default(),
                    root: Default::default(),
                    image_fingerprint: crate::package::Digest(String::new()),
                };
                self.diagnostic(
                    from,
                    &fake,
                    None,
                    RejectCode::UnresolvedBinding,
                    format!("unresolved name `{name}`"),
                    Some(span),
                );
            }
        }
        Ok(())
    }

    fn require_root(&mut self, need: Need) {
        let node = self.need_node(&need);
        if !self.roots.contains(&node) {
            self.roots.push(node);
        }
        if !self.processed.contains(&need) {
            self.pending.push_back(need);
        }
    }

    fn require(&mut self, from: NodeId, need: Need, kind: EdgeKind, reason: impl Into<String>) {
        let to = self.need_node(&need);
        self.graph.add_edge(from, to, kind, reason);
        if !self.processed.contains(&need) {
            self.pending.push_back(need);
        }
    }

    fn need_node(&mut self, need: &Need) -> NodeId {
        let package = &need.package().name;
        let kind = match need {
            Need::Binding { binding, .. } => NodeKind::Binding {
                name: binding.clone(),
            },
            Need::Activation { .. } => NodeKind::Activation,
            Need::Resource { resource, .. } => NodeKind::Resource {
                path: resource.clone(),
            },
            Need::Dataset { dataset, .. } => NodeKind::Dataset {
                name: dataset.clone(),
            },
            Need::S3Registration { registration, .. } => NodeKind::S3Registration {
                generic: registration.generic.clone(),
                class: registration.class.clone(),
            },
            Need::Native { component, .. } => NodeKind::NativeComponent {
                name: component.clone(),
            },
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
        self.diagnostics.push(Diagnostic {
            package: package.name.clone(),
            binding: binding.map(str::to_owned),
            code,
            message: message.into(),
            span,
            node: Some(node),
            reachable: true,
        });
    }
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
