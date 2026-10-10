use super::NodeId;
use crate::package::PackageId;
use crate::syntax::Span;
use std::collections::HashSet;

#[derive(Default)]
pub(super) struct ReflectionFacts {
    non_reflective_namespace_uses: HashSet<Span>,
    computed_namespace_info_reads: Vec<ComputedNamespaceInfoRead>,
}

pub(super) struct ComputedNamespaceInfoRead {
    pub(super) node: NodeId,
    pub(super) package: PackageId,
    pub(super) binding: String,
    pub(super) field: String,
    pub(super) span: Span,
}

impl ReflectionFacts {
    pub(super) fn add_non_reflective_namespace_uses(&mut self, uses: HashSet<Span>) {
        self.non_reflective_namespace_uses.extend(uses);
    }

    pub(super) fn is_non_reflective_namespace_use(&self, span: &Span) -> bool {
        self.non_reflective_namespace_uses.contains(span)
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
