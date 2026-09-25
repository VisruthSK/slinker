use super::state::AnalyzerState;
use crate::analysis::{Diagnostic, NodeKind, RejectCode};
use crate::build::{PackageOperation, PendingRelocation};
use crate::ir::{
    ExternalBindingAccess, ExternalPackageContract, InstalledObjectLocator, MaterializedSlot,
    MaterializedSlotSource, PackageIr, PackageOperationIr, PackageRole as LinkedPackageRole,
    ProgramIr, RootArtifactIr, TargetContract,
};
use crate::metadata::{RelationField, relations};
use crate::package::{Digest, ImportSpec, PackageAvailability, PackageId, PackageProvider};
use crate::syntax::{SourceOrigin, Sources, Span};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::Arc;

#[derive(Debug)]
pub struct LinkIr {
    packages: crate::package::PackageSources,
    program: ProgramIr,
    provenance: crate::ir::ProvenanceIr,
    blockers: crate::ir::AnalysisBlockerSet,
    sources: Sources,
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
        let program = self.finalize_program(root, &retained);
        let mut blockers = crate::ir::AnalysisBlockerSet::default();
        for diagnostic in &self.diagnostics {
            blockers.push(diagnostic_blocker(diagnostic));
        }
        LinkIr {
            program,
            provenance: crate::ir::ProvenanceIr::from_analysis(
                self.graph,
                self.roots,
                self.diagnostics,
            ),
            blockers,
            sources: self.sources,
            packages: self.packages.sources(retained),
        }
    }

    pub(super) fn finalize_program(
        &self,
        root: PackageId,
        retained: &BTreeSet<PackageId>,
    ) -> ProgramIr {
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
        let external_names = ordered
            .iter()
            .filter(|(_, role)| *role == LinkedPackageRole::External)
            .map(|(package, _)| self.packages.name(*package))
            .collect::<HashSet<_>>();
        let mut external_requirements = BTreeMap::<String, BTreeSet<String>>::new();
        for (package, role) in &ordered {
            if !matches!(role, LinkedPackageRole::Root | LinkedPackageRole::Linked) {
                continue;
            }
            let Some(image) = self.images.get(package) else {
                continue;
            };
            if let Ok(imports) = relations(&image.index.description, RelationField::Imports) {
                for relation in imports {
                    if external_names.contains(relation.package()) {
                        external_requirements
                            .entry(relation.package().to_owned())
                            .or_default()
                            .insert(relation.to_string());
                    }
                }
            }
            if let Ok(suggests) = relations(&image.index.description, RelationField::Suggests) {
                for relation in suggests {
                    if external_names.contains(relation.package())
                        && self.explicit_external_packages.contains(relation.package())
                    {
                        external_requirements
                            .entry(relation.package().to_owned())
                            .or_default()
                            .insert(relation.to_string());
                    }
                }
            }
        }
        for (package, role) in &ordered {
            let identity = self.packages.identity(*package).clone();
            let package_ir = match role {
                LinkedPackageRole::Root => PackageIr::Root {
                    build_identity: identity,
                },
                LinkedPackageRole::Linked => PackageIr::Linked {
                    build_identity: identity,
                },
                LinkedPackageRole::External => PackageIr::External {
                    contract: ExternalPackageContract {
                        package: identity.name.clone(),
                        requirements: external_requirements
                            .get(&identity.name)
                            .map(|requirements| requirements.iter().cloned().collect())
                            .unwrap_or_default(),
                    },
                    analyzed_identity: identity,
                },
            };
            builder.add_package(*package, package_ir);
        }

        let mut linked_namespaces = Vec::new();
        let mut namespace_ids = HashMap::new();
        for (package, role) in ordered {
            let package_name = self.packages.name(package);
            if role == LinkedPackageRole::External {
                let bindings = self.graph.nodes.iter().filter_map(|node| match &node.kind {
                    NodeKind::ExternalBinding { name } if node.package == package_name => {
                        Some((name.clone(), ExternalBindingAccess::Exported))
                    }
                    _ => None,
                });
                let namespace = builder.finish_external_namespace(package, bindings);
                namespace_ids.insert(package_name.to_owned(), namespace);
                continue;
            }
            let image = self
                .images
                .get(&package)
                .expect("Root/Linked package has an initialized image");
            let namespace_builder = self
                .namespace_builders
                .get(&package)
                .expect("Root/Linked namespace builder");
            let names = namespace_builder.bindings.clone();
            let slots = names.iter().map(|name| {
                let source =
                    image
                        .binding(name)
                        .map_or(MaterializedSlotSource::Unbound, |binding| {
                            let locator = InstalledObjectLocator {
                                root: name.clone(),
                                path: Vec::new(),
                            };
                            binding.closure.as_ref().map_or(
                                MaterializedSlotSource::Payload {
                                    locator: locator.clone(),
                                },
                                |closure| MaterializedSlotSource::Closure {
                                    source: Arc::clone(&closure.source),
                                    normalized_shape: self
                                        .normalized_shapes
                                        .get(&(package, name.clone()))
                                        .cloned()
                                        .unwrap_or_else(|| Digest::of(closure.source.as_bytes())),
                                    locator,
                                },
                            )
                        });
                MaterializedSlot {
                    name: name.clone(),
                    source,
                }
            });
            let exports = image
                .index
                .exports
                .values()
                .filter(|name| names.contains(*name))
                .cloned();
            let namespace = builder.finish_materialized_namespace(
                package,
                role,
                slots,
                exports,
                image.index.lifecycle.on_load.then(|| ".onLoad".into()),
            );
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
            for native in &image.index.dynlibs {
                builder.attach_native_component(namespace.namespace, native.name.clone());
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
                                self.graph
                                    .nodes
                                    .iter()
                                    .filter_map(|node| match &node.kind {
                                        NodeKind::ExternalBinding { name }
                                            if node.package == *target =>
                                        {
                                            Some(name.clone())
                                        }
                                        _ => None,
                                    })
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
        for (namespace, dependencies) in &namespace_dependencies {
            builder.set_activation_dependencies(*namespace, dependencies.iter().copied().collect());
        }
        let linked_set = linked_namespaces.iter().copied().collect::<BTreeSet<_>>();
        let mut remaining = linked_set.clone();
        let mut ordered_linked = Vec::new();
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
                .unwrap_or_else(|| *remaining.iter().next().expect("remaining namespace"));
            remaining.remove(&next);
            ordered_linked.push(next);
        }
        let root_image = self.images.get(&root).expect("Root image finalized");
        let root_namespace = &namespace_ids[self.packages.name(root)];
        let mut namespace_source = String::new();
        for binding in root_image.index.exports.values() {
            if root_namespace.bindings.contains_key(binding) {
                namespace_source
                    .push_str(&format!("export({})\n", namespace_directive_name(binding)));
            }
        }
        let mut external_namespace_imports = Vec::new();
        let mut linked_imports = Vec::new();
        for import in &root_image.index.imports {
            let (name, imported) = match import {
                ImportSpec::All { package, except } => {
                    if except.is_empty() {
                        (package, Vec::new())
                    } else {
                        continue;
                    }
                }
                ImportSpec::From { package, bindings } => (
                    package,
                    bindings
                        .iter()
                        .filter_map(|binding| {
                            namespace_ids
                                .get(package)
                                .and_then(|namespace| namespace.bindings.get(&binding.remote))
                                .copied()
                        })
                        .collect(),
                ),
            };
            let Some(namespace) = namespace_ids.get(name) else {
                continue;
            };
            match self
                .packages
                .availability(name)
                .and_then(PackageAvailability::package)
                .filter(|package| retained.contains(package))
                .map(|package| self.packages.role(package))
            {
                Some(LinkedPackageRole::External) => {
                    match import {
                        ImportSpec::All { .. } => namespace_source
                            .push_str(&format!("import({})\n", namespace_directive_name(name))),
                        ImportSpec::From { bindings, .. } => {
                            for binding in bindings {
                                namespace_source.push_str(&format!(
                                    "importFrom({}, {})\n",
                                    namespace_directive_name(name),
                                    namespace_directive_name(&binding.remote)
                                ));
                            }
                        }
                    }
                    external_namespace_imports.push(crate::ir::ExternalImportIr {
                        namespace: namespace.namespace,
                        bindings: imported,
                    });
                }
                Some(LinkedPackageRole::Linked) => linked_imports.push(crate::ir::LinkedImportIr {
                    namespace: namespace.namespace,
                    bindings: imported,
                }),
                Some(LinkedPackageRole::Root) | None => {}
            }
        }
        let contracts = external_requirements
            .into_iter()
            .map(|(package, requirements)| ExternalPackageContract {
                package,
                requirements: requirements.into_iter().collect(),
            })
            .collect::<Vec<_>>();
        let mut retained_resources = Vec::new();
        for relocation in &self.pending_relocations {
            let source = pending_relocation_span(relocation);
            let Some(entry) = self.sources.get(&source.source) else {
                continue;
            };
            let SourceOrigin::InstalledBinding {
                package: owner_package,
                binding: owner_binding,
            } = &entry.origin
            else {
                continue;
            };
            let Some(owner_namespace) = namespace_ids.get(owner_package) else {
                continue;
            };
            let Some(&owner_binding) = owner_namespace.bindings.get(owner_binding) else {
                continue;
            };
            let Some(code) = builder.binding_code(owner_binding) else {
                continue;
            };
            let site = builder.add_code_occurrence(code, source.start, source.end);
            match relocation {
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
                    builder.add_relocation(crate::ir::Relocation::Binding {
                        site,
                        target,
                        access: if *internal {
                            ExternalBindingAccess::Internal
                        } else {
                            ExternalBindingAccess::Exported
                        },
                    });
                }
                PendingRelocation::ResourceAccess {
                    package, resource, ..
                } => {
                    let resource_id = builder.add_resource(crate::ir::ResourceIr {
                        package: *package,
                        path: resource.clone(),
                    });
                    retained_resources.push(resource_id);
                    builder.add_relocation(crate::ir::Relocation::Resource {
                        site,
                        target: resource_id,
                    });
                }
                PendingRelocation::PackageOperation {
                    package, operation, ..
                } => builder.add_relocation(crate::ir::Relocation::Package {
                    site,
                    target: *package,
                    operation: match operation {
                        PackageOperation::RequireNamespace { result } => {
                            PackageOperationIr::RequireNamespace { result: *result }
                        }
                        PackageOperation::LoadNamespace => PackageOperationIr::LoadNamespace,
                        PackageOperation::GetNamespace => PackageOperationIr::GetNamespace,
                        PackageOperation::AsNamespace => PackageOperationIr::AsNamespace,
                        PackageOperation::PackageVersion { version } => {
                            PackageOperationIr::PackageVersion {
                                version: version.clone(),
                            }
                        }
                        PackageOperation::FindPackage => PackageOperationIr::FindPackage,
                    },
                }),
            }
        }
        let description = self.root_description.as_ref().map_or_else(
            || Arc::<str>::from(""),
            |source| Arc::from(rewrite_description_imports(source, &contracts)),
        );
        builder.set_root_artifact(RootArtifactIr {
            description,
            namespace: namespace_source.into(),
            external_description_requirements: contracts,
            external_namespace_imports,
            linked_imports,
            bootstrap_namespaces: ordered_linked,
            original_on_load: None,
            retained_resources,
        });
        builder.finish()
    }

    pub(super) fn finalize_syntax_observations(&mut self) {
        if self.observations.is_empty() || self.pending_relocations.is_empty() {
            return;
        }
        let pending_relocations = self
            .pending_relocations
            .iter()
            .map(pending_relocation_span)
            .cloned()
            .collect::<Vec<_>>();
        for observation in self.observations.clone() {
            if pending_relocations
                .iter()
                .any(|rewrite| spans_overlap(&observation.span, rewrite))
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

    /// Complete accumulated semantic blockers.
    pub fn blockers(&self) -> &crate::ir::AnalysisBlockerSet {
        &self.blockers
    }

    /// Exact selected installed image and build-time location of every finalized package.
    pub fn package_sources(&self) -> &crate::package::PackageSources {
        &self.packages
    }

    /// Diagnostic source map retained for provenance rendering only.
    pub fn sources(&self) -> &Sources {
        &self.sources
    }
}

pub(super) fn diagnostic_blocker(diagnostic: &Diagnostic) -> crate::ir::AnalysisBlocker {
    use crate::ir::AnalysisBlocker;
    match diagnostic.code {
        RejectCode::ActiveBinding => AnalysisBlocker::UnsupportedActiveBinding {
            binding: diagnostic.binding.clone().unwrap_or_default(),
        },
        RejectCode::ObjectSystem => AnalysisBlocker::UnsupportedObjectSystem {
            site: diagnostic.span.clone(),
        },
        RejectCode::UnknownNativeEffects | RejectCode::UnknownNativeLookup => {
            AnalysisBlocker::UnsupportedNative {
                component: diagnostic.message.clone(),
            }
        }
        RejectCode::UnknownClosureEnclosure => AnalysisBlocker::MutableClosureEnclosure {
            site: diagnostic.span.clone(),
        },
        RejectCode::EnvironmentMutation => AnalysisBlocker::OpenEnvironmentShape {
            site: diagnostic.span.clone(),
        },
        RejectCode::DynamicLookup | RejectCode::DynamicPackageDiscovery => {
            AnalysisBlocker::OpenCallable {
                site: diagnostic.span.clone(),
            }
        }
        _ => AnalysisBlocker::UnsupportedRootTransformation {
            detail: format!("{:?}: {}", diagnostic.code, diagnostic.message),
        },
    }
}

pub(super) fn pending_relocation_span(rewrite: &PendingRelocation) -> &Span {
    match rewrite {
        PendingRelocation::NamespaceAccess { source, .. }
        | PendingRelocation::ResourceAccess { source, .. }
        | PendingRelocation::PackageOperation { source, .. } => source,
    }
}

pub(super) fn namespace_directive_name(name: &str) -> String {
    if name.bytes().enumerate().all(|(index, byte)| {
        byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'.' && index > 0
    }) {
        name.to_owned()
    } else {
        format!("\"{}\"", name.replace('\\', "\\\\").replace('"', "\\\""))
    }
}

pub(super) fn rewrite_description_imports(
    source: &str,
    contracts: &[ExternalPackageContract],
) -> String {
    let imports = contracts
        .iter()
        .flat_map(|contract| contract.requirements.iter())
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    let lines = source.lines().collect::<Vec<_>>();
    let mut output = Vec::new();
    let mut index = 0;
    let mut inserted = false;
    while index < lines.len() {
        let line = lines[index];
        if line.starts_with("Imports:") {
            if !imports.is_empty() {
                output.push(format!("Imports: {imports}"));
            }
            inserted = true;
            index += 1;
            while index < lines.len()
                && lines[index].chars().next().is_some_and(char::is_whitespace)
            {
                index += 1;
            }
            continue;
        }
        output.push(line.to_owned());
        index += 1;
    }
    if !inserted && !imports.is_empty() {
        output.push(format!("Imports: {imports}"));
    }
    let mut result = output.join("\n");
    result.push('\n');
    result
}

pub(super) fn spans_overlap(left: &Span, right: &Span) -> bool {
    left.source == right.source && left.start < right.end && right.start < left.end
}
