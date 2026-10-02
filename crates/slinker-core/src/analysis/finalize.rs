use super::diagnostic::{Cause, Evidence};
use super::dynamic_names::{CreatedName, CreatorOperation, NameCreator};
use super::object_world::reachable_environment_labels;
use super::relocation::PendingRelocation;
use super::state::{AnalyzerState, LoadedPackage};
use crate::analysis::Need;
use crate::analysis::{Diagnostic, NodeKind, RejectCode};
use crate::ir::CodeIr;
use crate::ir::GenericId;
use crate::ir::NamespaceActivationIr;
use crate::ir::ProvenanceIr;
use crate::ir::ResourceIr;
use crate::ir::{
    BindingId, BindingName, ClosureHome, CodeId, ExportTable, ExternalBindingAccess,
    ExternalPackageContract, FinalizedNamespace, GenericHome, ImportRecordIr, ImportSlotIr,
    InvalidDataset, InvalidPayloadDependency, InvalidRelocation, MaterializedRole,
    MaterializedSlot, MaterializedSlotSource, NamespaceId, ObjectStep,
    PackageRole as LinkedPackageRole, PayloadDependency, ProgramBuilder, ProgramIr,
    RelocationTarget, RemovedImportIr, RootArtifactIr, TargetContract,
};
use crate::metadata::{Relation, RelationField, intersect_requirements, relations};
use crate::package::PackageIdentity;
use crate::package::PackageImage;
use crate::package::PackageSources;
use crate::package::{
    EnvironmentKind, EnvironmentLabel, NativeComponent, PackageAvailability, PackageId,
    PackageName, PackageProvider,
};
use crate::source::generated_description;
use crate::syntax::{SourceKey, SourceOrigin, Sources};
use crate::{Error, Result};
use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::sync::Arc;

#[derive(Debug)]
pub struct LinkIr {
    packages: PackageSources,
    program: ProgramIr,
    provenance: ProvenanceIr,
    blockers: Vec<Diagnostic>,
    sources: Sources,
    construction_evaluations: usize,
}

impl<P: PackageProvider> AnalyzerState<P> {
    fn loaded_ref(&self, package: PackageId) -> Result<&LoadedPackage> {
        self.loaded.get(&package).ok_or_else(|| {
            Error::Analysis(format!(
                "package `{}` was retained without being loaded",
                self.packages.name(package)
            ))
        })
    }

    pub(super) fn finalize(mut self) -> Result<LinkIr> {
        self.finalize_syntax_observations();
        let root = self.root;
        let retained = self
            .encountered
            .union(&self.external)
            .copied()
            .collect::<BTreeSet<_>>();
        let (program, issues) = self.finalize_program(&retained)?;
        let node = self.need_node(&Need::Activation { package: root });
        for issue in issues {
            self.diagnostic(
                node,
                root,
                None,
                RejectCode::UnsupportedRootTransformation,
                issue.to_string(),
                None,
            );
        }
        if retained
            .iter()
            .any(|package| self.packages.role(*package) == LinkedPackageRole::Linked)
        {
            for read in self.reflection.take_computed_namespace_info_reads() {
                self.diagnostic(
                    read.node,
                    read.package,
                    Some(&read.binding),
                    RejectCode::DynamicLookup,
                    format!(
                        "reads `.__NAMESPACE__.` field `{}` of a computed namespace, which can be a synthetic Linked namespace that does not reproduce it",
                        read.field
                    ),
                    Some(read.span),
                );
            }
            for pin in self.invocations.violated_pins() {
                self.diagnostic(
                    pin.node,
                    pin.package,
                    Some(&pin.callable.binding),
                    RejectCode::DynamicLookup,
                    format!(
                        "system.file() package `{}` defaults to \"{}\" but a caller of `{}` can supply it, naming a Linked package whose installation is removed",
                        pin.formal, pin.value, pin.callable.binding
                    ),
                    Some(pin.span),
                );
            }
            for (node, package, span) in self.relocations.take_dynamic_resource_lookups() {
                self.diagnostic(
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
        self.finalize_unresolved_names();
        let blockers = self.diagnostics.into_sorted();
        Ok(LinkIr {
            program,
            provenance: ProvenanceIr::new(self.graph, self.roots),
            blockers,
            sources: self.parses.into_sources(),
            packages: self.packages.sources(retained),
            construction_evaluations: self.construction_evaluations,
        })
    }

    fn finalize_program(
        &self,
        retained: &BTreeSet<PackageId>,
    ) -> Result<(ProgramIr, Vec<FinalizationIssue>)> {
        let target = &self.packages.target_environment().target;
        let root = self.root;
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
        let contracts = self.finalize_packages(&mut builder, &ordered, retained, &mut issues)?;
        let mut namespaces =
            self.finalize_namespaces(&mut builder, ordered, retained, &mut issues)?;
        let mut dependencies =
            self.attach_imports(&mut builder, retained, &namespaces.ids, &mut issues);
        let (root_exports, linked_contents) = self.export_contents(retained, &namespaces.ids)?;
        for (owner, dependency) in self
            .activation_time_dependencies(&namespaces.ids)
            .into_iter()
            .chain(self.attach_payload_dependencies(
                &mut builder,
                retained,
                &namespaces.ids,
                &mut issues,
            ))
        {
            if owner != dependency {
                dependencies.entry(owner).or_default().insert(dependency);
            }
        }
        for (namespace, (exports, removed_bindings)) in
            activation_order(linked_contents, &dependencies, &mut issues)
        {
            builder.add_activation(NamespaceActivationIr {
                namespace,
                on_load: namespaces.on_load.get(&namespace).copied(),
                native_components: namespaces
                    .linked_natives
                    .remove(&namespace)
                    .unwrap_or_default(),
                exports,
                removed_bindings,
            });
        }
        let root_namespace = namespaces.ids[&self.packages.name(root)].namespace;
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
        self.plan_relocations(&mut builder, &namespaces.ids, &mut issues)?;
        let description = self.root_description(&contracts, retained, &mut issues);
        let load = builder.root_load(root_namespace);
        let program = builder.finish(RootArtifactIr {
            description,
            exports: root_exports,
            native_components: namespaces.root_natives,
            on_load: root_on_load,
            load,
        });
        Ok((program, issues))
    }

    fn finalize_packages(
        &self,
        builder: &mut ProgramBuilder,
        ordered: &[(PackageId, LinkedPackageRole)],
        retained: &BTreeSet<PackageId>,
        issues: &mut Vec<FinalizationIssue>,
    ) -> Result<Vec<ExternalPackageContract>> {
        let mut contracts = Vec::new();
        let mut declared = self.declared_external_requirements(ordered)?;
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
        Ok(contracts)
    }

    fn finalize_namespaces(
        &self,
        builder: &mut ProgramBuilder,
        ordered: Vec<(PackageId, LinkedPackageRole)>,
        retained: &BTreeSet<PackageId>,
        issues: &mut Vec<FinalizationIssue>,
    ) -> Result<FinalizedNamespaces> {
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
            let LoadedPackage {
                image,
                namespace: namespace_builder,
                ..
            } = self.loaded_ref(package)?;
            let namespace_label = EnvironmentLabel::namespace(&package_name);
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
                            &binding.object.closure,
                            self.parses
                                .shape(&(package, SourceKey::Binding(name.clone()))),
                        ) {
                            (Some(closure), Some(normalized_shape))
                                if closure.environment == namespace_label =>
                            {
                                MaterializedSlotSource::Closure {
                                    source: Arc::clone(&closure.source),
                                    normalized_shape: normalized_shape.clone(),
                                }
                            }
                            _ => MaterializedSlotSource::Payload,
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
                    GenericId {
                        home: registration
                            .generic
                            .package
                            .filter(|package| retained.contains(package))
                            .map_or(GenericHome::Lexical, GenericHome::Program),
                        name: registration.generic.name.clone(),
                    },
                    registration.class.clone(),
                    method,
                );
            }
            for registration in &namespace_builder.optional_registrations {
                let Some(&method) = namespace.bindings.get(registration.method.as_str()) else {
                    continue;
                };
                builder.attach_s3_registration(
                    namespace.namespace,
                    GenericId {
                        home: GenericHome::Optional(registration.package.clone()),
                        name: registration.generic.clone(),
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
                if let (LinkedPackageRole::Linked, Some(library)) = (role, native.library.path()) {
                    builder.add_resource(ResourceIr {
                        package,
                        path: library.clone(),
                    });
                }
            }
            namespaces.ids.insert(package_name.to_owned(), namespace);
        }
        Ok(namespaces)
    }

    fn attach_imports(
        &self,
        builder: &mut ProgramBuilder,
        retained: &BTreeSet<PackageId>,
        namespace_ids: &HashMap<PackageName, FinalizedNamespace>,
        issues: &mut Vec<FinalizationIssue>,
    ) -> HashMap<NamespaceId, BTreeSet<NamespaceId>> {
        let mut namespace_dependencies = HashMap::<NamespaceId, BTreeSet<NamespaceId>>::new();
        for &package in retained {
            if self.packages.is_external(package) {
                continue;
            }
            let package_name = self.packages.name(package);
            let owner = namespace_ids[&package_name].namespace;
            let table = &self.namespace_imports[&package];
            let (names, records) = match table
                .names()
                .and_then(|names| Ok((names, table.records()?)))
            {
                Ok(resolved) => resolved,
                Err(unknown) => {
                    issues.push(FinalizationIssue::UnknownImportAll {
                        package: package_name.clone(),
                        import: unknown.clone(),
                    });
                    continue;
                }
            };
            let imports = names
                .into_iter()
                .filter_map(|(local, imported)| {
                    let bound = namespace_ids
                        .get(imported.package.as_str())
                        .and_then(|target| target.bindings.get(imported.binding.as_str()));
                    let slot = match bound {
                        Some(&binding) => {
                            namespace_dependencies
                                .entry(owner)
                                .or_default()
                                .insert(builder.binding_namespace(binding));
                            ImportSlotIr::Bound(binding)
                        }
                        None => {
                            let linked =
                                self.known_package(&imported.package)
                                    .is_some_and(|package| {
                                        self.packages.role(package) == LinkedPackageRole::Linked
                                    });
                            if !linked {
                                return None;
                            }
                            ImportSlotIr::Removed(RemovedImportIr {
                                package: imported.package,
                                binding: imported.binding,
                            })
                        }
                    };
                    Some((local, slot))
                })
                .collect();
            let import_records = records
                .into_iter()
                .map(|record| ImportRecordIr {
                    package: record.package,
                    names: record.names.into_iter().collect(),
                })
                .collect();
            builder.set_imports(owner, imports, import_records);
        }
        namespace_dependencies
    }

    fn export_contents(
        &self,
        retained: &BTreeSet<PackageId>,
        namespace_ids: &HashMap<PackageName, FinalizedNamespace>,
    ) -> Result<(ExportTable, HashMap<NamespaceId, LinkedActivationContents>)> {
        let mut root_exports = ExportTable::default();
        let mut linked_contents = HashMap::new();
        for &package in retained {
            if self.packages.is_external(package) {
                continue;
            }
            let index = &self.loaded_ref(package)?.image.index;
            let exports = ExportTable::new(index.exports.values().cloned().collect());
            let finalized = &namespace_ids[&self.packages.name(package)];
            if self.packages.role(package) == LinkedPackageRole::Linked {
                let removed_bindings = index
                    .binding_names
                    .iter()
                    .filter(|name| !finalized.bindings.contains_key(name.as_str()))
                    .cloned()
                    .collect();
                linked_contents.insert(finalized.namespace, (exports, removed_bindings));
            } else {
                root_exports = exports;
            }
        }
        Ok((root_exports, linked_contents))
    }

    fn plan_relocations(
        &self,
        builder: &mut ProgramBuilder,
        namespace_ids: &HashMap<PackageName, FinalizedNamespace>,
        issues: &mut Vec<FinalizationIssue>,
    ) -> Result<()> {
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
                    key: origin.key.clone(),
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
                    let Some(target_namespace) = namespace_ids.get(&self.packages.name(*package))
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
                    let resource_id = builder.add_resource(ResourceIr {
                        package: *package,
                        path: resource.as_str().into(),
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
                PendingRelocation::InstalledQuery { check, .. } => {
                    RelocationTarget::InstalledQuery { check: *check }
                }
                PendingRelocation::NamespaceArgument { package, .. } => {
                    RelocationTarget::NamespaceArgument { package: *package }
                }
                PendingRelocation::NativeSymbol {
                    package,
                    component,
                    symbol,
                    ..
                } => RelocationTarget::NativeSymbol {
                    package: *package,
                    component: component.clone(),
                    symbol: symbol.clone(),
                },
                PendingRelocation::NativeLibrary {
                    package, component, ..
                } => RelocationTarget::NativeLibrary {
                    package: *package,
                    component: component.clone(),
                },
                PendingRelocation::DatasetAccess {
                    package, dataset, ..
                } => {
                    if let Err(invalid) = builder.carry_dataset(*package, dataset.clone()) {
                        issues.push(FinalizationIssue::InvalidDataset(invalid));
                        continue;
                    }
                    RelocationTarget::Dataset {
                        package: *package,
                        dataset: dataset.clone(),
                    }
                }
                PendingRelocation::DataArgument { package, sets, .. } => {
                    let data = &self.loaded_ref(*package)?.image.index.data;
                    let carried = sets.iter().try_for_each(|set| {
                        let objects = data.set(set).unwrap_or_default().to_vec();
                        builder.carry_data_set(*package, set.clone(), objects)
                    });
                    if let Err(invalid) = carried {
                        issues.push(FinalizationIssue::InvalidDataset(invalid));
                        continue;
                    }
                    RelocationTarget::DataArgument { package: *package }
                }
                PendingRelocation::DescriptionArgument { package, .. } => {
                    RelocationTarget::DescriptionArgument {
                        description: builder.add_resource(ResourceIr {
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
        Ok(())
    }

    fn payload_closure(
        &self,
        builder: &mut ProgramBuilder,
        namespace_ids: &HashMap<PackageName, FinalizedNamespace>,
        origin: &SourceOrigin,
    ) -> Option<CodeId> {
        let package = self.known_package(&origin.package)?;
        let image = &self.loaded.get(&package)?.image;
        let shape = self.parses.shape(&(package, origin.key.clone()))?.clone();
        let bundle = builder.payload_bundle(namespace_ids[origin.package.as_str()].namespace)?;
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
                    image.binding(name)?.object.closure.as_ref()?,
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
                        .object
                        .closure
                        .as_ref()?,
                )
            }
            SourceKey::Closure { .. } => return None,
        };
        Some(builder.add_payload_closure(
            bundle,
            home,
            binding,
            CodeIr::new(Arc::clone(&closure.source), shape),
        ))
    }

    fn attach_payload_dependencies(
        &self,
        builder: &mut ProgramBuilder,
        retained: &BTreeSet<PackageId>,
        namespace_ids: &HashMap<PackageName, FinalizedNamespace>,
        issues: &mut Vec<FinalizationIssue>,
    ) -> Vec<(NamespaceId, NamespaceId)> {
        let mut linked = Vec::new();
        for &package in retained {
            let name = self.packages.name(package);
            let owner = &namespace_ids[&name];
            let (Some(bundle), Some(image)) = (
                builder.payload_bundle(owner.namespace),
                self.loaded.get(&package).map(|loaded| &loaded.image),
            ) else {
                continue;
            };
            let payloads = owner
                .bindings
                .iter()
                .filter(|&(_, &binding)| builder.binding_is_payload(binding))
                .map(|(binding, _)| binding.as_str());
            for label in reachable_environment_labels(image, payloads) {
                let EnvironmentKind::Namespace(target) = label.kind() else {
                    continue;
                };
                if target == "base" {
                    continue;
                }
                let Some(dependency) = namespace_ids.get(target) else {
                    issues.push(FinalizationIssue::PayloadOutsideProgram {
                        package: name.to_owned(),
                        namespace: target.to_owned(),
                    });
                    continue;
                };
                match builder.attach_payload_dependency(bundle, dependency.namespace) {
                    Ok(Some(PayloadDependency::Linked(dependency))) => {
                        linked.push((owner.namespace, dependency));
                    }
                    Ok(Some(PayloadDependency::External(_)) | None) => {}
                    Err(InvalidPayloadDependency::RootNamespace) => {
                        issues.push(FinalizationIssue::PayloadRefersToRoot(name.to_owned()));
                    }
                }
            }
        }
        linked
    }

    fn payload_environment_paths(
        &self,
        builder: &ProgramBuilder,
        namespace: &FinalizedNamespace,
        image: &PackageImage,
    ) -> HashMap<EnvironmentLabel, (BindingName, Vec<ObjectStep>)> {
        let mut queue = VecDeque::new();
        for (name, &binding) in &namespace.bindings {
            if builder.binding_is_payload(binding)
                && let Some(environment) = image
                    .binding(name)
                    .and_then(|binding| binding.object.environment.as_ref())
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
                if let Some(reached) = &binding.object.environment {
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
    ) -> Result<HashMap<PackageName, Vec<Relation>>> {
        let mut declared = HashMap::<PackageName, Vec<Relation>>::new();
        for (package, role) in ordered {
            if *role == LinkedPackageRole::External {
                declared.entry(self.packages.name(*package)).or_default();
            }
        }
        for (package, role) in ordered {
            if *role == LinkedPackageRole::External {
                continue;
            }
            let description = &self.loaded_ref(*package)?.image.index.description;
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
        Ok(declared)
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
        namespace_ids: &HashMap<PackageName, FinalizedNamespace>,
    ) -> Vec<(NamespaceId, NamespaceId)> {
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
            let registered = self.loaded.get(&package).is_some_and(|loaded| {
                loaded
                    .namespace
                    .registrations
                    .iter()
                    .any(|registration| registration.method == binding)
            });
            if !registered && !self.s3.is_closed_method(package, &binding) {
                self.diagnostic(
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

    fn finalize_unresolved_names(&mut self) {
        let unknown_registrations = self
            .loaded
            .iter()
            .flat_map(|(&package, loaded)| {
                loaded
                    .image
                    .index
                    .dynlibs
                    .iter()
                    .filter(|native| {
                        native.registration.is_some() && native.library.routines().is_none()
                    })
                    .map(move |native| (package, native.name.clone()))
            })
            .collect::<Vec<_>>();
        for (package, component) in unknown_registrations {
            let node = self.need_node(&Need::Activation { package });
            self.dynamic_names.observe_creator(NameCreator {
                node,
                package,
                binding: BindingName::from(component.as_str()),
                operation: CreatorOperation::DynLibRegistration,
                name: CreatedName::Any,
            });
        }
        let packages = &self.packages;
        let creatable = self
            .dynamic_names
            .creatable(|creator| {
                (
                    packages.name(creator.package).clone(),
                    creator.binding.clone(),
                    creator.operation,
                    creator.created_name().cloned(),
                )
            })
            .map(|(unresolved, creator)| {
                (
                    creator.clone(),
                    Evidence {
                        package: self.packages.name(unresolved.package).clone(),
                        binding: unresolved.binding.clone(),
                        span: Some(unresolved.span.clone()),
                        detail: format!("`{}` is bound nowhere", unresolved.name),
                    },
                )
            })
            .collect::<Vec<_>>();
        for (creator, evidence) in creatable {
            let package = self.packages.name(creator.package).clone();
            let primary = Diagnostic {
                package: package.clone(),
                binding: Some(creator.binding.clone()),
                code: RejectCode::UnresolvedBinding,
                message: match creator.created_name() {
                    Some(created) => format!(
                        "`{}` in `{package}::{}` can bind `{created}` at run time, so a free name bound nowhere may be created by it",
                        creator.operation, creator.binding
                    ),
                    None => format!(
                        "`{}` in `{package}::{}` can bind any name at run time, so free names bound nowhere may be created by it",
                        creator.operation, creator.binding
                    ),
                },
                span: None,
                node: creator.node,
                evidence: Vec::new(),
            };
            let cause = Cause::NameCreator {
                package: creator.package,
                created: creator.created_name().cloned(),
                operation: creator.operation,
                binding: creator.binding,
            };
            self.diagnostics.record_derived(cause, primary, evidence);
        }
    }

    fn finalize_syntax_observations(&mut self) {
        for observation in self.relocations.observations_of_rewritten_syntax() {
            self.diagnostic(
                observation.node,
                observation.package,
                None,
                RejectCode::SyntaxObservation,
                format!(
                    "{} can observe syntax changed by a planned rewrite",
                    observation.callee
                ),
                Some(observation.span),
            );
        }
    }
}

impl LinkIr {
    pub fn program(&self) -> &ProgramIr {
        &self.program
    }

    pub fn provenance(&self) -> &ProvenanceIr {
        &self.provenance
    }

    pub fn blockers(&self) -> &[Diagnostic] {
        &self.blockers
    }

    pub fn package_sources(&self) -> &PackageSources {
        &self.packages
    }

    pub fn sources(&self) -> &Sources {
        &self.sources
    }

    pub fn construction_evaluations(&self) -> usize {
        self.construction_evaluations
    }
}

#[derive(Debug)]
pub(super) enum FinalizationIssue {
    UnreachedExternal(PackageName),
    OnLoadNotRetained(PackageName),
    CyclicLinkedImports,
    RootOnLoadNotRelocatable,
    NonRelocatableCode {
        package: PackageName,
        key: SourceKey,
    },
    IncompatibleRequirements(String),
    UndeclaredExternal(PackageName),
    UnsatisfiedRequirement {
        identity: PackageIdentity,
        requirement: Relation,
    },
    InvalidRelocation(InvalidRelocation),
    InvalidDataset(InvalidDataset),
    Description(String),
    PayloadOutsideProgram {
        package: PackageName,
        namespace: String,
    },
    PayloadRefersToRoot(PackageName),
    UnknownImportAll {
        package: PackageName,
        import: PackageName,
    },
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
                "Linked namespaces import each other or refer to each other's namespace from payloads cyclically, which R cannot load",
            ),
            Self::PayloadOutsideProgram { package, namespace } => write!(
                f,
                "payload bindings of `{package}` refer to namespace `{namespace}`, which the retained program does not contain"
            ),
            Self::PayloadRefersToRoot(package) => write!(
                f,
                "payload bindings of Linked `{package}` refer to the Root namespace, which activates after them"
            ),

            Self::UnknownImportAll { package, import } => write!(
                f,
                "`{package}` imports every export of `{import}`, whose exports are unknown, so its imports environment cannot be reproduced"
            ),
            Self::RootOnLoadNotRelocatable => f.write_str(
                "the Root `.onLoad` is not relocatable source, so the generated wrapper cannot call it",
            ),
            Self::NonRelocatableCode { package, key } => write!(
                f,
                "`{package}::{key}` needs a code relocation but is not emitted as relocatable source"
            ),
            Self::IncompatibleRequirements(problem) | Self::Description(problem) => {
                f.write_str(problem)
            }
            Self::UndeclaredExternal(package) => write!(
                f,
                "External package `{package}` has no declared DESCRIPTION requirement in the retained program"
            ),
            Self::InvalidRelocation(invalid) => invalid.fmt(f),
            Self::InvalidDataset(InvalidDataset::NotLinked(_)) => {
                f.write_str("a dataset can be carried only for a Linked package")
            }
            Self::UnsatisfiedRequirement {
                identity,
                requirement,
            } => write!(f, "analyzed {identity} does not satisfy `{requirement}`"),
        }
    }
}

type LinkedActivationContents = (ExportTable, Vec<BindingName>);

#[derive(Default)]
struct FinalizedNamespaces {
    ids: HashMap<PackageName, FinalizedNamespace>,
    on_load: HashMap<NamespaceId, BindingId>,
    root_natives: Vec<NativeComponent>,
    linked_natives: HashMap<NamespaceId, Vec<NativeComponent>>,
}

fn activation_order<C>(
    linked: HashMap<NamespaceId, C>,
    dependencies: &HashMap<NamespaceId, BTreeSet<NamespaceId>>,
    issues: &mut Vec<FinalizationIssue>,
) -> Vec<(NamespaceId, C)> {
    let mut remaining = linked.into_iter().collect::<Vec<_>>();
    remaining.sort_unstable_by_key(|(namespace, _)| *namespace);
    let mut order = Vec::with_capacity(remaining.len());
    while !remaining.is_empty() {
        let next = remaining
            .iter()
            .position(|(namespace, _)| {
                dependencies.get(namespace).is_none_or(|dependencies| {
                    dependencies.iter().all(|dependency| {
                        remaining.iter().all(|(waiting, _)| waiting != dependency)
                    })
                })
            })
            .unwrap_or_else(|| {
                issues.push(FinalizationIssue::CyclicLinkedImports);
                0
            });
        order.push(remaining.remove(next));
    }
    order
}
