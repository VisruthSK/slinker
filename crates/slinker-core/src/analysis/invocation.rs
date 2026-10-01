use super::arguments::{declared_callables, matched_arg_index};
use super::resolution::ReferenceUse;
use super::s3::{CallableId, callable_target};
use super::state::{AnalyzerState, ParsedSite};
use crate::Result;
use crate::analysis::NodeId;
use crate::package::PackageId;
use crate::package::PackageProvider;
use crate::syntax::{CallSite, ParsedExpression, ParsedRFile, Span, StaticArg};
use std::collections::{HashMap, HashSet};

pub(super) type ClassDomain = Option<Vec<Vec<String>>>;

#[derive(Clone, Debug)]
pub(super) struct InvocationArgument {
    pub(super) name: Option<String>,
    pub(super) classes: ClassDomain,
    forwards_dots: bool,
}

#[derive(Clone, Debug)]
pub(super) struct Invocation {
    pub(super) arguments: Vec<InvocationArgument>,
}

impl Invocation {
    pub(super) fn from_call(parsed: &ParsedRFile, call: &CallSite) -> Self {
        Self::from_call_arguments(parsed, call, |_| true)
    }

    pub(super) fn from_call_arguments(
        parsed: &ParsedRFile,
        call: &CallSite,
        keep: impl Fn(usize) -> bool,
    ) -> Self {
        let arguments = call
            .arg_names
            .iter()
            .enumerate()
            .filter(|(index, _)| keep(*index))
            .map(|(index, name)| InvocationArgument {
                name: name.clone(),
                classes: call
                    .arg_bindings
                    .get(index)
                    .and_then(Option::as_ref)
                    .and_then(|binding| parsed.class_domain_for(binding, call.scope)),
                forwards_dots: name.is_none()
                    && matches!(
                        call.args.get(index),
                        Some(Some(StaticArg::Symbol(symbol))) if symbol == "..."
                    ),
            })
            .collect();
        Self { arguments }
    }

    pub(super) fn through_apply(forwarded: Self, positional_elements: usize) -> Self {
        let elements = (0..positional_elements).map(|_| InvocationArgument {
            name: None,
            classes: None,
            forwards_dots: false,
        });
        Self {
            arguments: elements.chain(forwarded.arguments).collect(),
        }
    }

    pub(super) fn may_supply(&self, formals: &[String], formal: &str) -> bool {
        if self.arguments.iter().any(|argument| argument.forwards_dots) {
            return true;
        }
        let named = self
            .arguments
            .iter()
            .filter_map(|argument| argument.name.as_deref())
            .collect::<Vec<_>>();
        if named
            .iter()
            .any(|name| formal.starts_with(name) && !formals.iter().any(|other| other == name))
            || named.contains(&formal)
        {
            return true;
        }
        let before_dots = formals.iter().take_while(|formal| *formal != "...");
        let unmatched = before_dots
            .filter(|candidate| {
                !named
                    .iter()
                    .any(|name| candidate.starts_with(name) || *name == candidate.as_str())
            })
            .collect::<Vec<_>>();
        let Some(position) = unmatched.iter().position(|candidate| *candidate == formal) else {
            return false;
        };
        self.arguments
            .iter()
            .filter(|argument| argument.name.is_none())
            .count()
            > position
    }
}

#[derive(Default)]
pub(super) struct InvocationModel {
    uses: HashMap<CallableId, Vec<Option<Invocation>>>,
    unclassified: HashSet<CallableId>,
    pinned: Vec<PinnedUse>,
}

impl InvocationModel {
    pub(super) fn record(&mut self, callable: CallableId, invocation: Option<Invocation>) {
        self.uses.entry(callable).or_default().push(invocation);
    }

    pub(super) fn record_unclassified(&mut self, callable: CallableId) {
        self.unclassified.insert(callable);
    }

    pub(super) fn is_unclassified(&self, callable: &CallableId) -> bool {
        self.unclassified.contains(callable)
    }

    pub(super) fn uses(&self, callable: &CallableId) -> &[Option<Invocation>] {
        self.uses.get(callable).map_or(&[], Vec::as_slice)
    }

    pub(super) fn may_supply(
        &self,
        callable: &CallableId,
        formals: &[String],
        formal: &str,
    ) -> bool {
        self.uses(callable).iter().any(|usage| {
            usage
                .as_ref()
                .is_none_or(|invocation| invocation.may_supply(formals, formal))
        })
    }
}

struct ApplyFamily {
    own_formals: &'static [&'static str],
    function: &'static str,
    control: &'static [&'static str],
    positional_elements: usize,
    forwards_extras: bool,
}

fn apply_family(callee: &str) -> Option<ApplyFamily> {
    let family = match callee {
        "lapply" => ApplyFamily {
            own_formals: &["X", "FUN"],
            function: "FUN",
            control: &[],
            positional_elements: 1,
            forwards_extras: true,
        },
        "sapply" => ApplyFamily {
            own_formals: &["X", "FUN"],
            function: "FUN",
            control: &["simplify", "USE.NAMES"],
            positional_elements: 1,
            forwards_extras: true,
        },
        "vapply" => ApplyFamily {
            own_formals: &["X", "FUN", "FUN.VALUE"],
            function: "FUN",
            control: &["USE.NAMES"],
            positional_elements: 1,
            forwards_extras: true,
        },
        "Map" => ApplyFamily {
            own_formals: &["f"],
            function: "f",
            control: &["SIMPLIFY", "USE.NAMES"],
            positional_elements: 0,
            forwards_extras: true,
        },
        "Filter" => ApplyFamily {
            own_formals: &["f", "x"],
            function: "f",
            control: &[],
            positional_elements: 1,
            forwards_extras: false,
        },
        "Reduce" => ApplyFamily {
            own_formals: &["f", "x", "init", "right", "accumulate", "simplify"],
            function: "f",
            control: &[],
            positional_elements: 2,
            forwards_extras: false,
        },
        _ => return None,
    };
    Some(family)
}

impl<P: PackageProvider> AnalyzerState<P> {
    pub(super) fn base_apply_invocation(
        &mut self,
        site: ParsedSite<'_>,
        parsed: &ParsedRFile,
        expression: &ParsedExpression,
        function_span: &Span,
    ) -> Result<Option<Invocation>> {
        for call in &expression.calls {
            let Some(family) = apply_family(&call.callee) else {
                continue;
            };
            let Some(function) = matched_arg_index(call, family.own_formals, family.function)
            else {
                continue;
            };
            if call.arg_spans.get(function).and_then(Option::as_ref) != Some(function_span)
                || !self.call_resolves_definitely_to_base(
                    site.package,
                    site.image,
                    site.lexical_environment,
                    call,
                )?
            {
                continue;
            }
            let own = family
                .own_formals
                .iter()
                .filter_map(|formal| matched_arg_index(call, family.own_formals, formal))
                .collect::<Vec<_>>();
            let named = |index: usize, names: &[&str]| {
                call.arg_names
                    .get(index)
                    .and_then(Option::as_deref)
                    .is_some_and(|name| names.contains(&name))
            };
            if call
                .arg_names
                .iter()
                .flatten()
                .any(|name| name == "MoreArgs")
            {
                return Ok(None);
            }
            let forwarded = Invocation::from_call_arguments(parsed, call, |index| {
                family.forwards_extras && !own.contains(&index) && !named(index, family.control)
            });
            return Ok(Some(Invocation::through_apply(
                forwarded,
                family.positional_elements,
            )));
        }
        Ok(None)
    }
}

pub(super) struct PinnedUse {
    pub(super) node: NodeId,
    pub(super) package: PackageId,
    pub(super) callable: CallableId,
    pub(super) formals: Vec<String>,
    pub(super) formal: String,
    pub(super) value: String,
    pub(super) span: Span,
}

impl InvocationModel {
    pub(super) fn pin_default(&mut self, pinned: PinnedUse) {
        self.pinned.push(pinned);
    }

    pub(super) fn violated_pins(&mut self) -> Vec<PinnedUse> {
        let pinned = std::mem::take(&mut self.pinned);
        pinned
            .into_iter()
            .filter(|pin| {
                self.is_unclassified(&pin.callable)
                    || self.may_supply(&pin.callable, &pin.formals, &pin.formal)
            })
            .collect()
    }
}

impl<P: PackageProvider> AnalyzerState<P> {
    pub(super) fn process_declared_callable_calls(
        &mut self,
        site: ParsedSite<'_>,
        parsed: &ParsedRFile,
        expression: &ParsedExpression,
    ) -> Result<()> {
        for call in &expression.calls {
            let function = match (call.callee.as_str(), apply_family(&call.callee)) {
                ("do.call", _) => {
                    matched_arg_index(call, &["what", "args", "quote", "envir"], "what")
                }
                (_, Some(family)) => matched_arg_index(call, family.own_formals, family.function),
                _ => None,
            };
            let Some(function) = function else {
                continue;
            };
            let Some(callables) = declared_callables(parsed, call, function) else {
                continue;
            };
            if !self.guards_active(site, &call.guards, &call.span)?
                || !self.call_resolves_definitely_to_base(
                    site.package,
                    site.image,
                    site.lexical_environment,
                    call,
                )?
            {
                continue;
            }
            let invocation = match call.arg_spans.get(function).and_then(Option::as_ref) {
                Some(span) if call.callee != "do.call" => {
                    self.base_apply_invocation(site, parsed, expression, span)?
                }
                _ => None,
            };
            for callable in callables {
                let resolution = self.declared_callable_resolution(
                    site.package,
                    site.image,
                    site.lexical_environment,
                    &callable,
                )?;
                let target = callable_target(&resolution);
                self.require_resolved(
                    site.node,
                    site.package,
                    Some(site.binding),
                    resolution,
                    call.span.clone(),
                    ReferenceUse::Recorded,
                );
                if let Some(target) = target {
                    self.record_use(target, invocation.clone())?;
                }
            }
        }
        Ok(())
    }
}
