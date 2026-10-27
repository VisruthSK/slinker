use super::NodeId;
use crate::package::EnvironmentLabel;
use crate::package::{PackageId, PackageName};
use crate::syntax::NamespaceUse;
use crate::syntax::{CallSite, Span};
use std::collections::HashSet;

#[derive(Default)]
pub(super) struct ReflectionFacts {
    computed_namespace_info_reads: Vec<ComputedNamespaceInfoRead>,
    process_observations: Vec<ProcessObservation>,
    loaded_queries: Vec<LoadedQuery>,
    namespace_uses: Vec<NamespaceUseObservation>,
    registry_observations: Vec<ProcessObservation>,
    checked_registry_uses: HashSet<Span>,
    namespace_mutations: Vec<NamespaceMutation>,
    proved_mutations: HashSet<Span>,
    written_callables: Vec<(PackageId, crate::package::BindingName, Span)>,
}

pub(super) struct NamespaceMutation {
    pub(super) node: NodeId,
    pub(super) package: PackageId,
    pub(super) binding: String,
    pub(super) call: CallSite,
}

pub(super) struct NamespaceUseObservation {
    pub(super) node: NodeId,
    pub(super) owner: PackageId,
    pub(super) binding: String,
    pub(super) environment: EnvironmentLabel,
    pub(super) usage: NamespaceUse,
    pub(super) declared_names: Option<Vec<PackageName>>,
}

pub(super) struct LoadedQuery {
    pub(super) node: NodeId,
    pub(super) owner: PackageId,
    pub(super) name: PackageName,
    pub(super) call: CallSite,
}

pub(super) struct ProcessObservation {
    pub(super) node: NodeId,
    pub(super) package: PackageId,
    pub(super) binding: String,
    pub(super) operation: String,
    pub(super) span: Span,
}

pub(super) struct ComputedNamespaceInfoRead {
    pub(super) node: NodeId,
    pub(super) package: PackageId,
    pub(super) binding: String,
    pub(super) field: String,
    pub(super) span: Span,
}

impl ReflectionFacts {
    pub(super) fn observe_namespace_mutation(&mut self, mutation: NamespaceMutation) {
        self.namespace_mutations.push(mutation);
    }
    pub(super) fn prove_mutation(&mut self, span: Span) {
        self.proved_mutations.insert(span);
    }
    pub(super) fn take_unproved_mutations(&mut self) -> Vec<NamespaceMutation> {
        std::mem::take(&mut self.namespace_mutations)
            .into_iter()
            .filter(|mutation| !self.proved_mutations.contains(&mutation.call.span))
            .collect()
    }
    pub(super) fn record_namespace_write(
        &mut self,
        package: PackageId,
        name: crate::package::BindingName,
        span: Span,
    ) {
        self.written_callables.push((package, name, span));
    }
    pub(super) fn take_namespace_writes(
        &mut self,
    ) -> Vec<(PackageId, crate::package::BindingName, Span)> {
        std::mem::take(&mut self.written_callables)
    }
    pub(super) fn observe_registry(&mut self, observation: ProcessObservation) {
        self.registry_observations.push(observation);
    }
    pub(super) fn check_registry_use(&mut self, span: Span) {
        self.checked_registry_uses.insert(span);
    }
    pub(super) fn take_raw_registry_observations(&mut self) -> Vec<ProcessObservation> {
        std::mem::take(&mut self.registry_observations)
            .into_iter()
            .filter(|observation| !self.checked_registry_uses.contains(&observation.span))
            .collect()
    }
    pub(super) fn observe_namespace_use(&mut self, usage: NamespaceUseObservation) {
        self.namespace_uses.push(usage);
    }
    pub(super) fn take_namespace_uses(&mut self) -> Vec<NamespaceUseObservation> {
        std::mem::take(&mut self.namespace_uses)
    }
    pub(super) fn defer_loaded_query(&mut self, query: LoadedQuery) {
        self.loaded_queries.push(query);
    }

    pub(super) fn take_loaded_queries(&mut self) -> Vec<LoadedQuery> {
        std::mem::take(&mut self.loaded_queries)
    }

    pub(super) fn observe_process(&mut self, observation: ProcessObservation) {
        self.process_observations.push(observation);
    }

    pub(super) fn take_process_observations(&mut self) -> Vec<ProcessObservation> {
        std::mem::take(&mut self.process_observations)
    }

    pub(super) fn defer_computed_namespace_info_read(
        &mut self,
        node: NodeId,
        package: PackageId,
        binding: &str,
        field: &str,
        span: Span,
    ) {
        self.computed_namespace_info_reads
            .push(ComputedNamespaceInfoRead {
                node,
                package,
                binding: binding.to_owned(),
                field: field.to_owned(),
                span,
            });
    }

    pub(super) fn take_computed_namespace_info_reads(&mut self) -> Vec<ComputedNamespaceInfoRead> {
        std::mem::take(&mut self.computed_namespace_info_reads)
    }
}
