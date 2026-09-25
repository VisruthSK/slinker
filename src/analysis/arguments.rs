use crate::syntax::{CallSite, ConstructionCall, Span, StaticArg};

pub(super) trait NamedArguments {
    fn len(&self) -> usize;
    fn name(&self, index: usize) -> Option<&str>;
}

impl NamedArguments for CallSite {
    fn len(&self) -> usize {
        self.args.len()
    }

    fn name(&self, index: usize) -> Option<&str> {
        self.arg_names.get(index).and_then(Option::as_deref)
    }
}

impl NamedArguments for ConstructionCall {
    fn len(&self) -> usize {
        self.arguments.len()
    }

    fn name(&self, index: usize) -> Option<&str> {
        self.arguments.get(index)?.name.as_deref()
    }
}

pub(super) fn matched_arg_index<A, S>(arguments: &A, formals: &[S], target: &str) -> Option<usize>
where
    A: NamedArguments,
    S: AsRef<str>,
{
    let target_index = formals
        .iter()
        .position(|formal| formal.as_ref() == target)?;
    let mut assigned = vec![None; formals.len()];
    let mut consumed = vec![false; arguments.len()];

    // R first performs exact named matching.
    for (arg_index, consumed) in consumed.iter_mut().enumerate() {
        let Some(name) = arguments.name(arg_index) else {
            continue;
        };
        if let Some(formal_index) = formals.iter().position(|formal| formal.as_ref() == name)
            && assigned[formal_index].is_none()
        {
            assigned[formal_index] = Some(arg_index);
            *consumed = true;
        }
    }

    // Then accept an unambiguous partial name. This bounded matcher is used
    // only for primitives whose relevant formal prefix is known here.
    for (arg_index, consumed) in consumed.iter_mut().enumerate() {
        let Some(name) = arguments.name(arg_index) else {
            continue;
        };
        if *consumed {
            continue;
        }
        let candidates = formals
            .iter()
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

    // Remaining unnamed arguments match the remaining formals positionally.
    let mut next_formal = 0;
    for (arg_index, consumed) in consumed.iter_mut().enumerate() {
        if arguments.name(arg_index).is_some() || *consumed {
            continue;
        }
        while next_formal < assigned.len() && assigned[next_formal].is_some() {
            next_formal += 1;
        }
        if next_formal == assigned.len() {
            break;
        }
        assigned[next_formal] = Some(arg_index);
        *consumed = true;
        next_formal += 1;
    }

    assigned[target_index]
}

pub(super) fn matched_call_arg_index(
    call: &CallSite,
    formals: &[&str],
    target: &str,
) -> Option<usize> {
    matched_arg_index(call, formals, target)
}

pub(super) fn matched_static_arg<'a>(
    call: &'a CallSite,
    formals: &[&str],
    target: &str,
) -> Option<&'a StaticArg> {
    let index = matched_call_arg_index(call, formals, target)?;
    call.args.get(index)?.as_ref()
}

pub(super) fn native_selector_span(call: &CallSite) -> Option<&Span> {
    let index = matched_call_arg_index(call, &[".NAME"], ".NAME")?;
    call.arg_spans.get(index)?.as_ref()
}

pub(super) fn static_string_arg(call: &CallSite) -> Option<&str> {
    let argument = match call.callee.as_str() {
        "requireNamespace" | "loadNamespace" | "getNamespace" | "asNamespace" => {
            matched_static_arg(
                call,
                namespace_formals(&call.callee),
                namespace_target(&call.callee),
            )
        }
        "packageVersion" => matched_static_arg(call, &["pkg"], "pkg"),
        "find.package" => matched_static_arg(call, &["package"], "package"),
        "UseMethod" => matched_static_arg(call, &["generic", "object"], "generic"),
        _ => call.args.first().and_then(Option::as_ref),
    }?;
    match argument {
        StaticArg::String(value) => Some(value),
        StaticArg::Symbol(_) => None,
    }
}

pub(super) fn namespace_formals(name: &str) -> &'static [&'static str] {
    match name {
        "requireNamespace" | "loadNamespace" => &["package"],
        "getNamespace" => &["name"],
        "asNamespace" => &["ns"],
        _ => &[],
    }
}

pub(super) fn namespace_target(name: &str) -> &'static str {
    match name {
        "requireNamespace" | "loadNamespace" => "package",
        "getNamespace" => "name",
        "asNamespace" => "ns",
        _ => "",
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
    let selector = matched_call_arg_index(call, &[".NAME"], ".NAME")?;
    let mut current = 0;
    for index in 0..call.args.len() {
        if index == selector
            || call.arg_names.get(index).and_then(Option::as_deref) == Some("PACKAGE")
        {
            continue;
        }
        current += 1;
        if current == position {
            return Some(index);
        }
    }
    None
}
