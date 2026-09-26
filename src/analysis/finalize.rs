use super::relocation::PendingRelocation;
use super::state::AnalyzerState;
use crate::analysis::Need;
use crate::analysis::{Diagnostic, NodeKind, RejectCode};
use crate::ir::{
    ExportTable, ExternalBindingAccess, ExternalPackageContract, InvalidRelocation,
    MaterializedRole, MaterializedSlot, MaterializedSlotSource, PackageIr,
    PackageRole as LinkedPackageRole, ProgramIr, RelocationTarget, RootArtifactIr, TargetContract,
    UnretainedName,
};
use crate::metadata::{Relation, RelationField, intersect_requirements, relations};
use crate::package::{ImportSpec, PackageAvailability, PackageId, PackageProvider};
use crate::source::generated_description;
use crate::syntax::Sources;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
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
            for (node, package, span) in std::mem::take(&mut self.dynamic_resource_lookups) {
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
            sources: self.sources,
            packages: self.packages.sources(retained),
            construction_evaluations: self.construction_evaluations,
        }
    }

    pub(super) fn finalize_program(
        &self,
        retained: &BTreeSet<PackageId>,
    ) -> (ProgramIr, Vec<FinalizationIssue>) {
        let target = &self.packages.target_environment().target;
        let mut builder = ProgramIr::builder(TargetContract {
            r_version: target.r_version.clone(),
            platform: target.os.clone(),
            arch: target.arch.clone(),
        });
        let ordered = retained
            .iter()
            .map(|package| (*package, self.packages.role(*package)))
            .collect::<Vec<_>>();
        let mut retained_bindings = HashMap::<PackageId, BTreeSet<String>>::new();
        for need in self.needs.started() {
            if let Need::Binding { package, binding } = need {
                retained_bindings
                    .entry(*package)
                    .or_default()
                    .insert(binding.clone());
            }
        }
        let mut issues = Vec::new();
        let mut on_load_bindings = HashMap::<crate::ir::NamespaceId, crate::ir::BindingId>::new();
        let mut contracts = Vec::new();
        let mut declared = self.declared_external_requirements(&ordered);
        for (package, role) in &ordered {
            let identity = self.packages.identity(*package).clone();
            let package_ir = match role {
                LinkedPackageRole::Root => PackageIr::Root {
                    build_identity: identity,
                },
                LinkedPackageRole::Linked => PackageIr::Linked {
                    build_identity: identity,
                },
                LinkedPackageRole::External => {
                    let contract = self.external_contract(
                        *package,
                        declared.remove(identity.name.as_str()).unwrap_or_default(),
                        &mut issues,
                    );
                    contracts.push(contract.clone());
                    PackageIr::External {
                        contract,
                        analyzed_identity: identity,
                    }
                }
            };
            builder.add_package(*package, package_ir);
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

        let mut linked_namespaces = Vec::new();
        let mut root_native_components = Vec::new();
        let mut linked_native_components = HashMap::new();
        let mut namespace_ids = HashMap::new();
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
                    namespace_ids.insert(package_name.to_owned(), namespace);
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
                            self.normalized_shapes.get(&(package, name.clone())),
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
                        on_load_bindings.insert(namespace.namespace, binding);
                    }
                    None => issues.push(FinalizationIssue::OnLoadNotRetained(
                        package_name.to_owned(),
                    )),
                }
            }
            for registration in &namespace_builder.registrations {
                let Some(&method) = namespace.bindings.get(&registration.method) else {
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
                MaterializedRole::Root => root_native_components = image.index.dynlibs.clone(),
                MaterializedRole::Linked => {
                    linked_native_components
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
                linked_namespaces.push(namespace.namespace);
            }
            namespace_ids.insert(package_name.to_owned(), namespace);
        }
        let mut namespace_dependencies =
            HashMap::<crate::ir::NamespaceId, BTreeSet<crate::ir::NamespaceId>>::new();
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
                            .map(|image| image.index.exports.values().cloned().collect::<Vec<_>>())
                            .unwrap_or_else(|| {
                                self.external_bindings
                                    .keys()
                                    .filter(|(owner, _)| self.packages.name(*owner) == target)
                                    .map(|(_, name)| name.clone())
                                    .collect()
                            });
                        (
                            target,
                            exported
                                .into_iter()
                                .filter(|name| !except.contains(name))
                                .map(|name| (name.clone(), name))
                                .collect(),
                        )
                    }
                };
                let Some(target) = namespace_ids.get(target_name) else {
                    continue;
                };
                for (local, remote) in pairs {
                    if let Some(&binding) = target.bindings.get(&remote) {
                        builder.attach_import(owner, local, binding);
                        namespace_dependencies
                            .entry(owner)
                            .or_default()
                            .insert(builder.binding_namespace(binding));
                    }
                }
            }
        }
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
                    .filter(|name| !kept.contains_key(*name))
                    .map(|name| (name.clone(), UnretainedName::Stub))
                    .collect::<BTreeMap<_, _>>();
                for name in index
                    .exports
                    .values()
                    .filter(|name| !kept.contains_key(*name))
                {
                    unretained
                        .entry(name.clone())
                        .and_modify(|state| *state = UnretainedName::ExportedStub)
                        .or_insert(UnretainedName::ExportedByActivation);
                }
                linked_contents.insert(namespace, (ExportTable::new(exports), unretained));
            } else {
                root_exports = ExportTable::new(exports);
            }
        }
        for (owner, dependency) in self.activation_time_dependencies(&namespace_ids) {
            if owner != dependency {
                namespace_dependencies
                    .entry(owner)
                    .or_default()
                    .insert(dependency);
            }
        }
        let linked_set = linked_namespaces.iter().copied().collect::<BTreeSet<_>>();
        let mut remaining = linked_set.clone();
        while !remaining.is_empty() {
            let next = remaining
                .iter()
                .copied()
                .find(|namespace| {
                    namespace_dependencies
                        .get(namespace)
                        .is_none_or(|dependencies| {
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
            let (exports, unretained) = linked_contents
                .remove(&next)
                .expect("every Linked namespace has activation contents");
            builder.add_activation(crate::ir::NamespaceActivationIr {
                namespace: next,
                on_load: on_load_bindings.get(&next).copied(),
                native_components: linked_native_components.remove(&next).unwrap_or_default(),
                exports,
                unretained,
            });
        }
        let root = self.root.expect("root package established before analysis");
        let root_namespace = namespace_ids[self.packages.name(root)].namespace;
        let root_on_load = on_load_bindings.get(&root_namespace).and_then(|&binding| {
            let closure = builder.binding_closure(binding);
            if closure.is_none() {
                issues.push(FinalizationIssue::RootOnLoadNotRelocatable);
            }
            closure
        });
        for relocation in &self.pending_relocations {
            let source = relocation.source();
            let origin = self.sources.origin(&source.source);
            let (owner_package, owner_binding) = (&origin.package, &origin.binding);
            let Some(code) = namespace_ids[owner_package.as_str()]
                .bindings
                .get(owner_binding)
                .and_then(|binding| builder.binding_code(*binding))
            else {
                if relocation.reaches_removed_installation() {
                    issues.push(FinalizationIssue::NonRelocatableCode {
                        package: owner_package.clone(),
                        binding: owner_binding.clone(),
                    });
                }
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
                    let Some(&target) = target_namespace.bindings.get(binding) else {
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
                PendingRelocation::RequireNamespace { result, .. } => {
                    RelocationTarget::RequireNamespace { result: *result }
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
            };
            if let Err(invalid) = builder.relocate(code, source.range(), target) {
                issues.push(FinalizationIssue::InvalidRelocation(invalid));
            }
        }
        let imports = contracts
            .iter()
            .flat_map(|contract| contract.requirements.iter().cloned())
            .collect::<Vec<_>>();
        let description = match &self.root_description {
            None => None,
            Some(source) => generated_description(
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
            ),
        };
        builder.set_root_artifact(RootArtifactIr {
            description,
            exports: root_exports,
            native_components: root_native_components,
            on_load: root_on_load,
        });
        (builder.finish(), issues)
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
        declared: Vec<Relation>,
        issues: &mut Vec<FinalizationIssue>,
    ) -> ExternalPackageContract {
        let identity = self.packages.identity(package);
        let platform = self.packages.is_platform(package);
        let requirements = if declared.is_empty() {
            Vec::new()
        } else {
            intersect_requirements(&identity.name, &declared).unwrap_or_else(|problem| {
                issues.push(FinalizationIssue::IncompatibleRequirements(problem));
                Vec::new()
            })
        };
        if !platform && declared.is_empty() {
            issues.push(FinalizationIssue::UndeclaredExternal(identity.name.clone()));
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
                    }))
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
        if self.observations.is_empty() || self.pending_relocations.is_empty() {
            return;
        }
        let pending_relocations = self
            .pending_relocations
            .iter()
            .map(PendingRelocation::source)
            .cloned()
            .collect::<Vec<_>>();
        for observation in self.observations.clone() {
            if pending_relocations
                .iter()
                .any(|rewrite| observation.span.overlaps(rewrite))
            {
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
