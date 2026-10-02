use super::NodeId;
use super::relocation::NamespaceCall;
use crate::package::PackageId;
use crate::syntax::{CallSite, Span};
use std::collections::{HashMap, HashSet};

#[derive(Default)]
pub(super) struct ReflectionFacts {
    non_reflective_namespace_uses: HashSet<Span>,
    contextual_namespace_calls: HashMap<Span, Option<String>>,
    computed_namespace_info_reads: Vec<ComputedNamespaceInfoRead>,
    pending_namespace_operations: Vec<PendingNamespaceOperation>,
}

pub(super) struct PendingNamespaceOperation {
    pub(super) node: NodeId,
    pub(super) package: PackageId,
    pub(super) binding: String,
    pub(super) call: CallSite,
    pub(super) operation: NamespaceCall,
}

pub(super) struct ComputedNamespaceInfoRead {
    pub(super) node: NodeId,
    pub(super) package: PackageId,
    pub(super) binding: String,
    pub(super) field: String,
    pub(super) span: Span,
}

impl ReflectionFacts {
    pub(super) fn set_non_reflective_namespace_uses(&mut self, uses: HashSet<Span>) {
        self.non_reflective_namespace_uses = uses;
    }

    pub(super) fn is_non_reflective_namespace_use(&self, span: &Span) -> bool {
        self.non_reflective_namespace_uses.contains(span)
    }

    pub(super) fn record_contextual_namespace_call(&mut self, span: &Span, package: &str) {
        self.contextual_namespace_calls
            .entry(span.clone())
            .and_modify(|known| {
                if known.as_deref() != Some(package) {
                    *known = None;
                }
            })
            .or_insert_with(|| Some(package.to_owned()));
    }

    pub(super) fn contextual_namespace(&self, span: &Span) -> Option<&str> {
        self.contextual_namespace_calls.get(span)?.as_deref()
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

    pub(super) fn defer_namespace_operation(&mut self, pending: PendingNamespaceOperation) {
        self.pending_namespace_operations.push(pending);
    }

    pub(super) fn take_pending_namespace_operations(&mut self) -> Vec<PendingNamespaceOperation> {
        std::mem::take(&mut self.pending_namespace_operations)
    }

    pub(super) fn take_computed_namespace_info_reads(&mut self) -> Vec<ComputedNamespaceInfoRead> {
        std::mem::take(&mut self.computed_namespace_info_reads)
    }
}
