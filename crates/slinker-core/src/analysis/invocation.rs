use super::arguments::{MatchScope, declared_callables, match_arguments, matched_arg_index};
use super::resolution::ReferenceUse;
use super::s3::{CallableId, callable_target};
use super::state::{AnalyzerState, ParsedSite};
use crate::Result;
use crate::analysis::NodeId;
use crate::package::PackageProvider;
use crate::package::{Atom, PackageId};
use crate::syntax::{CallSite, ParsedExpression, ParsedRFile, Span, StaticArg};
use std::collections::HashMap;

pub(super) type ClassDomain = Option<Vec<Vec<String>>>;

#[derive(Clone, Debug)]
pub(super) struct InvocationArgument {
    pub(super) name: Option<Atom>,
    pub(super) classes: ClassDomain,
    forwards_dots: bool,
}

#[derive(Clone, Debug)]
pub(super) struct Invocation {
    pub(super) arguments: Vec<InvocationArgument>,
}

impl Invocation {
    pub(super) fn argument_for(
        &self,
        formals: &[String],
        formal: &str,
    ) -> Option<&InvocationArgument> {
        if self.arguments.iter().any(|argument| argument.forwards_dots) {
            return None;
        }
        let target = formals.iter().position(|candidate| candidate == formal)?;
        let matched = match_arguments(
            self.arguments
                .iter()
                .map(|argument| argument.name.as_deref()),
            formals,
            MatchScope::Signature,
        )
        .ok()?;
        Some(&self.arguments[matched.argument(target)?])
    }

    pub(super) fn from_call(parsed: &ParsedRFile, call: &CallSite) -> Self {
        Self::from_call_arguments(parsed, call, |_| true)
    }

    fn from_call_arguments(
        parsed: &ParsedRFile,
        call: &CallSite,
        keep: impl Fn(usize) -> bool,
    ) -> Self {
        let arguments = call
            .arguments
            .iter()
            .enumerate()
            .filter(|(index, _)| keep(*index))
            .map(|(_, argument)| InvocationArgument {
                name: argument.name.clone(),
                classes: argument
                    .binding
                    .as_ref()
                    .and_then(|binding| parsed.class_domain_for(binding, call.scope)),
                forwards_dots: argument.name.is_none()
                    && matches!(
                        &argument.value,
                        Some(StaticArg::Symbol(symbol)) if symbol == "..."
                    ),
            })
            .collect();
        Self { arguments }
    }

    fn through_apply(forwarded: Self, positional_elements: usize) -> Self {
        let elements = (0..positional_elements).map(|_| InvocationArgument {
            name: None,
            classes: None,
            forwards_dots: false,
        });
        Self {
            arguments: elements.chain(forwarded.arguments).collect(),
        }
    }

    fn may_supply(&self, formals: &[String], formal: &str) -> bool {
        if self.arguments.iter().any(|argument| argument.forwards_dots) {
            return true;
        }
        let Some(target) = formals.iter().position(|candidate| candidate == formal) else {
            return true;
        };
        match match_arguments(
            self.arguments
                .iter()
                .map(|argument| argument.name.as_deref()),
            formals,
            MatchScope::Signature,
        ) {
            Ok(matched) => matched.argument(target).is_some(),
            Err(_) => true,
        }
    }
}

#[derive(Default)]
pub(super) struct InvocationModel {
    uses: HashMap<CallableId, CallerCoverage>,
    pinned: Vec<PinnedUse>,
}

#[derive(Default)]
pub(super) enum CallerCoverage {
    #[default]
    Pending,
    Known {
        first: Invocation,
        rest: Vec<Invocation>,
    },
    Open,
}

impl CallerCoverage {
    pub(super) fn pending(&self) -> bool {
        matches!(self, Self::Pending)
    }
    pub(super) fn open(&self) -> bool {
        matches!(self, Self::Open)
    }
    pub(super) fn invocations(&self) -> impl Iterator<Item = &Invocation> {
        let (first, rest) = match self {
            Self::Known { first, rest } => (Some(first), rest.as_slice()),
            Self::Pending | Self::Open => (None, &[][..]),
        };
        first.into_iter().chain(rest)
    }
}

impl InvocationModel {
    pub(super) fn record(&mut self, callable: CallableId, invocation: Option<Invocation>) {
        let entry = self.uses.entry(callable).or_default();
        match (entry, invocation) {
            (entry, None) => *entry = CallerCoverage::Open,
            (entry @ CallerCoverage::Pending, Some(first)) => {
                *entry = CallerCoverage::Known {
                    first,
                    rest: Vec::new(),
                }
            }
            (CallerCoverage::Known { rest, .. }, Some(invocation)) => rest.push(invocation),
            (CallerCoverage::Open, Some(_)) => {}
        }
    }

    pub(super) fn record_unclassified(&mut self, callable: CallableId) {
        self.record(callable, None);
    }

    fn is_unclassified(&self, callable: &CallableId) -> bool {
        self.coverage(callable).open()
    }

    pub(super) fn coverage(&self, callable: &CallableId) -> &CallerCoverage {
        self.uses.get(callable).unwrap_or(&CallerCoverage::Pending)
    }

    fn may_supply(&self, callable: &CallableId, formals: &[String], formal: &str) -> bool {
        let coverage = self.coverage(callable);
        coverage.open()
            || coverage.pending()
            || coverage
                .invocations()
                .any(|invocation| invocation.may_supply(formals, formal))
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
        &self,
        site: ParsedSite<'_>,
        parsed: &ParsedRFile,
        expression: &ParsedExpression,
        function_span: &Span,
    ) -> Result<Option<Invocation>> {
        for call in &expression.calls {
            if call.callee == "do.call" {
                if let Some(invocation) =
                    self.do_call_invocation(site, parsed, expression, call, function_span)?
                {
                    return Ok(Some(invocation));
                }
                continue;
            }
            let Some(family) = apply_family(&call.callee) else {
                continue;
            };
            let Some(function) = matched_arg_index(call, family.own_formals, family.function)
            else {
                continue;
            };
            if call.arg_span(function) != Some(function_span)
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
                call.arg_name(index)
                    .is_some_and(|name| names.contains(&name))
            };
            if call
                .arguments
                .iter()
                .any(|argument| argument.name.as_deref() == Some("MoreArgs"))
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

    fn do_call_invocation(
        &self,
        site: ParsedSite<'_>,
        parsed: &ParsedRFile,
        expression: &ParsedExpression,
        call: &CallSite,
        function_span: &Span,
    ) -> Result<Option<Invocation>> {
        let formals = ["what", "args", "quote", "envir"];
        let span_of = |formal: &str| {
            let index = matched_arg_index(call, &formals, formal)?;
            call.arg_span(index)
        };
        if span_of("what") != Some(function_span) {
            return Ok(None);
        }
        let Some(args_span) = span_of("args") else {
            return Ok(None);
        };
        let Some(list) = expression
            .calls
            .iter()
            .find(|candidate| candidate.callee == "list" && candidate.span == *args_span)
        else {
            return Ok(None);
        };
        let resolves_to_base = |state: &Self, call: &CallSite| {
            state.call_resolves_definitely_to_base(
                site.package,
                site.image,
                site.lexical_environment,
                call,
            )
        };
        if !resolves_to_base(self, call)? || !resolves_to_base(self, list)? {
            return Ok(None);
        }
        Ok(Some(Invocation::from_call(parsed, list)))
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
        &self,
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
            let invocation = match call.arg_span(function) {
                Some(span) => self.base_apply_invocation(site, parsed, expression, span)?,
                None => None,
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
