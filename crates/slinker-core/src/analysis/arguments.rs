use crate::syntax::{CallSite, DeclaredCallable, ParsedRFile, Span, StaticArg};
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
    let mut assigned = vec![None; formals.len()];
    let mut consumed = vec![false; arguments.arg_count()];
    let before_dots = formals
        .iter()
        .position(|formal| formal.as_ref() == "...")
        .unwrap_or(formals.len());

    for (arg_index, consumed) in consumed.iter_mut().enumerate() {
        let Some(name) = arguments.arg_name(arg_index) else {
            continue;
        };
        if let Some(formal_index) = formals.iter().position(|formal| formal.as_ref() == name)
            && assigned[formal_index].is_none()
        {
            assigned[formal_index] = Some(arg_index);
            *consumed = true;
        }
    }

    for (arg_index, consumed) in consumed.iter_mut().enumerate() {
        let Some(name) = arguments.arg_name(arg_index) else {
            continue;
        };
        if *consumed {
            continue;
        }
        let candidates = formals
            .iter()
            .take(before_dots)
            .enumerate()
            .filter(|(formal_index, formal)| {
                assigned[*formal_index].is_none() && formal.as_ref().starts_with(name)
            })
            .map(|(formal_index, _)| formal_index)
            .collect::<Vec<_>>();
        if candidates.len() == 1 {
            let formal_index = candidates[0];
            assigned[formal_index] = Some(arg_index);
            *consumed = true;
        }
    }

    let mut next_formal = 0;
    for (arg_index, consumed) in consumed.iter_mut().enumerate() {
        if arguments.arg_name(arg_index).is_some() || *consumed {
            continue;
        }
        while next_formal < before_dots && assigned[next_formal].is_some() {
            next_formal += 1;
        }
        if next_formal == before_dots {
            break;
        }
        assigned[next_formal] = Some(arg_index);
        *consumed = true;
        next_formal += 1;
    }

    assigned[target_index]
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

pub(super) fn native_selector_span(call: &CallSite) -> Option<&Span> {
    let index = matched_arg_index(call, &[".NAME"], ".NAME")?;
    call.arg_span(index)
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
        StaticArg::Symbol(_) => None,
    }
}

pub(super) fn namespace_formal(name: &str) -> Option<&'static str> {
    match name {
        "requireNamespace" | "loadNamespace" => Some("package"),
        "getNamespace" => Some("name"),
        "asNamespace" => Some("ns"),
        _ => None,
    }
}

pub(super) fn static_package_arg(call: &CallSite) -> Option<&str> {
    let argument = matched_static_arg(call, &["package"], "package")?;
    match argument {
        StaticArg::String(value) | StaticArg::Symbol(value) => Some(value),
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

pub(super) fn only_package_argument(call: &CallSite, rewritable: &[&str]) -> bool {
    call.arg_count()
        == 1 + call
            .arguments
            .iter()
            .filter_map(|argument| argument.name.as_deref())
            .filter(|name| rewritable.contains(name))
            .count()
}
