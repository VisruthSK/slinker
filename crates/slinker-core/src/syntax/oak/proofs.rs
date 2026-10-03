use super::census::{Census, IfRegion, assignment_of, node_range, raw_call, static_arg_of};
use super::context::OakParseContext;
use super::predicates::{
    BranchAssumption, PredicateValue, SimplePredicate, assumption_symbols,
    assumptions_are_repeatable, assumptions_imply, branch_assumptions_at,
    captured_condition_symbols_stable, case_is_consistent_with, condition_facts_stable,
    effective_predicate, expand_boolean_alias_assumptions, parse_simple_predicate, unparenthesized,
};
use super::scan::{
    ForRegion, FunctionRegion, contains_call_named, find_if_regions, function_body_range,
    identifier_occurs_before, last_top_level_expression, matching_delimiter, name_token_end,
    skip_comment, skip_trivia_bounded, statement_start, static_symbol,
};
use super::{LiveUse, innermost_function_region, text_offset};
use crate::syntax::facts::{CallSite, CalleeKind, StaticArg};
use crate::syntax::source::TextRange;
use air_r_syntax::{AnyRExpression, RRoot};
use biome_rowan::AstNode;
use oak_semantic::semantic_index::{DefinitionKind, ScopeId, SemanticIndex};
use std::collections::BTreeSet;

#[derive(Clone, Copy)]
pub(super) struct ControlRegions<'a> {
    pub(super) for_regions: &'a [ForRegion],
    pub(super) if_regions: &'a [IfRegion],
}

#[derive(Clone, Copy)]
pub(super) struct BindingProofContext<'a> {
    pub(super) live_use: &'a LiveUse,
    pub(super) use_assumptions: &'a [BranchAssumption],
    pub(super) definition_starts: &'a [usize],
    pub(super) defining_scope: Option<ScopeId>,
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
    root: &RRoot,
    context: &OakParseContext,
    index: &SemanticIndex,
    census: &Census,
    live_use: &LiveUse,
) -> bool {
    let (for_regions, regions) = (&census.fors, &census.ifs);
    let reaching = index
        .reaching_definitions(live_use.scope, live_use.use_id)
        .filter_map(|(scope, definition_id)| {
            let definition = &index.definitions(scope)[definition_id];
            let symbol = index.symbols(scope).symbol(definition.symbol());
            let definition_start = text_offset(definition.range().start());
            if symbol.name() != live_use.name
                || !matches!(definition.kind(), DefinitionKind::Assignment(_))
            {
                return None;
            }
            if scope != live_use.scope {
                let assumptions = branch_assumptions_at(index, census, definition_start);
                let symbols = assumption_symbols(&assumptions);
                if !captured_condition_symbols_stable(
                    text,
                    index,
                    scope,
                    definition_start,
                    text.len(),
                    &symbols,
                ) {
                    return None;
                }
            }
            Some((definition, definition_start, scope))
        })
        .filter(|(_, start, _)| *start < live_use.start)
        .collect::<Vec<_>>();

    if reaching.is_empty() {
        return false;
    }

    if reaching.iter().any(|(_, definition_start, _)| {
        definition_must_execute_before_position(
            text,
            *definition_start,
            live_use.start,
            for_regions,
            regions,
        )
    }) {
        return true;
    }

    let mut use_assumptions = branch_assumptions_at(index, census, live_use.start);
    expand_boolean_alias_assumptions(
        root,
        index,
        live_use.scope,
        live_use.start,
        &mut use_assumptions,
    );

    for (_definition, definition_start, defining_scope) in &reaching {
        if !definition_is_direct_in_branch(text, regions, *definition_start) {
            continue;
        }
        let definition_assumptions = branch_assumptions_at(index, census, *definition_start);
        if definition_assumptions.is_empty()
            || !assumptions_imply(&use_assumptions, &definition_assumptions)
            || !assumptions_are_repeatable(context, index, live_use.scope, &definition_assumptions)
        {
            continue;
        }
        let symbols = assumption_symbols(&definition_assumptions);
        if condition_facts_stable(
            text,
            index,
            live_use.scope,
            *defining_scope,
            *definition_start,
            live_use.start,
            &symbols,
        ) {
            return true;
        }
    }

    let definition_starts = reaching
        .iter()
        .map(|(_, start, _)| *start)
        .collect::<Vec<_>>();
    let defining_scope = reaching.first().map(|(_, _, scope)| *scope);

    for region in regions {
        let Some(TextRange { end: chain_end, .. }) = region.else_branch else {
            continue;
        };
        if chain_end > live_use.start || region.if_start >= live_use.start {
            continue;
        }

        let chain_assumptions = branch_assumptions_at(index, census, region.if_start);
        if !assumptions_imply(&use_assumptions, &chain_assumptions) {
            continue;
        }
        let symbols = assumption_symbols(&chain_assumptions);
        if !defining_scope.is_some_and(|scope| {
            condition_facts_stable(
                text,
                index,
                live_use.scope,
                scope,
                region.if_start,
                live_use.start,
                &symbols,
            )
        }) {
            continue;
        }

        if if_chain_all_returning_paths_bind(
            text,
            context,
            index,
            regions,
            region,
            &definition_starts,
        ) {
            return true;
        }
    }

    for region in regions {
        let Some(TextRange { end: chain_end, .. }) = region.else_branch else {
            continue;
        };
        if chain_end > live_use.start || region.if_start >= live_use.start {
            continue;
        }
        if exhaustive_equality_dispatch_proves_binding(
            text,
            context,
            index,
            census,
            region,
            BindingProofContext {
                live_use,
                use_assumptions: &use_assumptions,
                definition_starts: &definition_starts,
                defining_scope,
            },
        ) {
            return true;
        }
    }

    false
}

pub(super) fn exhaustive_equality_dispatch_proves_binding(
    text: &str,
    context: &OakParseContext,
    index: &SemanticIndex,
    census: &Census,
    region: &IfRegion,
    proof: BindingProofContext<'_>,
) -> bool {
    let regions = &census.ifs;
    let BindingProofContext {
        live_use,
        use_assumptions,
        definition_starts,
        defining_scope,
    } = proof;
    let mut current = region;
    let mut selector = None::<String>;
    let mut cases = Vec::<(PredicateValue, usize, usize)>::new();
    let final_else = loop {
        let Some(SimplePredicate::Eq { symbol, value }) =
            parse_simple_predicate(&current.condition)
        else {
            return false;
        };
        if selector
            .as_ref()
            .is_some_and(|existing| existing != &symbol)
        {
            return false;
        }
        selector.get_or_insert(symbol);
        if cases.iter().any(|(existing, _, _)| existing == &value) {
            return false;
        }
        cases.push((value, current.then_branch.start, current.then_branch.end));

        let Some(TextRange {
            start: else_start,
            end: else_end,
        }) = current.else_branch
        else {
            break None;
        };
        if let Some(nested) = regions
            .iter()
            .find(|candidate| candidate.if_start == else_start)
        {
            current = nested;
            continue;
        }
        break Some((else_start, else_end));
    };

    let selector = selector.expect("equality chain has at least one selector");
    match final_else {
        Some((start, end)) => {
            if !branch_exits_current_function(text, context, index, regions, start, end) {
                return false;
            }
        }
        None => {
            let Some(allowed) = prior_membership_guard_values(
                text,
                context,
                index,
                census,
                region.if_start,
                &selector,
            ) else {
                return false;
            };
            let covered = cases
                .iter()
                .filter_map(|(value, _, _)| match value {
                    PredicateValue::String(value) => Some(value.clone()),
                    PredicateValue::Logical(_) | PredicateValue::Number(_) => None,
                })
                .collect::<BTreeSet<_>>();
            if covered != allowed {
                return false;
            }
        }
    }

    if !defining_scope.is_some_and(|scope| {
        condition_facts_stable(
            text,
            index,
            live_use.scope,
            scope,
            region.if_start,
            live_use.start,
            &BTreeSet::from([selector.clone()]),
        )
    }) {
        return false;
    }

    let known = use_assumptions
        .iter()
        .filter_map(effective_predicate)
        .collect::<Vec<_>>();
    let mut possible_case = false;
    for (value, branch_start, branch_end) in cases {
        if !case_is_consistent_with(&selector, &value, &known) {
            continue;
        }
        possible_case = true;
        if !branch_all_paths_bind_or_exit(
            text,
            context,
            index,
            regions,
            branch_start,
            branch_end,
            definition_starts,
        ) {
            return false;
        }
    }
    possible_case
}

pub(super) fn branch_all_paths_bind_or_exit(
    text: &str,
    context: &OakParseContext,
    index: &SemanticIndex,
    regions: &[IfRegion],
    start: usize,
    end: usize,
    definition_starts: &[usize],
) -> bool {
    branch_binds_reaching_definition(text, regions, start, end, definition_starts)
        || branch_exits_current_function(text, context, index, regions, start, end)
        || regions.iter().any(|nested| {
            nested.if_start >= start
                && nested.if_start < end
                && definition_is_top_level_in_branch(text, start, end, nested.if_start)
                && if_chain_all_returning_paths_bind(
                    text,
                    context,
                    index,
                    regions,
                    nested,
                    definition_starts,
                )
        })
}

pub(super) fn prior_membership_guard_values(
    text: &str,
    context: &OakParseContext,
    index: &SemanticIndex,
    census: &Census,
    before: usize,
    selector: &str,
) -> Option<BTreeSet<String>> {
    census
        .ifs
        .iter()
        .filter(|region| region.if_start < before && region.then_branch.end <= before)
        .rev()
        .find_map(|region| {
            if !branch_exits_current_function(
                text,
                context,
                index,
                &census.ifs,
                region.then_branch.start,
                region.then_branch.end,
            ) {
                return None;
            }
            membership_guard_values(&region.condition, selector, context, index)
        })
}

fn membership_guard_values(
    condition: &AnyRExpression,
    selector: &str,
    context: &OakParseContext,
    index: &SemanticIndex,
) -> Option<BTreeSet<String>> {
    let condition = unparenthesized(condition);
    if let AnyRExpression::RBinaryExpression(binary) = &condition {
        let operator = binary.operator().ok()?;
        let name = operator.text_trimmed();
        let (scope, _) = index.scope_at(binary.range().start());
        if matches!(name, "||" | "|")
            && context.resolves_to_base(name)
            && index.resolve(name, scope).is_none()
        {
            return membership_guard_values(&binary.left().ok()?, selector, context, index)
                .or_else(|| {
                    membership_guard_values(&binary.right().ok()?, selector, context, index)
                });
        }
    }
    let AnyRExpression::RUnaryExpression(negation) = condition else {
        return None;
    };
    let (scope, _) = index.scope_at(negation.range().start());
    if negation.operator().ok()?.text_trimmed() != "!"
        || !["!", "%in%", "c"]
            .iter()
            .all(|name| context.resolves_to_base(name) && index.resolve(name, scope).is_none())
    {
        return None;
    }
    let AnyRExpression::RBinaryExpression(membership) = unparenthesized(&negation.argument().ok()?)
    else {
        return None;
    };
    if membership.operator().ok()?.text_trimmed() != "%in%"
        || static_arg_of(&unparenthesized(&membership.left().ok()?))
            != Some(StaticArg::Symbol(selector.into()))
    {
        return None;
    }
    let AnyRExpression::RCall(vector) = unparenthesized(&membership.right().ok()?) else {
        return None;
    };
    if static_arg_of(&vector.function().ok()?) != Some(StaticArg::Symbol("c".into())) {
        return None;
    }
    let values = raw_call(&vector)?
        .args
        .into_iter()
        .map(|argument| match argument.static_arg {
            Some(StaticArg::String(value)) => Some(value.as_str().to_owned()),
            _ => None,
        })
        .collect::<Option<BTreeSet<_>>>()?;
    (!values.is_empty()).then_some(values)
}

pub(super) fn definition_is_direct_in_branch(
    text: &str,
    regions: &[IfRegion],
    definition_start: usize,
) -> bool {
    regions
        .iter()
        .filter_map(|region| {
            if definition_start >= region.then_branch.start
                && definition_start < region.then_branch.end
            {
                Some((region.then_branch.start, region.then_branch.end))
            } else if let Some(TextRange { start, end }) = region.else_branch {
                (definition_start >= start && definition_start < end).then_some((start, end))
            } else {
                None
            }
        })
        .min_by_key(|(start, end)| end.saturating_sub(*start))
        .is_some_and(|(start, end)| {
            definition_is_top_level_in_branch(text, start, end, definition_start)
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
        text,
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

    branch_binds_reaching_definition(text, regions, else_start, else_end, definition_starts)
        || branch_exits_current_function(text, context, index, regions, else_start, else_end)
}

pub(super) fn branch_binds_reaching_definition(
    text: &str,
    regions: &[IfRegion],
    branch_start: usize,
    branch_end: usize,
    definition_starts: &[usize],
) -> bool {
    definition_starts.iter().copied().any(|definition_start| {
        definition_start >= branch_start
            && definition_start < branch_end
            && definition_is_top_level_in_branch(text, branch_start, branch_end, definition_start)
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
    text: &str,
    branch_start: usize,
    branch_end: usize,
    definition_start: usize,
) -> bool {
    let start = skip_trivia_bounded(text, branch_start, branch_end);
    if text.as_bytes().get(start).copied() != Some(b'{') {
        return start == definition_start;
    }
    let Some(close) = matching_delimiter(text, start) else {
        return false;
    };
    if definition_start <= start || definition_start >= close {
        return false;
    }

    let statement = skip_trivia_bounded(
        text,
        statement_start(text, definition_start).max(start + 1),
        definition_start,
    );
    if statement != definition_start {
        return false;
    }

    let bytes = text.as_bytes();
    let mut cursor = start + 1;
    let mut brace_depth = 0usize;
    let mut quote = None;
    while cursor < definition_start {
        let byte = bytes[cursor];
        if let Some(delimiter) = quote {
            if byte == b'\\' {
                cursor = (cursor + 2).min(definition_start);
                continue;
            }
            if byte == delimiter {
                quote = None;
            }
            cursor += 1;
            continue;
        }
        match byte {
            b'\'' | b'"' | b'`' => {
                quote = Some(byte);
                cursor += 1;
            }
            b'#' => cursor = skip_comment(text, cursor, definition_start),
            b'{' => {
                brace_depth += 1;
                cursor += 1;
            }
            b'}' => {
                brace_depth = brace_depth.saturating_sub(1);
                cursor += 1;
            }
            _ => cursor += 1,
        }
    }
    brace_depth == 0
}

pub(super) fn branch_exits_current_function(
    text: &str,
    context: &OakParseContext,
    index: &SemanticIndex,
    regions: &[IfRegion],
    start: usize,
    end: usize,
) -> bool {
    let Some((expression_start, expression_end)) = last_top_level_expression(text, start, end)
    else {
        return false;
    };

    if let Some(region) = regions.iter().find(|region| {
        region.if_start == expression_start
            && region
                .else_branch
                .is_some_and(|branch| branch.end <= expression_end)
    }) {
        return if_chain_all_paths_exit(text, context, index, regions, region);
    }

    let Some((package, callee, callee_start)) =
        direct_call_expression(text, expression_start, expression_end)
    else {
        return false;
    };
    if package.is_none() && callee == "return" {
        return true;
    }
    call_is_non_returning(context, index, package.as_deref(), &callee, callee_start)
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
    for scope in index.scope_ids() {
        for (use_id, use_site) in index.uses(scope).iter() {
            if text_offset(use_site.range().start()) != start {
                continue;
            }
            let symbol = index.symbols(scope).symbol(use_site.symbol());
            if symbol.name() != callee {
                continue;
            }
            return !index.use_is_bound(scope, use_id)
                && index.reaching_definitions(scope, use_id).next().is_none();
        }
    }
    false
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
