use super::census::{assignment_of, node_range};
use super::context::OakParseContext;
use super::scan::{
    IfRegion, contains_call_named, skip_comment, split_top_level_operator, static_string,
    static_symbol, strip_outer_parentheses,
};
use super::text_offset;
use crate::syntax::source::TextRange;
use air_r_syntax::RRoot;
use oak_semantic::semantic_index::{DefinitionKind, ScopeId, SemanticIndex};
use std::collections::BTreeSet;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct BranchAssumption {
    pub(super) condition: String,
    pub(super) truth: bool,
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
        value: String,
        is_null: bool,
    },
    Static(bool),
}

pub(super) fn branch_assumptions_at(
    text: &str,
    regions: &[IfRegion],
    position: usize,
) -> Vec<BranchAssumption> {
    let mut assumptions = Vec::new();
    for region in regions {
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
        assumptions.push(BranchAssumption {
            condition: canonical_condition(text, region.condition.start, region.condition.end),
            truth,
            symbols: condition_symbols(text, region.condition.start, region.condition.end),
        });
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
    let predicate = parse_simple_predicate(&assumption.condition)?;
    if assumption.truth {
        Some(predicate)
    } else {
        Some(negate_predicate(predicate))
    }
}

pub(super) fn expand_boolean_alias_assumptions(
    text: &str,
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
            .map(|assignment| node_range(&assignment.value))
            .max_by_key(|range| range.start)
            .and_then(|range| text.get(range.start..range.end));
        let Some(rhs) = rhs.filter(|rhs| parse_simple_predicate(rhs).is_some()) else {
            continue;
        };
        assumptions.push(BranchAssumption {
            condition: rhs.to_owned(),
            truth,
            symbols: condition_symbols(rhs, 0, rhs.len()),
        });
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

pub(super) fn parse_simple_predicate(condition: &str) -> Option<SimplePredicate> {
    let condition = strip_outer_parentheses(condition);
    if condition == "TRUE" {
        return Some(SimplePredicate::Static(true));
    }
    if condition == "FALSE" {
        return Some(SimplePredicate::Static(false));
    }
    if let Some(symbol) = condition.strip_prefix('!').and_then(static_symbol) {
        return Some(SimplePredicate::Eq {
            symbol,
            value: PredicateValue::Logical(false),
        });
    }
    if let Some(inner) = condition
        .strip_prefix("!is.null(")
        .and_then(|rest| rest.strip_suffix(')'))
    {
        let value = strip_outer_parentheses(inner.trim());
        return (!value.is_empty()).then(|| SimplePredicate::IsNull {
            value: value.to_owned(),
            is_null: false,
        });
    }
    if let Some(inner) = condition
        .strip_prefix("is.null(")
        .and_then(|rest| rest.strip_suffix(')'))
    {
        let value = strip_outer_parentheses(inner.trim());
        return (!value.is_empty()).then(|| SimplePredicate::IsNull {
            value: value.to_owned(),
            is_null: true,
        });
    }
    if let Some((left, right)) = split_top_level_operator(condition, "==") {
        let (symbol, value) = symbol_constant_pair(left, right)?;
        return Some(SimplePredicate::Eq { symbol, value });
    }
    if let Some((left, right)) = split_top_level_operator(condition, "!=") {
        let (symbol, value) = symbol_constant_pair(left, right)?;
        return Some(SimplePredicate::Ne { symbol, value });
    }
    static_symbol(condition).map(|symbol| SimplePredicate::Eq {
        symbol,
        value: PredicateValue::Logical(true),
    })
}

pub(super) fn symbol_constant_pair(left: &str, right: &str) -> Option<(String, PredicateValue)> {
    if let (Some(symbol), Some(value)) = (static_symbol(left), predicate_constant(right)) {
        return Some((symbol, value));
    }
    if let (Some(value), Some(symbol)) = (predicate_constant(left), static_symbol(right)) {
        return Some((symbol, value));
    }
    None
}

pub(super) fn predicate_constant(value: &str) -> Option<PredicateValue> {
    if let Some(value) = static_string(value) {
        return Some(PredicateValue::String(value));
    }
    match value {
        "TRUE" => Some(PredicateValue::Logical(true)),
        "FALSE" => Some(PredicateValue::Logical(false)),
        _ if simple_numeric_literal(value) => Some(PredicateValue::Number(value.to_owned())),
        _ => None,
    }
}

pub(super) fn simple_numeric_literal(value: &str) -> bool {
    let value = value.strip_suffix('L').unwrap_or(value);
    !value.is_empty() && value.parse::<f64>().is_ok()
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

pub(super) fn canonical_condition(text: &str, start: usize, end: usize) -> String {
    let bytes = text.as_bytes();
    let mut output = String::new();
    let mut cursor = start;
    let mut quote = None;
    while cursor < end {
        let byte = bytes[cursor];
        if let Some(delimiter) = quote {
            output.push(byte as char);
            if byte == b'\\' {
                if cursor + 1 < end {
                    cursor += 1;
                    output.push(bytes[cursor] as char);
                }
            } else if byte == delimiter {
                quote = None;
            }
            cursor += 1;
            continue;
        }
        match byte {
            b'\'' | b'"' | b'`' => {
                quote = Some(byte);
                output.push(byte as char);
                cursor += 1;
            }
            b'#' => cursor = skip_comment(text, cursor, end),
            byte if byte.is_ascii_whitespace() => cursor += 1,
            _ => {
                let character = text[cursor..].chars().next().expect("valid UTF-8 source");
                output.push(character);
                cursor += character.len_utf8();
            }
        }
    }
    output
}

pub(super) fn condition_symbols(text: &str, start: usize, end: usize) -> BTreeSet<String> {
    let mut symbols = BTreeSet::new();
    let bytes = text.as_bytes();
    let mut cursor = start;
    let mut quote = None;
    while cursor < end {
        let byte = bytes[cursor];
        if let Some(delimiter) = quote {
            if byte == b'\\' {
                cursor = (cursor + 2).min(end);
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
            b'#' => cursor = skip_comment(text, cursor, end),
            _ => {
                let Some(character) = text[cursor..].chars().next() else {
                    break;
                };
                if character.is_alphabetic() || character == '.' || character == '_' {
                    let token_start = cursor;
                    cursor += character.len_utf8();
                    while cursor < end {
                        let Some(next) = text[cursor..].chars().next() else {
                            break;
                        };
                        if !(next.is_alphanumeric() || next == '.' || next == '_') {
                            break;
                        }
                        cursor += next.len_utf8();
                    }
                    if let Some(name) = text.get(token_start..cursor) {
                        symbols.insert(name.to_owned());
                    }
                } else {
                    cursor += character.len_utf8();
                }
            }
        }
    }
    symbols
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
