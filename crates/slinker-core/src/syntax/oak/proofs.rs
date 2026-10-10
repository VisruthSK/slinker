use super::census::{Census, IfRegion, assignment_of, node_range, static_arg_of};
use super::context::OakParseContext;
use super::scan::{
    ForRegion, FunctionRegion, contains_call_named, find_if_regions, function_body_range,
    identifier_occurs_before, last_top_level_expression, matching_delimiter, name_token_end,
    skip_trivia_bounded, static_symbol,
};
use super::{LiveUse, innermost_function_region, text_offset};
use crate::syntax::facts::{CallSite, CalleeKind, StaticArg};
use crate::syntax::source::TextRange;
use air_r_syntax::{AnyRExpression, RIfStatement, RRoot};
use biome_rowan::{AstNode, AstNodeList};
use oak_semantic::semantic_index::{DefinitionKind, ScopeId, SemanticIndex};

#[derive(Clone, Copy)]
pub(super) struct ControlRegions<'a> {
    pub(super) for_regions: &'a [ForRegion],
    pub(super) if_regions: &'a [IfRegion],
}

pub(super) fn formal_default_use_is_bound(regions: &[FunctionRegion], live_use: &LiveUse) -> bool {
    let mut containing = regions
        .iter()
        .filter(|region| {
            region
                .formals
                .contains_range(TextRange::new(live_use.start, live_use.end))
        })
        .collect::<Vec<_>>();
    containing.sort_by_key(|region| region.formals.end.saturating_sub(region.formals.start));
    containing
        .into_iter()
        .any(|region| region.parameters.contains(&live_use.name))
}

pub(super) fn for_body_use_is_bound(
    index: &SemanticIndex,
    regions: &[ForRegion],
    live_use: &LiveUse,
) -> bool {
    if !regions.iter().any(|region| {
        region.variable == live_use.name
            && region
                .body
                .contains_range(TextRange::new(live_use.start, live_use.end))
    }) {
        return false;
    }

    index
        .reaching_definitions(live_use.scope, live_use.use_id)
        .any(|(scope, definition_id)| {
            let definition = &index.definitions(scope)[definition_id];
            let symbol = index.symbols(scope).symbol(definition.symbol());
            symbol.name() == live_use.name
                && matches!(definition.kind(), DefinitionKind::ForVariable(_))
        })
}

pub(super) fn recursive_closure_binding_is_initialized(
    root: &RRoot,
    index: &SemanticIndex,
    functions: &[FunctionRegion],
    live_use: &LiveUse,
) -> bool {
    let Some(function) = innermost_function_region(functions, live_use.start) else {
        return false;
    };
    index
        .reaching_definitions(live_use.scope, live_use.use_id)
        .filter(|(scope, _)| *scope != live_use.scope)
        .any(|(scope, definition_id)| {
            let definition = &index.definitions(scope)[definition_id];
            if !matches!(definition.kind(), DefinitionKind::Assignment(_)) {
                return false;
            }
            let symbol = index.symbols(scope).symbol(definition.symbol());
            if symbol.name() != live_use.name {
                return false;
            }
            assignment_of(root, definition.kind()).is_some_and(|assignment| {
                matches!(&assignment.value, AnyRExpression::RFunctionDefinition(value)
                    if node_range(value).start == function.function_start)
            })
        })
}

pub(super) fn post_for_use_may_fall_through(
    text: &str,
    index: &SemanticIndex,
    regions: &[ForRegion],
    if_regions: &[IfRegion],
    live_use: &LiveUse,
) -> bool {
    for region in regions.iter().rev() {
        if region.variable != live_use.name || region.body.end > live_use.start {
            continue;
        }

        let has_reaching_for_definition = index
            .reaching_definitions(live_use.scope, live_use.use_id)
            .any(|(scope, definition_id)| {
                if scope != live_use.scope {
                    return false;
                }
                let definition = &index.definitions(scope)[definition_id];
                let symbol = index.symbols(scope).symbol(definition.symbol());
                symbol.name() == live_use.name
                    && matches!(definition.kind(), DefinitionKind::ForVariable(_))
            });
        if !has_reaching_for_definition {
            continue;
        }

        if scope_has_definite_binding_before(
            text,
            index,
            live_use.scope,
            &live_use.name,
            region.variable_start,
            live_use.start,
            ControlRegions {
                for_regions: regions,
                if_regions,
            },
        ) {
            return false;
        }

        return true;
    }

    false
}

pub(super) fn scope_has_definite_binding_before(
    text: &str,
    index: &SemanticIndex,
    scope: ScopeId,
    name: &str,
    before: usize,
    use_position: usize,
    regions: ControlRegions<'_>,
) -> bool {
    let Some(symbol_id) = index.symbols(scope).id(name) else {
        return false;
    };

    index
        .definitions(scope)
        .iter()
        .any(|(_definition_id, definition)| {
            if definition.symbol() != symbol_id {
                return false;
            }
            match definition.kind() {
                DefinitionKind::Parameter(_) => true,
                DefinitionKind::Assignment(_) | DefinitionKind::Assign { .. } => {
                    let definition_start = text_offset(definition.range().start());
                    definition_start < before
                        && definition_must_execute_before_position(
                            text,
                            definition_start,
                            use_position,
                            regions.for_regions,
                            regions.if_regions,
                        )
                }
                DefinitionKind::ForVariable(_)
                | DefinitionKind::SuperAssignment(_)
                | DefinitionKind::Import { .. } => false,
            }
        })
}

pub(super) fn definition_must_execute_before_position(
    _text: &str,
    definition_start: usize,
    position: usize,
    for_regions: &[ForRegion],
    if_regions: &[IfRegion],
) -> bool {
    if definition_start >= position {
        return false;
    }

    for region in for_regions {
        if definition_start >= region.body.start
            && definition_start < region.body.end
            && !(position >= region.body.start && position < region.body.end)
        {
            return false;
        }
    }

    for region in if_regions {
        let in_then = definition_start >= region.then_branch.start
            && definition_start < region.then_branch.end;
        if in_then && !(position >= region.then_branch.start && position < region.then_branch.end) {
            return false;
        }
        if let Some(TextRange {
            start: else_start,
            end: else_end,
        }) = region.else_branch
        {
            let in_else = definition_start >= else_start && definition_start < else_end;
            if in_else && !(position >= else_start && position < else_end) {
                return false;
            }
        }
    }

    true
}

pub(super) fn conditional_fallthrough_proven_bound(
    text: &str,
    context: &OakParseContext,
    index: &SemanticIndex,
    census: &Census,
    live_use: &LiveUse,
) -> bool {
    let definitions = index
        .reaching_definitions(live_use.scope, live_use.use_id)
        .filter_map(|(scope, definition_id)| {
            let definition = &index.definitions(scope)[definition_id];
            (scope == live_use.scope && matches!(definition.kind(), DefinitionKind::Assignment(_)))
                .then(|| text_offset(definition.range().start()))
        })
        .filter(|start| *start < live_use.start)
        .collect::<Vec<_>>();
    if definitions.iter().any(|start| {
        definition_must_execute_before_position(
            text,
            *start,
            live_use.start,
            &census.fors,
            &census.ifs,
        )
    }) {
        return true;
    }
    census.ifs.iter().any(|region| {
        region
            .else_branch
            .is_some_and(|branch| branch.end <= live_use.start)
            && definition_must_execute_before_position(
                text,
                region.if_start,
                live_use.start,
                &census.fors,
                &census.ifs,
            )
            && if_chain_all_returning_paths_bind(
                text,
                context,
                index,
                &census.ifs,
                region,
                &definitions,
            )
    })
}

pub(super) fn if_chain_all_returning_paths_bind(
    text: &str,
    context: &OakParseContext,
    index: &SemanticIndex,
    regions: &[IfRegion],
    region: &IfRegion,
    definition_starts: &[usize],
) -> bool {
    if !branch_binds_reaching_definition(
        regions,
        region.then_branch.start,
        region.then_branch.end,
        definition_starts,
    ) && !branch_exits_current_function(
        text,
        context,
        index,
        regions,
        region.then_branch.start,
        region.then_branch.end,
    ) {
        return false;
    }

    let Some(TextRange {
        start: else_start,
        end: else_end,
    }) = region.else_branch
    else {
        return false;
    };

    if let Some(nested) = regions
        .iter()
        .find(|candidate| candidate.if_start == else_start)
    {
        return if_chain_all_returning_paths_bind(
            text,
            context,
            index,
            regions,
            nested,
            definition_starts,
        );
    }

    branch_binds_reaching_definition(regions, else_start, else_end, definition_starts)
        || branch_exits_current_function(text, context, index, regions, else_start, else_end)
}

pub(super) fn branch_binds_reaching_definition(
    regions: &[IfRegion],
    branch_start: usize,
    branch_end: usize,
    definition_starts: &[usize],
) -> bool {
    definition_starts.iter().copied().any(|definition_start| {
        definition_start >= branch_start
            && definition_start < branch_end
            && definition_is_top_level_in_branch(regions, branch_start, branch_end, definition_start)
            && !regions.iter().any(|nested| {
                nested.if_start >= branch_start
                    && nested.if_start < branch_end
                    && nested.if_start != branch_start
                    && ((definition_start >= nested.then_branch.start && definition_start < nested.then_branch.end)
                        || matches!(nested.else_branch, Some(TextRange { start, end }) if definition_start >= start && definition_start < end))
            })
    })
}

pub(super) fn definition_is_top_level_in_branch(
    regions: &[IfRegion],
    branch_start: usize,
    branch_end: usize,
    definition_start: usize,
) -> bool {
    let Some(expression) = branch_expression(regions, branch_start, branch_end) else {
        return false;
    };
    match expression {
        AnyRExpression::RBracedExpressions(block) => block
            .expressions()
            .iter()
            .any(|expression| node_range(&expression).start == definition_start),
        expression => node_range(&expression).start == definition_start,
    }
}

fn branch_expression(regions: &[IfRegion], start: usize, end: usize) -> Option<AnyRExpression> {
    regions.iter().find_map(|region| {
        let statement = RIfStatement::cast(region.condition.syntax().parent()?)?;
        let expression = if region.then_branch == TextRange::new(start, end) {
            statement.consequence().ok()?
        } else if region.else_branch == Some(TextRange::new(start, end)) {
            statement.else_clause()?.alternative().ok()?
        } else {
            return None;
        };
        Some(expression)
    })
}

pub(super) fn branch_exits_current_function(
    text: &str,
    context: &OakParseContext,
    index: &SemanticIndex,
    regions: &[IfRegion],
    start: usize,
    end: usize,
) -> bool {
    let Some(expression) = branch_expression(regions, start, end).and_then(last_expression) else {
        return false;
    };
    let range = node_range(&expression);

    if let Some(region) = regions.iter().find(|region| {
        region.if_start == range.start
            && region
                .else_branch
                .is_some_and(|branch| branch.end <= range.end)
    }) {
        return if_chain_all_paths_exit(text, context, index, regions, region);
    }

    let Some((package, callee, callee_start)) = ast_direct_call(&expression) else {
        return false;
    };
    if package.is_none() && callee == "return" {
        return true;
    }
    call_is_non_returning(context, index, package.as_deref(), &callee, callee_start)
}

fn last_expression(expression: AnyRExpression) -> Option<AnyRExpression> {
    match expression {
        AnyRExpression::RBracedExpressions(block) => block.expressions().iter().last(),
        expression => Some(expression),
    }
}

fn ast_direct_call(expression: &AnyRExpression) -> Option<(Option<String>, String, usize)> {
    let AnyRExpression::RCall(call) = expression else {
        return None;
    };
    let function = call.function().ok()?;
    match &function {
        AnyRExpression::RIdentifier(_) => match static_arg_of(&function)? {
            StaticArg::Symbol(name) => Some((None, name.to_string(), node_range(&function).start)),
            _ => None,
        },
        AnyRExpression::RNamespaceExpression(namespace) => {
            let (package, callee) = (namespace.left().ok()?, namespace.right().ok()?);
            Some((
                Some(static_symbol(&package.syntax().text_trimmed().to_string())?),
                static_symbol(&callee.syntax().text_trimmed().to_string())?,
                node_range(&callee).start,
            ))
        }
        _ => None,
    }
}

pub(super) fn if_chain_all_paths_exit(
    text: &str,
    context: &OakParseContext,
    index: &SemanticIndex,
    regions: &[IfRegion],
    region: &IfRegion,
) -> bool {
    if !branch_exits_current_function(
        text,
        context,
        index,
        regions,
        region.then_branch.start,
        region.then_branch.end,
    ) {
        return false;
    }
    let Some(TextRange {
        start: else_start,
        end: else_end,
    }) = region.else_branch
    else {
        return false;
    };
    if let Some(nested) = regions
        .iter()
        .find(|candidate| candidate.if_start == else_start)
    {
        if_chain_all_paths_exit(text, context, index, regions, nested)
    } else {
        branch_exits_current_function(text, context, index, regions, else_start, else_end)
    }
}

pub(super) fn bare_call_is_external(index: &SemanticIndex, callee: &str, start: usize) -> bool {
    let Ok(offset) = u32::try_from(start) else {
        return false;
    };
    let Some((scope, use_id, use_site)) = index.use_at(offset.into()) else {
        return false;
    };
    text_offset(use_site.range().start()) == start
        && index.symbols(scope).symbol(use_site.symbol()).name() == callee
        && !index.use_is_bound(scope, use_id)
        && index.reaching_definitions(scope, use_id).next().is_none()
}

pub(super) fn call_is_non_returning(
    context: &OakParseContext,
    index: &SemanticIndex,
    package: Option<&str>,
    callee: &str,
    callee_start: usize,
) -> bool {
    match package {
        Some("base") => matches!(callee, "stop" | "q" | "quit"),
        Some(_) => false,
        None if !bare_call_is_external(index, callee, callee_start) => false,
        None if matches!(callee, "stop" | "q" | "quit") => context.resolves_to_base(callee),
        None => context.non_returning_names.contains(callee),
    }
}

pub(super) fn direct_call_expression(
    text: &str,
    start: usize,
    end: usize,
) -> Option<(Option<String>, String, usize)> {
    let start = skip_trivia_bounded(text, start, end);
    let first_end = name_token_end(text, start)?;
    let first = static_symbol(text.get(start..first_end)?)?;
    let mut cursor = skip_trivia_bounded(text, first_end, end);

    let (package, callee, callee_start) = if text
        .get(cursor..end)
        .is_some_and(|rest| rest.starts_with(":::"))
    {
        cursor += 3;
        cursor = skip_trivia_bounded(text, cursor, end);
        let callee_start = cursor;
        let callee_end = name_token_end(text, cursor)?;
        let callee = static_symbol(text.get(cursor..callee_end)?)?;
        cursor = callee_end;
        (Some(first), callee, callee_start)
    } else if text
        .get(cursor..end)
        .is_some_and(|rest| rest.starts_with("::"))
    {
        cursor += 2;
        cursor = skip_trivia_bounded(text, cursor, end);
        let callee_start = cursor;
        let callee_end = name_token_end(text, cursor)?;
        let callee = static_symbol(text.get(cursor..callee_end)?)?;
        cursor = callee_end;
        (Some(first), callee, callee_start)
    } else {
        (None, first, start)
    };

    cursor = skip_trivia_bounded(text, cursor, end);
    if text.as_bytes().get(cursor).copied() != Some(b'(') {
        return None;
    }
    let close = matching_delimiter(text, cursor)?;
    let trailing = skip_trivia_bounded(text, close + 1, end);
    (trailing >= end).then_some((package, callee, callee_start))
}

pub(crate) fn closure_definitely_non_returning(text: &str, context: &OakParseContext) -> bool {
    let Some((body_start, body_end)) = function_body_range(text) else {
        return false;
    };
    if contains_call_named(text.get(body_start..body_end).unwrap_or_default(), "return") {
        return false;
    }
    expression_definitely_non_returning(text, context, body_start, body_end)
}

pub(super) fn expression_definitely_non_returning(
    text: &str,
    context: &OakParseContext,
    start: usize,
    end: usize,
) -> bool {
    let Some((expression_start, expression_end)) = last_top_level_expression(text, start, end)
    else {
        return false;
    };

    let regions = find_if_regions(text);
    if let Some(region) = regions.iter().find(|region| {
        region.if_start == expression_start
            && region
                .else_branch
                .is_some_and(|branch| branch.end <= expression_end)
    }) {
        let Some(TextRange {
            start: else_start,
            end: else_end,
        }) = region.else_branch
        else {
            return false;
        };
        return expression_definitely_non_returning(
            text,
            context,
            region.then_branch.start,
            region.then_branch.end,
        ) && expression_definitely_non_returning(text, context, else_start, else_end);
    }

    let Some((package, callee, callee_start)) =
        direct_call_expression(text, expression_start, expression_end)
    else {
        return false;
    };

    match package.as_deref() {
        Some("base") => matches!(callee.as_str(), "stop" | "q" | "quit"),
        Some(_) => false,
        None if matches!(callee.as_str(), "stop" | "q" | "quit") => {
            context.resolves_to_base(&callee)
                && !identifier_occurs_before(text, &callee, callee_start)
        }
        None => {
            context.non_returning_names.contains(callee.as_str())
                && !identifier_occurs_before(text, &callee, callee_start)
        }
    }
}

pub(super) fn is_base_call(context: &OakParseContext, call: &CallSite) -> bool {
    if call.callee_kind != CalleeKind::DefinitelyExternal {
        return false;
    }
    match call.qualified_package.as_deref() {
        Some("base") => true,
        Some(_) => false,
        None => context.resolves_to_base(&call.callee),
    }
}
