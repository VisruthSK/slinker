use super::relocation::PendingRelocation;
use super::state::AnalyzerState;
use crate::analysis::Need;
use crate::analysis::{Diagnostic, NodeKind, RejectCode};
use crate::ir::{
    BindingId, BindingName, ClosureHome, CodeId, ExportTable, ExternalBindingAccess,
    ExternalPackageContract, FinalizedNamespace, InvalidRelocation, MaterializedRole,
    MaterializedSlot, MaterializedSlotSource, NamespaceId, ObjectStep,
    PackageRole as LinkedPackageRole, ProgramBuilder, ProgramIr, RelocationTarget, RootArtifactIr,
    TargetContract, UnretainedName,
};
use crate::metadata::{Relation, RelationField, intersect_requirements, relations};
use crate::package::{
    ImportSpec, NativeComponent, PackageAvailability, PackageId, PackageProvider,
};
use crate::source::generated_description;
use crate::syntax::{SourceKey, SourceOrigin, Sources};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::sync::Arc;

#[derive(Debug)]
pub struct LinkIr {
    packages: crate::package::PackageSources,
    program: ProgramIr,
    provenance: crate::ir::ProvenanceIr,
    blockers: Vec<Diagnostic>,
    assumptions: Vec<Diagnostic>,
    sources: Sources,
    construction_evaluations: usize,
}

impl<P: PackageProvider> AnalyzerState<P> {
    pub(super) fn finalize(mut self) -> LinkIr {
        self.finalize_syntax_observations();
        let root = self.root.expect("root package established before analysis");
        let retained = self
            .encountered
            .union(&self.external)
            .copied()
            .collect::<BTreeSet<_>>();
        let (program, issues) = self.finalize_program(&retained);
        let node = self.need_node(&Need::Activation { package: root });
        for issue in issues {
            self.diagnostic(node, root, None, issue.code(), issue.to_string(), None);
        }
        if retained
            .iter()
            .any(|package| self.packages.role(*package) == LinkedPackageRole::Linked)
        {
            for (node, package, span) in self.relocations.take_dynamic_resource_lookups() {
                self.assume(
                    node,
                    package,
                    None,
                    RejectCode::DynamicLookup,
                    "dynamic system.file() package can name a Linked package whose installation is removed",
                    Some(span),
                );
            }
        }
        self.finalize_s3_dispatch(&retained);
        let (blockers, assumptions) = self.diagnostics.into_sorted();
        LinkIr {
            program,
            provenance: crate::ir::ProvenanceIr::from_analysis(self.graph, self.roots),
            blockers,
            assumptions,
            sources: self.parses.into_sources(),
            packages: self.packages.sources(retained),
            construction_evaluations: self.construction_evaluations,
        }
    }

    pub(super) fn finalize_program(
        &self,
        retained: &BTreeSet<PackageId>,
    ) -> (ProgramIr, Vec<FinalizationIssue>) {
        let target = &self.packages.target_environment().target;
        let root = self.root.expect("root package established before analysis");
        let mut builder = ProgramIr::builder(
            TargetContract {
                r_version: target.r_version.clone(),
                platform: target.os.clone(),
                arch: target.arch.clone(),
            },
            root,
            self.packages.identity(root).clone(),
        );
        let ordered = retained
            .iter()
            .map(|package| (*package, self.packages.role(*package)))
            .collect::<Vec<_>>();
        let mut issues = Vec::new();
        let contracts = self.finalize_packages(&mut builder, &ordered, retained, &mut issues);
        let mut namespaces = self.finalize_namespaces(&mut builder, ordered, retained, &mut issues);
        let mut dependencies = self.attach_imports(&mut builder, retained, &namespaces.ids);
        let (root_exports, mut linked_contents) =
            self.export_contents(&builder, retained, &namespaces.ids);
        for (owner, dependency) in self.activation_time_dependencies(&namespaces.ids) {
            if owner != dependency {
                dependencies.entry(owner).or_default().insert(dependency);
            }
        }
        for namespace in activation_order(&namespaces.linked, &dependencies, &mut issues) {
            let (exports, unretained) = linked_contents
                .remove(&namespace)
                .expect("every Linked namespace has activation contents");
            builder.add_activation(crate::ir::NamespaceActivationIr {
                namespace,
                on_load: namespaces.on_load.get(&namespace).copied(),
                native_components: namespaces
                    .linked_natives
                    .remove(&namespace)
                    .unwrap_or_default(),
                exports,
                unretained,
            });
        }
        let root_namespace = namespaces.ids[self.packages.name(root)].namespace;
        let root_on_load = namespaces
            .on_load
            .get(&root_namespace)
            .and_then(|&binding| {
                let closure = builder.binding_closure(binding);
                if closure.is_none() {
                    issues.push(FinalizationIssue::RootOnLoadNotRelocatable);
                }
                closure
            });
        self.plan_relocations(&mut builder, &namespaces.ids, &mut issues);
        let description = self.root_description(&contracts, retained, &mut issues);
        builder.set_root_artifact(RootArtifactIr {
            description,
            exports: root_exports,
            native_components: namespaces.root_natives,
            on_load: root_on_load,
        });
        (builder.finish(), issues)
    }

    fn finalize_packages(
        &self,
        builder: &mut ProgramBuilder,
        ordered: &[(PackageId, LinkedPackageRole)],
        retained: &BTreeSet<PackageId>,
        issues: &mut Vec<FinalizationIssue>,
    ) -> Vec<ExternalPackageContract> {
        let mut contracts = Vec::new();
        let mut declared = self.declared_external_requirements(ordered);
        for (package, role) in ordered {
            let identity = self.packages.identity(*package).clone();
            match role {
                LinkedPackageRole::Root => {}
                LinkedPackageRole::Linked => builder.add_linked_package(*package, identity),
                LinkedPackageRole::External => {
                    let contract = self.external_contract(
                        *package,
                        &declared.remove(identity.name.as_str()).unwrap_or_default(),
                        issues,
                    );
                    contracts.push(contract.clone());
                    builder.add_external_package(*package, identity, contract);
                }
            }
        }
        contracts.sort_by(|left, right| left.package.cmp(&right.package));
        let mut unreached = self
            .explicit_external_packages
            .iter()
            .filter(|name| {
                self.packages
                    .availability(name)
                    .and_then(PackageAvailability::package)
                    .is_none_or(|package| !retained.contains(&package))
            })
            .collect::<Vec<_>>();
        unreached.sort();
        issues.extend(
            unreached
                .into_iter()
                .map(|name| FinalizationIssue::UnreachedExternal(name.clone())),
        );
        contracts
    }

    fn finalize_namespaces(
        &self,
        builder: &mut ProgramBuilder,
        ordered: Vec<(PackageId, LinkedPackageRole)>,
        retained: &BTreeSet<PackageId>,
        issues: &mut Vec<FinalizationIssue>,
    ) -> FinalizedNamespaces {
        let mut retained_bindings = HashMap::<PackageId, BTreeSet<BindingName>>::new();
        for need in self.needs.started() {
            if let Need::Binding { package, binding } = need {
                retained_bindings
                    .entry(*package)
                    .or_default()
                    .insert(binding.clone());
            }
        }
        let mut namespaces = FinalizedNamespaces::default();
        for (package, role) in ordered {
            let package_name = self.packages.name(package);
            let materialized = match role {
                LinkedPackageRole::Root => MaterializedRole::Root,
                LinkedPackageRole::Linked => MaterializedRole::Linked,
                LinkedPackageRole::External => {
                    let bindings = self
                        .external_bindings
                        .iter()
                        .filter(|((owner, _), _)| *owner == package)
                        .map(|((_, name), access)| (name.clone(), *access));
                    let namespace = builder.finish_external_namespace(package, bindings);
                    namespaces.ids.insert(package_name.to_owned(), namespace);
                    continue;
                }
            };
            let image = self
                .images
                .get(&package)
                .expect("Root/Linked package has an initialized image");
            let namespace_builder = self
                .namespace_builders
                .get(&package)
                .expect("Root/Linked namespace builder");
            let namespace_label = format!("namespace:{package_name}");
            let mut names = retained_bindings.remove(&package).unwrap_or_default();
            names.extend(
                namespace_builder
                    .registrations
                    .iter()
                    .map(|registration| registration.method.clone()),
            );
            let slots = names.iter().map(|name| {
                let source = match image.binding(name) {
                    None => MaterializedSlotSource::Unbound,
                    Some(binding) => {
                        match (
                            &binding.closure,
                            self.parses
                                .shape(&(package, SourceKey::Binding(name.to_string()))),
                        ) {
                            (Some(closure), Some(normalized_shape))
                                if closure.environment == namespace_label =>
                            {
                                MaterializedSlotSource::Closure {
                                    source: Arc::clone(&closure.source),
                                    normalized_shape: normalized_shape.clone(),
                                    binding: name.clone(),
                                }
                            }
                            _ => MaterializedSlotSource::Payload {
                                binding: name.clone(),
                            },
                        }
                    }
                };
                MaterializedSlot {
                    name: name.clone(),
                    source,
                }
            });
            let namespace = builder.finish_materialized_namespace(package, materialized, slots);
            if image.index.lifecycle.on_load {
                match namespace.bindings.get(".onLoad") {
                    Some(&binding) => {
                        namespaces.on_load.insert(namespace.namespace, binding);
                    }
                    None => issues.push(FinalizationIssue::OnLoadNotRetained(
                        package_name.to_owned(),
                    )),
                }
            }
            for registration in &namespace_builder.registrations {
                let Some(&method) = namespace.bindings.get(registration.method.as_str()) else {
                    continue;
                };
                builder.attach_s3_registration(
                    namespace.namespace,
                    crate::ir::GenericId {
                        package: registration
                            .generic
                            .package
                            .filter(|package| retained.contains(package)),
                        name: registration.generic.name.clone(),
                    },
                    registration.class.clone(),
                    method,
                );
            }
            match materialized {
                MaterializedRole::Root => namespaces.root_natives.clone_from(&image.index.dynlibs),
                MaterializedRole::Linked => {
                    namespaces
                        .linked_natives
                        .insert(namespace.namespace, image.index.dynlibs.clone());
                }
            }
            for native in &image.index.dynlibs {
                if let (LinkedPackageRole::Linked, Some(library)) = (role, &native.library) {
                    builder.add_resource(crate::ir::ResourceIr {
                        package,
                        path: library.clone(),
                    });
                }
            }
            if role == LinkedPackageRole::Linked {
                namespaces.linked.push(namespace.namespace);
            }
            namespaces.ids.insert(package_name.to_owned(), namespace);
        }
        namespaces
    }

    fn attach_imports(
        &self,
        builder: &mut ProgramBuilder,
        retained: &BTreeSet<PackageId>,
        namespace_ids: &HashMap<String, FinalizedNamespace>,
    ) -> HashMap<NamespaceId, BTreeSet<NamespaceId>> {
        let mut namespace_dependencies = HashMap::<NamespaceId, BTreeSet<NamespaceId>>::new();
        for &package in retained {
            if self.packages.is_external(package) {
                continue;
            }
            let Some(image) = self.images.get(&package) else {
                continue;
            };
            let owner = namespace_ids[self.packages.name(package)].namespace;
            for import in &image.index.imports {
                let (target_name, pairs) = match import {
                    ImportSpec::From {
                        package: target,
                        bindings,
                    } => (
                        target,
                        bindings
                            .iter()
                            .map(|binding| (binding.local.clone(), binding.remote.clone()))
                            .collect::<Vec<_>>(),
                    ),
                    ImportSpec::All {
                        package: target,
                        except,
                    } => {
                        let exported = self
                            .packages
                            .availability(target)
                            .and_then(PackageAvailability::package)
                            .filter(|package| retained.contains(package))
                            .and_then(|package| self.images.get(&package))
                            .map_or_else(
                                || {
                                    self.external_bindings
                                        .keys()
                                        .filter(|(owner, _)| {
                                            self.packages.name(*owner) == target.as_str()
                                        })
                                        .map(|(_, name)| name.to_string())
                                        .collect()
                                },
                                |image| {
                                    image
                                        .index
                                        .exports
                                        .values()
                                        .map(ToString::to_string)
                                        .collect::<Vec<_>>()
                                },
                            );
                        (
                            target,
                            exported
                                .into_iter()
                                .filter(|name| !except.iter().any(|excluded| excluded == name))
                                .map(|name| {
                                    (BindingName::from(name.as_str()), BindingName::from(name))
                                })
                                .collect(),
                        )
                    }
                };
                let Some(target) = namespace_ids.get(target_name.as_str()) else {
                    continue;
                };
                for (local, remote) in pairs {
                    if let Some(&binding) = target.bindings.get(remote.as_str()) {
                        builder.attach_import(owner, local, binding);
                        namespace_dependencies
                            .entry(owner)
                            .or_default()
                            .insert(builder.binding_namespace(binding));
                    }
                }
            }
        }
        namespace_dependencies
    }

    fn export_contents(
        &self,
        builder: &ProgramBuilder,
        retained: &BTreeSet<PackageId>,
        namespace_ids: &HashMap<String, FinalizedNamespace>,
    ) -> (ExportTable, HashMap<NamespaceId, LinkedActivationContents>) {
        let mut root_exports = ExportTable::default();
        let mut linked_contents = HashMap::new();
        for &package in retained {
            if self.packages.is_external(package) {
                continue;
            }
            let namespace = namespace_ids[self.packages.name(package)].namespace;
            let exports = self.images[&package]
                .index
                .exports
                .values()
                .filter_map(|name| builder.visible_binding(namespace, name))
                .collect();
            if self.packages.role(package) == LinkedPackageRole::Linked {
                let index = &self.images[&package].index;
                let kept = &namespace_ids[self.packages.name(package)].bindings;
                let mut unretained = index
                    .binding_names
                    .iter()
                    .filter(|name| !kept.contains_key(name.as_str()))
                    .map(|name| (name.to_string(), UnretainedName::Stub))
                    .collect::<BTreeMap<_, _>>();
                for name in index
                    .exports
                    .values()
                    .filter(|name| !kept.contains_key(name.as_str()))
                {
                    unretained
                        .entry(name.to_string())
                        .and_modify(|state| *state = UnretainedName::ExportedStub)
                        .or_insert(UnretainedName::ExportedByActivation);
                }
                linked_contents.insert(namespace, (ExportTable::new(exports), unretained));
            } else {
                root_exports = ExportTable::new(exports);
            }
        }
        (root_exports, linked_contents)
    }

    fn plan_relocations(
        &self,
        builder: &mut ProgramBuilder,
        namespace_ids: &HashMap<String, FinalizedNamespace>,
        issues: &mut Vec<FinalizationIssue>,
    ) {
        let emitted_code = |builder: &ProgramBuilder, origin: &SourceOrigin| {
            origin
                .key
                .namespace_binding()
                .and_then(|binding| namespace_ids[origin.package.as_str()].bindings.get(binding))
                .and_then(|binding| builder.binding_code(*binding))
        };
        let mut payload_codes = HashMap::<&SourceOrigin, Option<CodeId>>::new();
        for relocation in self.relocations.relocations() {
            let origin = self.parses.sources().origin(&relocation.source().source);
            let required = relocation.reaches_removed_installation()
                || relocation.named_namespace().is_some_and(|package| {
                    self.packages.role(package) == LinkedPackageRole::Linked
                });
            if !required
                || payload_codes.contains_key(origin)
                || emitted_code(builder, origin).is_some()
            {
                continue;
            }
            let code = self.payload_closure(builder, namespace_ids, origin);
            if code.is_none() {
                issues.push(FinalizationIssue::NonRelocatableCode {
                    package: origin.package.clone(),
                    binding: origin.key.to_string(),
                });
            }
            payload_codes.insert(origin, code);
        }
        for relocation in self.relocations.relocations() {
            let source = relocation.source();
            let origin = self.parses.sources().origin(&source.source);
            let Some(code) = emitted_code(builder, origin)
                .or_else(|| payload_codes.get(origin).copied().flatten())
            else {
                continue;
            };
            let target = match relocation {
                PendingRelocation::NamespaceAccess {
                    package,
                    binding,
                    internal,
                    ..
                } => {
                    let Some(target_namespace) = namespace_ids.get(self.packages.name(*package))
                    else {
                        continue;
                    };
                    let Some(&target) = target_namespace.bindings.get(binding.as_str()) else {
                        continue;
                    };
                    RelocationTarget::Binding {
                        target,
                        access: if *internal {
                            ExternalBindingAccess::Internal
                        } else {
                            ExternalBindingAccess::Exported
                        },
                    }
                }
                PendingRelocation::ResourceAccess {
                    package, resource, ..
                } => {
                    let resource_id = builder.add_resource(crate::ir::ResourceIr {
                        package: *package,
                        path: resource.clone(),
                    });
                    RelocationTarget::Resource {
                        target: resource_id,
                    }
                }
                PendingRelocation::RequireNamespace { loaded, .. } => {
                    RelocationTarget::RequireNamespace {
                        result: loaded.is_some(),
                    }
                }
                PendingRelocation::NamespaceLoad {
                    package, operation, ..
                } => RelocationTarget::Namespace {
                    package: *package,
                    operation: *operation,
                },
                PendingRelocation::PackageVersion { version, .. } => {
                    RelocationTarget::PackageVersion {
                        version: version.clone(),
                    }
                }
                PendingRelocation::LoadedQuery { .. } => RelocationTarget::LoadedQuery,
                PendingRelocation::NamespaceArgument { package, .. } => {
                    RelocationTarget::NamespaceArgument { package: *package }
                }
                PendingRelocation::DescriptionArgument { package, .. } => {
                    RelocationTarget::DescriptionArgument {
                        description: builder.add_resource(crate::ir::ResourceIr {
                            package: *package,
                            path: "Meta/package.rds".into(),
                        }),
                    }
                }
            };
            if let Err(invalid) = builder.relocate(code, source.range(), target) {
                issues.push(FinalizationIssue::InvalidRelocation(invalid));
            }
        }
    }

    fn payload_closure(
        &self,
        builder: &mut ProgramBuilder,
        namespace_ids: &HashMap<String, FinalizedNamespace>,
        origin: &SourceOrigin,
    ) -> Option<CodeId> {
        let package = self.known_package(&origin.package)?;
        let image = self.images.get(&package)?;
        let shape = self.parses.shape(&(package, origin.key.clone()))?.clone();
        let (home, binding, closure) = match &origin.key {
            SourceKey::Binding(name) => {
                let binding = *namespace_ids[origin.package.as_str()]
                    .bindings
                    .get(name.as_str())?;
                if !builder.binding_is_payload(binding) {
                    return None;
                }
                (
                    ClosureHome::Namespace,
                    BindingName::from(name.as_str()),
                    image.binding(name)?.closure.as_ref()?,
                )
            }
            SourceKey::Private {
                environment,
                binding,
            } => {
                let (root, steps) = self
                    .payload_environment_paths(
                        builder,
                        &namespace_ids[origin.package.as_str()],
                        image,
                    )
                    .remove(environment)?;
                (
                    ClosureHome::Reached { root, steps },
                    BindingName::from(binding.as_str()),
                    image
                        .private_binding(environment, binding)?
                        .closure
                        .as_ref()?,
                )
            }
            SourceKey::Closure { .. } | SourceKey::Runtime => return None,
        };
        Some(builder.add_payload_closure(
            package,
            home,
            binding,
            crate::ir::CodeIr::new(Arc::clone(&closure.source), shape),
        ))
    }

    fn payload_environment_paths(
        &self,
        builder: &ProgramBuilder,
        namespace: &FinalizedNamespace,
        image: &crate::package::PackageImage,
    ) -> HashMap<String, (BindingName, Vec<ObjectStep>)> {
        let mut queue = VecDeque::new();
        for (name, &binding) in &namespace.bindings {
            if builder.binding_is_payload(binding)
                && let Some(environment) = image
                    .binding(name)
                    .and_then(|binding| binding.environment.as_ref())
            {
                queue.push_back((
                    environment.clone(),
                    name.clone(),
                    vec![ObjectStep::Environment],
                ));
            }
        }
        let mut paths = HashMap::new();
        while let Some((environment, root, steps)) = queue.pop_front() {
            let Some(private) = image.private_environment(&environment) else {
                continue;
            };
            if paths.contains_key(&environment) {
                continue;
            }
            let mut parent = steps.clone();
            parent.push(ObjectStep::Parent);
            queue.push_back((private.parent.clone(), root.clone(), parent));
            let mut bindings = private.bindings.iter().collect::<Vec<_>>();
            bindings.sort_by(|left, right| left.0.cmp(right.0));
            for (name, binding) in bindings {
                if let Some(reached) = &binding.environment {
                    let mut through = steps.clone();
                    through.extend([ObjectStep::Binding(name.clone()), ObjectStep::Environment]);
                    queue.push_back((reached.clone(), root.clone(), through));
                }
            }
            paths.insert(environment, (root, steps));
        }
        paths
    }

    fn root_description(
        &self,
        contracts: &[ExternalPackageContract],
        retained: &BTreeSet<PackageId>,
        issues: &mut Vec<FinalizationIssue>,
    ) -> Option<Arc<str>> {
        let imports = contracts
            .iter()
            .flat_map(|contract| contract.requirements.iter().cloned())
            .collect::<Vec<_>>();
        let source = self.root_description.as_ref()?;
        generated_description(
            source,
            |name| {
                self.packages
                    .availability(name)
                    .and_then(PackageAvailability::package)
                    .is_some_and(|package| {
                        retained.contains(&package)
                            && self.packages.role(package) == LinkedPackageRole::Linked
                    })
            },
            &imports,
        )
        .map_or_else(
            |problems| {
                issues.extend(problems.into_iter().map(FinalizationIssue::Description));
                None
            },
            |description| Some(Arc::from(description)),
        )
    }

    fn declared_external_requirements(
        &self,
        ordered: &[(PackageId, LinkedPackageRole)],
    ) -> HashMap<&str, Vec<Relation>> {
        let mut declared = HashMap::<&str, Vec<Relation>>::new();
        for (package, role) in ordered {
            if *role == LinkedPackageRole::External {
                declared.entry(self.packages.name(*package)).or_default();
            }
        }
        for (package, role) in ordered {
            if *role == LinkedPackageRole::External {
                continue;
            }
            let description = &self.images[package].index.description;
            let suggested = relations(description, RelationField::Suggests)
                .unwrap_or_default()
                .into_iter()
                .filter(|relation| self.explicit_external_packages.contains(relation.package()));
            for relation in relations(description, RelationField::Imports)
                .unwrap_or_default()
                .into_iter()
                .chain(suggested)
            {
                if let Some(requirements) = declared.get_mut(relation.package()) {
                    requirements.push(relation);
                }
            }
        }
        declared
    }

    fn external_contract(
        &self,
        package: PackageId,
        declared: &[Relation],
        issues: &mut Vec<FinalizationIssue>,
    ) -> ExternalPackageContract {
        let identity = self.packages.identity(package);
        let platform = self.packages.is_platform(package);
        let requirements = if declared.is_empty() {
            Vec::new()
        } else {
            intersect_requirements(&identity.name, declared).unwrap_or_else(|problem| {
                issues.push(FinalizationIssue::IncompatibleRequirements(problem));
                Vec::new()
            })
        };
        if !platform && declared.is_empty() {
            issues.push(FinalizationIssue::UndeclaredExternal(
                identity.name.to_string(),
            ));
        }
        if let Some(unmet) = requirements
            .iter()
            .find(|relation| !relation.requirement().matches(&identity.version))
        {
            issues.push(FinalizationIssue::UnsatisfiedRequirement {
                identity: identity.clone(),
                requirement: unmet.clone(),
            });
        }
        ExternalPackageContract {
            package: identity.name.clone(),
            platform,
            requirements,
        }
    }

    fn activation_time_dependencies(
        &self,
        namespace_ids: &HashMap<String, crate::ir::FinalizedNamespace>,
    ) -> Vec<(crate::ir::NamespaceId, crate::ir::NamespaceId)> {
        let mut dependencies = Vec::new();
        for start in self.graph.nodes.iter().filter(|node| {
            matches!(
                node.kind,
                NodeKind::Lifecycle { .. } | NodeKind::S3Registration { .. }
            )
        }) {
            let Some(owner) = namespace_ids.get(&start.package) else {
                continue;
            };
            let mut seen = HashSet::new();
            let mut stack = vec![start.id];
            while let Some(node) = stack.pop() {
                if !seen.insert(node) {
                    continue;
                }
                for &next in self.dependencies.get(&node).into_iter().flatten() {
                    let target = &self.graph.nodes[next.0];
                    if target.package == start.package {
                        stack.push(next);
                    } else if let Some(dependency) = namespace_ids.get(&target.package) {
                        dependencies.push((owner.namespace, dependency.namespace));
                    }
                }
            }
        }
        dependencies
    }

    fn finalize_s3_dispatch(&mut self, retained: &BTreeSet<PackageId>) {
        let mut open_registrations = Vec::new();
        for &package in retained {
            if !self.s3.has_generics()
                || self.packages.role(package) != LinkedPackageRole::External
                || self.packages.is_platform(package)
            {
                continue;
            }
            match self.packages.index(package) {
                Ok(index) => {
                    open_registrations.extend(index.s3.iter().flat_map(|registration| {
                        self.s3
                            .generics_reaching(&registration.class)
                            .filter(|key| {
                                key.name == registration.generic.name
                                    && registration.generic.package.as_deref().is_none_or(|owner| {
                                        owner == self.packages.name(key.package)
                                    })
                            })
                            .map(move |key| (package, key.clone()))
                    }));
                }
                Err(error) => {
                    let node = self.need_node(&Need::Activation { package });
                    self.diagnostic(
                        node,
                        package,
                        None,
                        RejectCode::ObjectSystem,
                        format!("cannot read External S3 registrations: {error}"),
                        None,
                    );
                }
            }
        }
        for (external, generic) in open_registrations {
            let message = format!(
                "External package `{}` registers S3 methods for `{}`, so its method set is open",
                self.packages.name(external),
                generic.name
            );
            for (node, package, span) in self.s3.sites(&generic).to_vec() {
                self.diagnostic(
                    node,
                    package,
                    None,
                    RejectCode::ObjectSystem,
                    message.clone(),
                    Some(span),
                );
            }
        }
        for (node, package, binding, span) in self.s3.take_next_method_calls() {
            let registered = self
                .namespace_builders
                .get(&package)
                .is_some_and(|namespace| {
                    namespace
                        .registrations
                        .iter()
                        .any(|registration| registration.method == binding)
                });
            if !registered && !self.s3.is_closed_method(package, &binding) {
                self.assume(
                    node,
                    package,
                    Some(&binding),
                    RejectCode::ObjectSystem,
                    "NextMethod is not inside a registered method or a method of a closed S3 generic",
                    Some(span),
                );
            }
        }
    }

    pub(super) fn finalize_syntax_observations(&mut self) {
        for observation in self.relocations.observations_of_rewritten_syntax() {
            self.diagnostic(
                observation.node,
                observation.package,
                None,
                RejectCode::SyntaxObservation,
                format!(
                    "{} can observe syntax changed by a planned rewrite",
                    observation.kind
                ),
                Some(observation.span),
            );
        }
    }
}

impl LinkIr {
    /// Immutable semantic construction authority produced by finalization.
    pub fn program(&self) -> &ProgramIr {
        &self.program
    }

    /// Successful typed derivations used only by explanation/query consumers.
    pub fn provenance(&self) -> &crate::ir::ProvenanceIr {
        &self.provenance
    }

    /// Every independent semantic blocker, sorted deterministically.
    pub fn blockers(&self) -> &[Diagnostic] {
        &self.blockers
    }

    pub fn assumptions(&self) -> &[Diagnostic] {
        &self.assumptions
    }

    /// Exact selected installed image and build-time location of every finalized package.
    pub fn package_sources(&self) -> &crate::package::PackageSources {
        &self.packages
    }

    /// Diagnostic source map retained for provenance rendering only.
    pub fn sources(&self) -> &Sources {
        &self.sources
    }

    pub fn construction_evaluations(&self) -> usize {
        self.construction_evaluations
    }
}

#[derive(Debug)]
pub(super) enum FinalizationIssue {
    UnreachedExternal(String),
    OnLoadNotRetained(String),
    CyclicLinkedImports,
    RootOnLoadNotRelocatable,
    NonRelocatableCode {
        package: String,
        binding: String,
    },
    IncompatibleRequirements(String),
    UndeclaredExternal(String),
    UnsatisfiedRequirement {
        identity: crate::package::PackageIdentity,
        requirement: Relation,
    },
    InvalidRelocation(InvalidRelocation),
    Description(String),
}

impl FinalizationIssue {
    fn code(&self) -> RejectCode {
        RejectCode::UnsupportedRootTransformation
    }
}

impl std::fmt::Display for FinalizationIssue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnreachedExternal(name) => write!(
                f,
                "`--external {name}` names a package the retained program never reaches"
            ),
            Self::OnLoadNotRetained(package) => write!(
                f,
                "`{package}::.onLoad` runs when the namespace loads but is not retained"
            ),
            Self::CyclicLinkedImports => f.write_str(
                "Linked namespaces import each other cyclically, which R cannot load",
            ),
            Self::RootOnLoadNotRelocatable => f.write_str(
                "the Root `.onLoad` is not relocatable source, so the generated wrapper cannot call it",
            ),
            Self::NonRelocatableCode { package, binding } => write!(
                f,
                "`{package}::{binding}` needs a code relocation but is not emitted as relocatable source"
            ),
            Self::IncompatibleRequirements(problem) | Self::Description(problem) => {
                f.write_str(problem)
            }
            Self::UndeclaredExternal(package) => write!(
                f,
                "External package `{package}` has no declared DESCRIPTION requirement in the retained program"
            ),
            Self::InvalidRelocation(invalid) => invalid.fmt(f),
            Self::UnsatisfiedRequirement {
                identity,
                requirement,
            } => write!(f, "analyzed {identity} does not satisfy `{requirement}`"),
        }
    }
}

type LinkedActivationContents = (ExportTable, BTreeMap<String, UnretainedName>);

#[derive(Default)]
struct FinalizedNamespaces {
    ids: HashMap<String, FinalizedNamespace>,
    linked: Vec<NamespaceId>,
    on_load: HashMap<NamespaceId, BindingId>,
    root_natives: Vec<NativeComponent>,
    linked_natives: HashMap<NamespaceId, Vec<NativeComponent>>,
}

fn activation_order(
    linked: &[NamespaceId],
    dependencies: &HashMap<NamespaceId, BTreeSet<NamespaceId>>,
    issues: &mut Vec<FinalizationIssue>,
) -> Vec<NamespaceId> {
    let linked_set = linked.iter().copied().collect::<BTreeSet<_>>();
    let mut remaining = linked_set.clone();
    let mut order = Vec::new();
    while !remaining.is_empty() {
        let next = remaining
            .iter()
            .copied()
            .find(|namespace| {
                dependencies.get(namespace).is_none_or(|dependencies| {
                    dependencies.iter().all(|dependency| {
                        !linked_set.contains(dependency) || !remaining.contains(dependency)
                    })
                })
            })
            .unwrap_or_else(|| {
                issues.push(FinalizationIssue::CyclicLinkedImports);
                *remaining.iter().next().expect("remaining namespace")
            });
        remaining.remove(&next);
        order.push(next);
    }
    order
}
