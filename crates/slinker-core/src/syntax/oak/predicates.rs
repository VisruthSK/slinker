use super::census::{Census, assignment_of, node_range, static_arg_of};
use super::context::OakParseContext;
use super::scan::{contains_call_named, static_symbol};
use super::text_offset;
use crate::syntax::StaticArg;
use crate::syntax::source::TextRange;
use air_r_syntax::{AnyRExpression, AnyRValue, RRoot, RSyntaxKind};
use biome_rowan::{AstNode, AstSeparatedList};
use oak_semantic::semantic_index::{DefinitionKind, ScopeId, SemanticIndex};
use std::collections::BTreeSet;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct BranchAssumption {
    pub(super) predicate: Option<SimplePredicate>,
    pub(super) symbols: BTreeSet<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum PredicateValue {
    String(String),
    Logical(bool),
    Number(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum SimplePredicate {
    Eq {
        symbol: String,
        value: PredicateValue,
    },
    Ne {
        symbol: String,
        value: PredicateValue,
    },
    IsNull {
        value: ExpressionKey,
        is_null: bool,
    },
    Static(bool),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ExpressionKey(Vec<(RSyntaxKind, String)>);

pub(super) fn branch_assumptions_at(
    index: &SemanticIndex,
    census: &Census,
    position: usize,
) -> Vec<BranchAssumption> {
    let mut assumptions = Vec::new();
    for region in &census.ifs {
        let truth = if position >= region.then_branch.start && position < region.then_branch.end {
            Some(true)
        } else if let Some(TextRange { start, end }) = region.else_branch {
            (position >= start && position < end).then_some(false)
        } else {
            None
        };
        let Some(truth) = truth else {
            continue;
        };
        assumptions.push(assumption(&region.condition, index, truth));
    }
    assumptions
}

pub(super) fn assumptions_imply(known: &[BranchAssumption], required: &[BranchAssumption]) -> bool {
    required.iter().all(|required| {
        let Some(required) = effective_predicate(required) else {
            return false;
        };
        known
            .iter()
            .filter_map(effective_predicate)
            .any(|known| known == required)
    })
}

pub(super) fn assumptions_are_repeatable(
    context: &OakParseContext,
    index: &SemanticIndex,
    scope: ScopeId,
    assumptions: &[BranchAssumption],
) -> bool {
    assumptions.iter().all(|assumption| {
        let Some(predicate) = effective_predicate(assumption) else {
            return false;
        };
        predicate_is_repeatable(context, index, scope, &predicate)
    })
}

pub(super) fn effective_predicate(assumption: &BranchAssumption) -> Option<SimplePredicate> {
    assumption.predicate.clone()
}

pub(super) fn expand_boolean_alias_assumptions(
    root: &RRoot,
    index: &SemanticIndex,
    scope: ScopeId,
    position: usize,
    assumptions: &mut Vec<BranchAssumption>,
) {
    let aliases = assumptions
        .iter()
        .filter_map(effective_predicate)
        .filter_map(|predicate| match predicate {
            SimplePredicate::Eq {
                symbol,
                value: PredicateValue::Logical(value),
            } => Some((symbol, value)),
            _ => None,
        })
        .collect::<Vec<_>>();
    for (symbol, truth) in aliases {
        let Some(symbol_id) = index.symbols(scope).id(&symbol) else {
            continue;
        };
        let rhs = index
            .definitions(scope)
            .iter()
            .filter(|(_, definition)| {
                definition.symbol() == symbol_id
                    && matches!(definition.kind(), DefinitionKind::Assignment(_))
                    && text_offset(definition.range().start()) < position
            })
            .filter_map(|(_, definition)| assignment_of(root, definition.kind()))
            .map(|assignment| assignment.value)
            .max_by_key(|value| node_range(value).start);
        let Some(rhs) = rhs.filter(|rhs| parse_simple_predicate(rhs).is_some()) else {
            continue;
        };
        assumptions.push(assumption(&rhs, index, truth));
    }
}

pub(super) fn predicate_is_repeatable(
    context: &OakParseContext,
    index: &SemanticIndex,
    scope: ScopeId,
    predicate: &SimplePredicate,
) -> bool {
    match predicate {
        SimplePredicate::Eq { .. } | SimplePredicate::Ne { .. } | SimplePredicate::Static(_) => {
            true
        }
        SimplePredicate::IsNull { .. } => {
            context.resolves_to_base("is.null") && index.resolve("is.null", scope).is_none()
        }
    }
}

pub(super) fn negate_predicate(predicate: SimplePredicate) -> SimplePredicate {
    match predicate {
        SimplePredicate::Eq { symbol, value } => SimplePredicate::Ne { symbol, value },
        SimplePredicate::Ne { symbol, value } => SimplePredicate::Eq { symbol, value },
        SimplePredicate::IsNull { value, is_null } => SimplePredicate::IsNull {
            value,
            is_null: !is_null,
        },
        SimplePredicate::Static(value) => SimplePredicate::Static(!value),
    }
}

pub(super) fn unparenthesized(expression: &AnyRExpression) -> AnyRExpression {
    let mut expression = expression.clone();
    while let AnyRExpression::RParenthesizedExpression(parentheses) = &expression {
        let Ok(body) = parentheses.body() else { break };
        expression = body;
    }
    expression
}

pub(super) fn parse_simple_predicate(condition: &AnyRExpression) -> Option<SimplePredicate> {
    let condition = unparenthesized(condition);
    match &condition {
        AnyRExpression::RTrueExpression(_) => Some(SimplePredicate::Static(true)),
        AnyRExpression::RFalseExpression(_) => Some(SimplePredicate::Static(false)),
        AnyRExpression::RUnaryExpression(unary) if unary.operator().ok()?.text_trimmed() == "!" => {
            parse_simple_predicate(&unary.argument().ok()?).map(negate_predicate)
        }
        AnyRExpression::RBinaryExpression(binary) => {
            let operator = binary.operator().ok()?;
            let (left, right) = (binary.left().ok()?, binary.right().ok()?);
            let (symbol, value) = symbol_constant_pair(&left, &right)?;
            match operator.text_trimmed() {
                "==" => Some(SimplePredicate::Eq { symbol, value }),
                "!=" => Some(SimplePredicate::Ne { symbol, value }),
                _ => None,
            }
        }
        AnyRExpression::RCall(call) => {
            let function = call.function().ok()?;
            if symbol_of(&function)?.as_str() != "is.null" {
                return None;
            }
            let arguments = call.arguments().ok()?;
            let mut items = arguments.items().iter();
            let value = unparenthesized(&items.next()?.ok()?.value()?);
            if items.next().is_some() {
                return None;
            }
            let key = ExpressionKey(
                value
                    .syntax()
                    .descendants_tokens(biome_rowan::Direction::Next)
                    .map(|token| (token.kind(), token.text_trimmed().to_owned()))
                    .collect(),
            );
            Some(SimplePredicate::IsNull {
                value: key,
                is_null: true,
            })
        }
        _ => symbol_of(&condition).map(|symbol| SimplePredicate::Eq {
            symbol,
            value: PredicateValue::Logical(true),
        }),
    }
}

fn symbol_of(expression: &AnyRExpression) -> Option<String> {
    match unparenthesized(expression) {
        AnyRExpression::RIdentifier(identifier) => {
            static_symbol(identifier.name_token().ok()?.text_trimmed())
        }
        _ => None,
    }
}

fn symbol_constant_pair(
    left: &AnyRExpression,
    right: &AnyRExpression,
) -> Option<(String, PredicateValue)> {
    symbol_of(left).zip(predicate_constant(right)).or_else(|| {
        predicate_constant(left)
            .zip(symbol_of(right))
            .map(|(value, symbol)| (symbol, value))
    })
}

fn predicate_constant(value: &AnyRExpression) -> Option<PredicateValue> {
    let value = unparenthesized(value);
    match &value {
        AnyRExpression::RTrueExpression(_) => Some(PredicateValue::Logical(true)),
        AnyRExpression::RFalseExpression(_) => Some(PredicateValue::Logical(false)),
        AnyRExpression::AnyRValue(AnyRValue::RStringValue(_)) => match static_arg_of(&value)? {
            StaticArg::String(value) => Some(PredicateValue::String(value.as_str().to_owned())),
            StaticArg::Symbol(_) => None,
        },
        AnyRExpression::AnyRValue(AnyRValue::RIntegerValue(_) | AnyRValue::RDoubleValue(_)) => {
            Some(PredicateValue::Number(
                value.syntax().text_trimmed().to_string(),
            ))
        }
        _ => None,
    }
}

fn assumption(condition: &AnyRExpression, index: &SemanticIndex, truth: bool) -> BranchAssumption {
    let predicate = parse_simple_predicate(condition).map(|predicate| {
        if truth {
            predicate
        } else {
            negate_predicate(predicate)
        }
    });
    let range = condition.syntax().text_trimmed_range();
    let symbols = index
        .scope_ids()
        .flat_map(|scope| {
            index
                .uses(scope)
                .iter()
                .filter(|(_, site)| range.contains_range(site.range()))
                .map(move |(_, site)| index.symbols(scope).symbol(site.symbol()).name().to_owned())
        })
        .collect();
    BranchAssumption { predicate, symbols }
}

pub(super) fn case_is_consistent_with(
    selector: &str,
    value: &PredicateValue,
    predicates: &[SimplePredicate],
) -> bool {
    for predicate in predicates {
        match predicate {
            SimplePredicate::Eq {
                symbol,
                value: required,
            } if symbol == selector => {
                if required != value {
                    return false;
                }
            }
            SimplePredicate::Ne {
                symbol,
                value: excluded,
            } if symbol == selector => {
                if excluded == value {
                    return false;
                }
            }
            SimplePredicate::Static(false) => return false,
            _ => {}
        }
    }
    true
}

pub(super) fn assumption_symbols(assumptions: &[BranchAssumption]) -> BTreeSet<String> {
    assumptions
        .iter()
        .flat_map(|assumption| assumption.symbols.iter().cloned())
        .collect()
}

pub(super) fn condition_symbols_stable(
    text: &str,
    index: &SemanticIndex,
    start: usize,
    end: usize,
    symbols: &BTreeSet<String>,
) -> bool {
    if start >= end || symbols.is_empty() {
        return true;
    }

    for definition_scope in index.scope_ids() {
        for (_, definition) in index.definitions(definition_scope).iter() {
            if !matches!(
                definition.kind(),
                DefinitionKind::Assignment(_)
                    | DefinitionKind::SuperAssignment(_)
                    | DefinitionKind::ForVariable(_)
                    | DefinitionKind::Assign { .. }
            ) {
                continue;
            }
            let definition_start = text_offset(definition.range().start());
            if definition_start <= start || definition_start >= end {
                continue;
            }
            let name = index
                .symbols(definition_scope)
                .symbol(definition.symbol())
                .name();
            if symbols.contains(name) {
                return false;
            }
        }
    }

    let segment = text.get(start..end).unwrap_or_default();
    !contains_call_named(segment, "rm") && !contains_call_named(segment, "remove")
}

pub(super) fn condition_facts_stable(
    text: &str,
    index: &SemanticIndex,
    use_scope: ScopeId,
    defining_scope: ScopeId,
    start: usize,
    end: usize,
    symbols: &BTreeSet<String>,
) -> bool {
    if use_scope == defining_scope {
        condition_symbols_stable(text, index, start, end, symbols)
    } else {
        captured_condition_symbols_stable(text, index, defining_scope, start, end, symbols)
    }
}

pub(super) fn captured_condition_symbols_stable(
    text: &str,
    index: &SemanticIndex,
    defining_scope: ScopeId,
    start: usize,
    end: usize,
    symbols: &BTreeSet<String>,
) -> bool {
    if start >= end || symbols.is_empty() {
        return true;
    }
    for scope in index.scope_ids() {
        for (_, definition) in index.definitions(scope).iter() {
            let mutates_capture = matches!(definition.kind(), DefinitionKind::SuperAssignment(_))
                || (scope == defining_scope
                    && matches!(
                        definition.kind(),
                        DefinitionKind::Assignment(_)
                            | DefinitionKind::ForVariable(_)
                            | DefinitionKind::Assign { .. }
                    ));
            if !mutates_capture {
                continue;
            }
            let position = text_offset(definition.range().start());
            if position <= start || position >= end {
                continue;
            }
            let name = index.symbols(scope).symbol(definition.symbol()).name();
            if symbols.contains(name) {
                return false;
            }
        }
    }
    let segment = text.get(start..end).unwrap_or_default();
    !contains_call_named(segment, "rm") && !contains_call_named(segment, "remove")
}
