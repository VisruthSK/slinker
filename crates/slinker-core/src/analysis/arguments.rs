use super::relocation::NamespaceCall;
pub(super) use crate::arguments::{MatchScope, match_arguments};
use crate::ir::NamespaceOperation;
use crate::syntax::{CallSite, DeclaredCallable, ParsedRFile, StaticArg};
use std::collections::BTreeSet;

#[cfg(test)]
#[path = "../../tests/unit/analysis/arguments.rs"]
mod tests;

pub(super) fn matched_arg_index<S>(
    arguments: &CallSite,
    formals: &[S],
    target: &str,
) -> Option<usize>
where
    S: AsRef<str>,
{
    let target_index = formals
        .iter()
        .position(|formal| formal.as_ref() == target)?;
    match_arguments(
        arguments
            .arguments
            .iter()
            .map(|argument| argument.name.as_deref()),
        formals,
        MatchScope::Projection,
    )
    .ok()?
    .argument(target_index)
}

pub(super) fn matched_static_arg<'a>(
    call: &'a CallSite,
    formals: &[&str],
    target: &str,
) -> Option<&'a StaticArg> {
    let index = matched_arg_index(call, formals, target)?;
    call.static_arg(index)
}

pub(super) fn declared_strings(
    parsed: &ParsedRFile,
    call: &CallSite,
    formals: &[&str],
    target: &str,
) -> Option<BTreeSet<String>> {
    let index = matched_arg_index(call, formals, target)?;
    let binding = call.arg_binding(index)?;
    parsed.string_domain_for(binding, call.scope)
}

pub(super) fn declared_callables(
    parsed: &ParsedRFile,
    call: &CallSite,
    index: usize,
) -> Option<BTreeSet<DeclaredCallable>> {
    let binding = call.arg_binding(index)?;
    parsed.callable_domain_for(binding, call.scope)
}

pub(super) fn static_string_arg(call: &CallSite) -> Option<&str> {
    let argument = match call.callee.as_str() {
        "requireNamespace" | "loadNamespace" | "getNamespace" | "asNamespace" => {
            namespace_formal(&call.callee)
                .and_then(|formal| matched_static_arg(call, &[formal], formal))
        }
        "packageVersion" => matched_static_arg(call, &["pkg"], "pkg"),
        "find.package" => matched_static_arg(call, &["package"], "package"),
        "UseMethod" => matched_static_arg(call, &["generic", "object"], "generic"),
        _ => call.static_arg(0),
    }?;
    match argument {
        StaticArg::String(value) => Some(value),
        StaticArg::Symbol(_) | StaticArg::Logical(_) => None,
    }
}

pub(super) fn namespace_formal(name: &str) -> Option<&'static str> {
    namespace_formals(name)?.first().copied()
}

pub(super) fn namespace_formals(name: &str) -> Option<&'static [&'static str]> {
    let operation = match name {
        "requireNamespace" => NamespaceCall::Require,
        "loadNamespace" => NamespaceCall::Operation(NamespaceOperation::Load),
        "getNamespace" => NamespaceCall::Operation(NamespaceOperation::Get),
        "asNamespace" => NamespaceCall::Operation(NamespaceOperation::As),
        _ => return None,
    };
    Some(operation.arguments().0)
}

pub(super) fn static_package_arg(call: &CallSite) -> Option<&str> {
    let argument = matched_static_arg(call, &["package"], "package")?;
    match argument {
        StaticArg::String(value) | StaticArg::Symbol(value) => Some(value),
        StaticArg::Logical(_) => None,
    }
}

pub(super) fn native_call_argument_index(call: &CallSite, position: usize) -> Option<usize> {
    if position == 0 {
        return None;
    }
    let selector = matched_arg_index(call, &[".NAME"], ".NAME")?;
    let mut current = 0;
    for index in 0..call.arg_count() {
        if index == selector || call.arg_name(index) == Some("PACKAGE") {
            continue;
        }
        current += 1;
        if current == position {
            return Some(index);
        }
    }
    None
}

pub(super) fn reflective_name_formals(
    callee: &str,
) -> Option<(&'static [&'static str], &'static str)> {
    match callee {
        "get" => Some((&["x", "pos", "envir", "mode", "inherits"], "x")),
        "get0" => Some((&["x", "envir", "mode", "inherits", "ifnotfound"], "x")),
        "exists" => Some((&["x", "where", "envir", "frame", "mode", "inherits"], "x")),
        "match.fun" => Some((&["FUN", "descend"], "FUN")),
        "do.call" => Some((&["what", "args", "quote", "envir"], "what")),
        _ => None,
    }
}

pub(super) fn only_package_argument(
    call: &CallSite,
    formals: &[&str],
    package: &str,
    rewritable: &[&str],
) -> bool {
    if call.arguments.iter().any(|argument| {
        argument.name.is_none()
            && matches!(&argument.value, Some(StaticArg::Symbol(name)) if name == "...")
    }) {
        return false;
    }
    let Ok(matched) = match_arguments(
        call.arguments
            .iter()
            .map(|argument| argument.name.as_deref()),
        formals,
        MatchScope::Signature,
    ) else {
        return false;
    };
    let Some(primary) = formals.iter().position(|formal| *formal == package) else {
        return false;
    };
    if matched.argument(primary).is_none() {
        return false;
    }
    let consumed = formals
        .iter()
        .enumerate()
        .filter(|(_, formal)| **formal == package || rewritable.contains(formal))
        .filter_map(|(formal, _)| matched.argument(formal))
        .count();
    if consumed != call.arg_count() {
        return false;
    }
    let logical_control = match call.callee.as_str() {
        "requireNamespace" => Some("quietly"),
        "asNamespace" => Some("base.OK"),
        _ => None,
    };
    if let Some(control) = logical_control
        && let Some(argument) = formals
            .iter()
            .position(|formal| *formal == control)
            .and_then(|formal| matched.argument(formal))
    {
        return matches!(call.static_arg(argument), Some(StaticArg::Logical(_)));
    }
    true
}
