use super::{
    Census, Declarations, IfRegion, LexicalScopes, LiveUse, OakParseContext, Translation,
    assignment_of, ast_span, call_arguments, census, is_base_call, node_name, node_range,
    phase_for_scope, sole_positional_argument, static_arg_of, text_offset,
};
use crate::package::Atom;
use crate::syntax::facts::{
    ActiveBindingDef, CallArgument, CallSite, CalleeKind, LexicalBindingId, NamespaceInfoRead,
    NamespaceInfoReceiver, NamespaceUse, NamespaceUseKind, PackageRef, StaticArg,
    StaticEnvironment,
};
use crate::syntax::source::{SourceId, Span};
use air_r_syntax::{AnyRExpression, RBinaryExpression, RRoot};
use biome_rowan::{AstNode, AstSeparatedList};
use oak_semantic::semantic_index::{DefinitionKind, NamespaceAccessKind, SemanticIndex};
use std::collections::{BTreeMap, HashMap};

const NAMESPACE_INFO: &str = ".__NAMESPACE__.";

pub(super) fn collect_namespace_info_reads(
    source: SourceId,
    census: &Census,
    declarations: &Declarations,
) -> Vec<NamespaceInfoRead> {
    let mut reads = Vec::new();
    for expression in &census.expressions {
        if declarations.is_inert(node_range(expression).start) {
            continue;
        }
        if let Some((receiver, member)) = member_access(expression)
            && member == NAMESPACE_INFO
        {
            reads.push(NamespaceInfoRead {
                receiver: namespace_info_receiver(&receiver),
                field: extracted_field(expression),
                span: ast_span(&source, expression),
            });
        } else if let AnyRExpression::RIdentifier(_) = &expression
            && node_name(expression).as_deref() == Some(NAMESPACE_INFO)
            && let Some(field) = extracted_field(expression)
        {
            reads.push(NamespaceInfoRead {
                receiver: NamespaceInfoReceiver::Lexical,
                field: Some(field),
                span: ast_span(&source, expression),
            });
        }
    }
    reads
}

fn member_access(expression: &AnyRExpression) -> Option<(AnyRExpression, String)> {
    match expression {
        AnyRExpression::RExtractExpression(extract)
            if extract.operator().ok()?.text_trimmed() == "$" =>
        {
            Some((extract.left().ok()?, node_name(&extract.right().ok()?)?))
        }
        AnyRExpression::RSubset2(subset) => {
            let mut arguments = subset.arguments().ok()?.items().iter();
            let index = arguments.next()?.ok()?.value()?;
            if arguments.next().is_some() {
                return None;
            }
            match static_arg_of(&index)? {
                StaticArg::String(member) => {
                    Some((subset.function().ok()?, member.as_str().to_owned()))
                }
                StaticArg::Symbol(_) | StaticArg::Logical(_) => None,
            }
        }
        _ => None,
    }
}

fn extracted_field(expression: &AnyRExpression) -> Option<String> {
    let parent = AnyRExpression::cast(expression.syntax().parent()?)?;
    let (receiver, member) = member_access(&parent)?;
    (receiver.syntax() == expression.syntax()).then_some(member)
}

fn namespace_info_receiver(receiver: &AnyRExpression) -> NamespaceInfoReceiver {
    let AnyRExpression::RCall(call) = receiver else {
        return NamespaceInfoReceiver::Computed;
    };
    let named = call.function().ok().is_some_and(|function| {
        let callee = match function {
            AnyRExpression::RNamespaceExpression(namespace)
                if namespace
                    .left()
                    .ok()
                    .and_then(|name| node_name(&name))
                    .as_deref()
                    == Some("base") =>
            {
                namespace.right().ok().and_then(|name| node_name(&name))
            }
            function => node_name(&function),
        };
        matches!(callee.as_deref(), Some("asNamespace" | "getNamespace"))
    });
    match sole_positional_argument(call).and_then(|argument| static_arg_of(&argument)) {
        Some(StaticArg::String(package)) if named => {
            NamespaceInfoReceiver::Namespace(package.into())
        }
        _ => NamespaceInfoReceiver::Computed,
    }
}

pub(super) fn namespace_access_facts(
    translation: Translation<'_>,
    live_uses: &[LiveUse],
    live_calls: &mut Vec<CallSite>,
) -> Result<Vec<PackageRef>, String> {
    let Translation {
        source,
        index,
        scopes,
        declarations,
        census,
        ..
    } = translation;
    let mut package_refs = Vec::new();
    for access in index.namespace_accesses() {
        let start = text_offset(access.offset());
        if declarations.is_inert(start) {
            continue;
        }
        let namespace = census
            .namespaces
            .get(&start)
            .expect("Oak namespace access belongs to the indexed Air tree");
        let access_end = node_range(namespace).end;
        let internal = matches!(access.kind(), NamespaceAccessKind::Internal);
        let package = namespace
            .left()
            .ok()
            .and_then(|node| node_name(&node))
            .ok_or("namespace package name is not an established literal")?;
        let symbol = namespace
            .right()
            .ok()
            .and_then(|node| node_name(&node))
            .ok_or("namespace binding name is not an established literal")?;
        package_refs.push(PackageRef {
            package: package.as_str().into(),
            symbol: symbol.as_str().into(),
            internal,
            guards: Vec::new(),
            span: Span::new(*source, start, access_end),
        });

        if let Some(raw) = census.call_of(start, access_end) {
            let (scope, _) = index.scope_at(access.offset());
            let Some((lexical_scope, arguments)) =
                call_arguments(source, translation.root, index, live_uses, scopes, raw)
            else {
                continue;
            };
            live_calls.push(CallSite {
                callee: Atom::from(symbol.as_str()),
                callee_kind: CalleeKind::DefinitelyExternal,
                qualification: Some(if internal {
                    crate::syntax::QualifiedCall::Internal(Atom::from(package.as_str()))
                } else {
                    crate::syntax::QualifiedCall::Exported(Atom::from(package.as_str()))
                }),
                arguments,
                scope: lexical_scope,
                phase: phase_for_scope(index, scope),
                guards: Vec::new(),
                span: Span::new(*source, node_range(raw).start, node_range(raw).end),
            });
        }
    }
    Ok(package_refs)
}

pub(super) fn collect_active_bindings(
    source: SourceId,
    context: &OakParseContext,
    calls: &[CallSite],
    aliases: &BTreeMap<String, StaticEnvironment>,
    if_regions: &[IfRegion],
) -> Vec<ActiveBindingDef> {
    let mut bindings = Vec::new();

    for call in calls {
        if call.callee != "makeActiveBinding" || !is_base_call(context, call) {
            continue;
        }
        let Some(StaticArg::String(name)) = call
            .arguments
            .first()
            .and_then(|argument| argument.value.as_ref())
        else {
            continue;
        };
        let Some(target_argument) = call.arguments.get(2) else {
            continue;
        };
        let target = environment_target(context, calls, target_argument, aliases);
        let Some(target) = target else {
            continue;
        };
        let certain = !if_regions.iter().any(|region| {
            region.then_branch.contains_range(call.span.range())
                || region
                    .else_branch
                    .is_some_and(|branch| branch.contains_range(call.span.range()))
        });
        bindings.push(ActiveBindingDef {
            name: name.clone(),
            target,
            certain,
            guards: call.guards.clone(),
            span: Span::new(source, call.span.start, call.span.end),
        });
    }
    bindings
}

pub(super) fn collect_environment_aliases(
    root: &RRoot,
    context: &OakParseContext,
    index: &SemanticIndex,
    calls: &[CallSite],
) -> BTreeMap<String, StaticEnvironment> {
    let mut aliases = BTreeMap::new();
    for scope in index.scope_ids() {
        for (_definition_id, definition) in index.definitions(scope).iter() {
            if !matches!(definition.kind(), DefinitionKind::Assignment(_)) {
                continue;
            }
            let name = index.symbols(scope).symbol(definition.symbol()).name();
            let Some(assignment) = assignment_of(root, definition.kind()) else {
                continue;
            };
            let value = node_range(&assignment.value);
            let Some(target) = calls
                .iter()
                .find(|call| {
                    call.callee == "environment"
                        && is_base_call(context, call)
                        && value.contains_range(call.span.range())
                })
                .or_else(|| calls.iter().find(|call| call.span.range() == value))
                .and_then(|call| static_environment(context, call))
            else {
                continue;
            };
            aliases.insert(name.to_owned(), target);
        }
    }
    aliases
}

fn environment_target(
    context: &OakParseContext,
    calls: &[CallSite],
    argument: &CallArgument,
    aliases: &BTreeMap<String, StaticEnvironment>,
) -> Option<StaticEnvironment> {
    if let Some(StaticArg::Symbol(name)) = &argument.value
        && let Some(target) = aliases.get(name.as_str())
    {
        return Some(target.clone());
    }

    let nested = calls.iter().find(|call| {
        argument
            .span
            .as_ref()
            .is_some_and(|span| span.range().contains_range(call.span.range()))
    })?;
    static_environment(context, nested)
}

fn static_environment(context: &OakParseContext, call: &CallSite) -> Option<StaticEnvironment> {
    if !is_base_call(context, call) {
        return None;
    }
    match (
        call.callee.as_str(),
        call.arguments.first()?.value.as_ref()?,
    ) {
        ("asNamespace" | "getNamespace", StaticArg::String(package)) => {
            Some(StaticEnvironment::Namespace(package.into()))
        }
        ("environment", StaticArg::Symbol(binding)) => {
            Some(StaticEnvironment::ClosureBinding(binding.into()))
        }
        _ => None,
    }
}

fn formal_argument<'a>(
    call: &'a CallSite,
    formals: &[&str],
    target: &str,
) -> Option<&'a CallArgument> {
    let target = formals.iter().position(|formal| *formal == target)?;
    let matched = crate::arguments::match_arguments(
        call.arguments
            .iter()
            .map(|argument| argument.name.as_deref()),
        formals,
        crate::arguments::MatchScope::Projection,
    )
    .ok()?;
    call.arguments.get(matched.argument(target)?)
}

pub(super) fn collect_namespace_uses(
    source: SourceId,
    census: &Census,
    root: &RRoot,
    index: &SemanticIndex,
    scopes: &LexicalScopes,
    calls: &[CallSite],
    declarations: &Declarations,
) -> Vec<NamespaceUse> {
    let mut aliases =
        HashMap::<LexicalBindingId, Vec<(StaticEnvironment, crate::syntax::CallId)>>::new();
    loop {
        let mut changed = false;
        for scope in index.scope_ids() {
            for (_, definition) in index.definitions(scope).iter() {
                let Some(assignment) = assignment_of(root, definition.kind()) else {
                    continue;
                };
                let value = namespace_alias_value(assignment.value);
                let producers = match &value {
                    AnyRExpression::RIdentifier(identifier) => census::identifier_name(identifier)
                        .and_then(|name| scopes.binding(index, &name, scope))
                        .and_then(|binding| aliases.get(&binding).cloned())
                        .unwrap_or_default(),
                    _ => calls
                        .iter()
                        .find(|call| call.span.range() == node_range(&value))
                        .and_then(|call| {
                            static_environment_candidate(call)
                                .map(|target| vec![(target, call.id())])
                        })
                        .unwrap_or_default(),
                };
                let name = index.symbols(scope).symbol(definition.symbol()).name();
                if let Some(binding) = scopes.binding(index, name, scope) {
                    let targets = aliases.entry(binding).or_default();
                    for producer in producers {
                        if !targets.contains(&producer) {
                            targets.push(producer);
                            changed = true;
                        }
                    }
                }
            }
        }
        if !changed {
            break;
        }
    }
    let mut uses = Vec::new();
    for expression in &census.expressions {
        let span = ast_span(&source, expression);
        if declarations.is_inert(span.start) {
            continue;
        }
        if let Some(AnyRExpression::RBinaryExpression(assignment)) =
            expression.syntax().parent().and_then(AnyRExpression::cast)
            && assignment
                .operator()
                .ok()
                .is_some_and(|operator| matches!(operator.text_trimmed(), "<-" | "="))
            && assignment
                .left()
                .ok()
                .is_some_and(|left| left.syntax() == expression.syntax())
        {
            continue;
        }
        let producers = match &expression {
            AnyRExpression::RIdentifier(identifier) => {
                let (scope, _) = scopes.at(index, span.start);
                census::identifier_name(identifier)
                    .and_then(|name| scopes.binding(index, &name, scope))
                    .and_then(|binding| aliases.get(&binding))
                    .cloned()
                    .unwrap_or_default()
            }
            AnyRExpression::RCall(_) => calls
                .iter()
                .find(|call| call.span == span)
                .and_then(|call| {
                    static_environment_candidate(call).map(|target| vec![(target, call.id())])
                })
                .unwrap_or_default(),
            _ => Vec::new(),
        };
        for (target, producer) in producers {
            let mut operation = NamespaceUseKind::Escape;
            if let Some(parent) = expression.syntax().parent().and_then(AnyRExpression::cast) {
                if let Some((receiver, member)) = member_access(&parent) {
                    if receiver.syntax() == expression.syntax() {
                        let written = parent
                            .syntax()
                            .ancestors()
                            .filter_map(RBinaryExpression::cast)
                            .any(|assignment| {
                                assignment.operator().ok().is_some_and(|operator| {
                                    matches!(operator.text_trimmed(), "<-" | "=" | "<<-")
                                }) && assignment.left().ok().is_some_and(|left| {
                                    node_range(&left).contains_range(node_range(&parent))
                                })
                            });
                        operation = if written {
                            NamespaceUseKind::Write(member.into())
                        } else if member == NAMESPACE_INFO {
                            NamespaceUseKind::Metadata
                        } else {
                            let span = ast_span(&source, &parent);
                            match calls.iter().find(|call| {
                                call.span == span && matches!(call.callee.as_str(), "$" | "[[")
                            }) {
                                Some(call) => NamespaceUseKind::Read {
                                    binding: member.into(),
                                    accessor: call.id(),
                                },
                                None => NamespaceUseKind::Escape,
                            }
                        };
                    }
                } else if let AnyRExpression::RBinaryExpression(assignment) = &parent
                    && assignment.operator().ok().is_some_and(|operator| {
                        matches!(operator.text_trimmed(), "<-" | "=" | "->")
                    })
                {
                    // Alias references receive their own obligations. The value may not otherwise escape.
                    if assignment
                        .right()
                        .ok()
                        .is_some_and(|right| right.syntax() == expression.syntax())
                        && matches!(assignment.left().ok(), Some(AnyRExpression::RIdentifier(_)))
                    {
                        operation = NamespaceUseKind::Alias;
                    }
                }
            }
            if operation == NamespaceUseKind::Escape
                && let Some((call, argument)) = calls.iter().find_map(|call| {
                    call.arguments
                        .iter()
                        .enumerate()
                        .find(|(_, argument)| argument.span.as_ref() == Some(&span))
                        .map(|(position, _)| (call, position))
                })
            {
                operation = NamespaceUseKind::Argument {
                    call: call.id(),
                    index: argument,
                };
            }
            uses.push(NamespaceUse {
                target,
                producer,
                operation,
                span: span.clone(),
            });
        }
    }
    uses
}

fn namespace_alias_value(mut value: AnyRExpression) -> AnyRExpression {
    loop {
        let next = match &value {
            AnyRExpression::RParenthesizedExpression(parentheses) => parentheses.body().ok(),
            AnyRExpression::RBinaryExpression(binary) => {
                binary
                    .operator()
                    .ok()
                    .and_then(|token| match token.text_trimmed() {
                        "<-" | "<<-" | "=" => binary.right().ok(),
                        "->" | "->>" => binary.left().ok(),
                        _ => None,
                    })
            }
            _ => None,
        };
        match next {
            Some(next) => value = next,
            None => return value,
        }
    }
}

fn static_environment_candidate(call: &CallSite) -> Option<StaticEnvironment> {
    match call.callee.as_str() {
        "asNamespace" | "getNamespace" | "loadNamespace" => {
            let formals: &[&str] = match call.callee.as_str() {
                "asNamespace" => &["ns", "base.OK"],
                "getNamespace" => &["name"],
                _ => &[
                    "package",
                    "lib.loc",
                    "keep.source",
                    "partial",
                    "versionCheck",
                    "keep.parse.data",
                ],
            };
            match formal_argument(call, formals, formals[0])?.value.as_ref() {
                Some(StaticArg::String(package)) => {
                    Some(StaticEnvironment::Namespace(package.as_str().into()))
                }
                _ => Some(StaticEnvironment::Unproved),
            }
        }
        "environment" => match formal_argument(call, &["fun"], "fun")?.value.as_ref()? {
            StaticArg::Symbol(binding) => {
                Some(StaticEnvironment::ClosureBinding(binding.as_str().into()))
            }
            _ => None,
        },
        "baseenv" | "baseNamespaceEnv" => Some(StaticEnvironment::Namespace("base".into())),
        _ => None,
    }
}
