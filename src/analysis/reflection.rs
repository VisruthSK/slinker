use crate::syntax::Span;
use std::collections::{HashMap, HashSet};

#[derive(Default)]
pub(super) struct ReflectionFacts {
    non_reflective_namespace_uses: HashSet<Span>,
    contextual_namespace_calls: HashMap<Span, Option<String>>,
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
}
