use super::scan::static_arg;
use super::{LiveCall, ast_atom, ast_span, ast_text};
use crate::package::Atom;
use crate::syntax::facts::{
    CalleeKind, ConstructionArgument, ConstructionCall, ConstructionExpr, ConstructionExprKind,
    ConstructionTarget, StaticArg,
};
use crate::syntax::source::{SourceId, Span};
use air_r_syntax::{
    AnyRExpression, RBinaryExpression, RCall, RFunctionDefinition, RIfStatement, RRoot,
};
use biome_rowan::{AstNode, AstNodeList, AstSeparatedList};
use std::sync::Arc;

pub(super) fn collect_construction(
    source: SourceId,
    text: &str,
    root: &RRoot,
    calls: &[LiveCall],
) -> (Vec<String>, Vec<ConstructionExpr>) {
    let Some(function) = root
        .expressions()
        .iter()
        .find_map(|expression| outer_function(&expression))
    else {
        return (Vec::new(), Vec::new());
    };
    let parameters = function
        .parameters()
        .ok()
        .into_iter()
        .flat_map(|parameters| parameters.items().iter().collect::<Vec<_>>())
        .filter_map(std::result::Result::ok)
        .filter_map(|parameter| parameter.name().ok())
        .map(|name| ast_text(text, &name))
        .collect();
    let construction = function
        .body()
        .ok()
        .map(|body| construction_statements(&source, text, body, calls))
        .unwrap_or_default();
    (parameters, construction)
}

pub(super) fn outer_function(
    expression: &AnyRExpression,
) -> Option<air_r_syntax::RFunctionDefinition> {
    match expression {
        AnyRExpression::RFunctionDefinition(function) => Some(function.clone()),
        AnyRExpression::RBinaryExpression(binary)
            if binary
                .operator()
                .is_ok_and(|operator| operator.text_trimmed() == "<-") =>
        {
            match binary.right().ok()? {
                AnyRExpression::RFunctionDefinition(function) => Some(function),
                _ => None,
            }
        }
        _ => None,
    }
}

pub(super) fn construction_statements(
    source: &SourceId,
    text: &str,
    expression: AnyRExpression,
    calls: &[LiveCall],
) -> Vec<ConstructionExpr> {
    match expression {
        AnyRExpression::RBracedExpressions(block) => block
            .expressions()
            .iter()
            .filter_map(|expression| construction_expr(source, text, expression, calls))
            .collect(),
        expression => construction_expr(source, text, expression, calls)
            .into_iter()
            .collect(),
    }
}

pub(super) fn construction_expr(
    source: &SourceId,
    text: &str,
    expression: AnyRExpression,
    calls: &[LiveCall],
) -> Option<ConstructionExpr> {
    let span = ast_span(source, &expression);
    let kind = match expression {
        AnyRExpression::RNullExpression(_) => ConstructionExprKind::Null,
        AnyRExpression::RTrueExpression(_) => ConstructionExprKind::Logical { value: true },
        AnyRExpression::RFalseExpression(_) => ConstructionExprKind::Logical { value: false },
        AnyRExpression::RIdentifier(identifier) => ConstructionExprKind::Symbol {
            name: Atom::from(identifier.name_token().ok()?.text_trimmed()),
        },
        AnyRExpression::AnyRValue(_) => construction_value(text.get(span.start..span.end)?.trim()),
        AnyRExpression::RBracedExpressions(block) => ConstructionExprKind::Sequence {
            expressions: block
                .expressions()
                .iter()
                .filter_map(|expression| construction_expr(source, text, expression, calls))
                .collect(),
        },
        AnyRExpression::RParenthesizedExpression(parenthesized) => {
            return construction_expr(source, text, parenthesized.body().ok()?, calls);
        }
        AnyRExpression::RBinaryExpression(binary) => {
            construction_binary(source, text, &binary, calls)?
        }
        AnyRExpression::RCall(call) => {
            return construction_call(source, text, &call, span, calls);
        }
        AnyRExpression::RExtractExpression(extract) => ConstructionExprKind::Member {
            object: Arc::new(construction_expr(
                source,
                text,
                extract.left().ok()?,
                calls,
            )?),
            name: extract.right().ok().map(|name| ast_atom(text, &name)),
        },
        AnyRExpression::RSubset2(subset) => {
            let index = subset
                .arguments()
                .ok()?
                .items()
                .iter()
                .find_map(std::result::Result::ok)?
                .value()?;
            construction_index(source, text, subset.function().ok()?, index, calls)?
        }
        AnyRExpression::RSubset(subset) => {
            let index = subset
                .arguments()
                .ok()?
                .items()
                .iter()
                .find_map(std::result::Result::ok)?
                .value()?;
            construction_index(source, text, subset.function().ok()?, index, calls)?
        }
        AnyRExpression::RUnaryExpression(unary) => ConstructionExprKind::Call {
            call: Arc::new(ConstructionCall {
                callee: Atom::from(unary.operator().ok()?.text_trimmed()),
                callee_kind: CalleeKind::DefinitelyExternal,
                qualified_package: Some("base".into()),
                arguments: Arc::from([ConstructionArgument {
                    name: None,
                    value: construction_expr(source, text, unary.argument().ok()?, calls),
                }]),
            }),
        },
        AnyRExpression::RIfStatement(statement) => {
            construction_if(source, text, &statement, calls)?
        }
        AnyRExpression::RFunctionDefinition(function) => {
            construction_function(source, text, &function, calls)?
        }
        _ => ConstructionExprKind::Unknown,
    };
    Some(ConstructionExpr { kind, span })
}

pub(super) fn construction_value(value: &str) -> ConstructionExprKind {
    if let Some(StaticArg::String(value)) = static_arg(value) {
        ConstructionExprKind::String { value }
    } else if let Ok(integer) = value.strip_suffix('L').unwrap_or(value).parse::<i64>() {
        ConstructionExprKind::Integer { value: integer }
    } else {
        ConstructionExprKind::Double {
            value: Atom::from(value),
        }
    }
}

pub(super) fn construction_binary(
    source: &SourceId,
    text: &str,
    binary: &RBinaryExpression,
    calls: &[LiveCall],
) -> Option<ConstructionExprKind> {
    let operator = binary.operator().ok()?.text_trimmed().to_owned();
    let left = binary.left().ok()?;
    let right = binary.right().ok()?;
    Some(if operator == "<-" || operator == "=" {
        ConstructionExprKind::Assign {
            target: construction_target(source, text, left, calls),
            value: Arc::new(construction_expr(source, text, right, calls)?),
        }
    } else {
        ConstructionExprKind::Call {
            call: Arc::new(ConstructionCall {
                qualified_package: (!operator.starts_with('%')).then(|| "base".into()),
                callee: operator.into(),
                callee_kind: CalleeKind::DefinitelyExternal,
                arguments: Arc::from([
                    ConstructionArgument {
                        name: None,
                        value: construction_expr(source, text, left, calls),
                    },
                    ConstructionArgument {
                        name: None,
                        value: construction_expr(source, text, right, calls),
                    },
                ]),
            }),
        }
    })
}

pub(super) fn construction_call(
    source: &SourceId,
    text: &str,
    call: &RCall,
    span: Span,
    calls: &[LiveCall],
) -> Option<ConstructionExpr> {
    let site = calls
        .iter()
        .find(|candidate| candidate.site.span == span)
        .map(|call| &call.site);
    let function = call.function().ok()?;
    if site.is_none() && matches!(function, AnyRExpression::RExtractExpression(_)) {
        return Some(ConstructionExpr {
            kind: ConstructionExprKind::Sequence {
                expressions: std::iter::once(function)
                    .chain(
                        call.arguments()
                            .ok()?
                            .items()
                            .iter()
                            .filter_map(|argument| argument.ok()?.value()),
                    )
                    .filter_map(|expression| construction_expr(source, text, expression, calls))
                    .collect(),
            },
            span,
        });
    }
    let callee = site.map_or_else(|| ast_atom(text, &function), |site| site.callee.clone());
    let kind = ConstructionExprKind::Call {
        call: Arc::new(ConstructionCall {
            callee,
            callee_kind: site.map_or(CalleeKind::DefinitelyLexical, |site| site.callee_kind),
            qualified_package: site.and_then(|site| site.qualified_package.clone()),
            arguments: call
                .arguments()
                .ok()?
                .items()
                .iter()
                .filter_map(std::result::Result::ok)
                .map(|argument| ConstructionArgument {
                    name: argument
                        .name_clause()
                        .and_then(|clause| clause.name().ok())
                        .map(|name| ast_atom(text, &name)),
                    value: argument
                        .value()
                        .and_then(|value| construction_expr(source, text, value, calls)),
                })
                .collect(),
        }),
    };
    Some(ConstructionExpr { kind, span })
}

pub(super) fn construction_index(
    source: &SourceId,
    text: &str,
    object: AnyRExpression,
    index: AnyRExpression,
    calls: &[LiveCall],
) -> Option<ConstructionExprKind> {
    Some(ConstructionExprKind::Index {
        object: Arc::new(construction_expr(source, text, object, calls)?),
        index: Arc::new(construction_expr(source, text, index, calls)?),
    })
}

pub(super) fn construction_if(
    source: &SourceId,
    text: &str,
    statement: &RIfStatement,
    calls: &[LiveCall],
) -> Option<ConstructionExprKind> {
    Some(ConstructionExprKind::If {
        condition: Arc::new(construction_expr(
            source,
            text,
            statement.condition().ok()?,
            calls,
        )?),
        consequence: Arc::new(construction_expr(
            source,
            text,
            statement.consequence().ok()?,
            calls,
        )?),
        alternative: statement
            .else_clause()
            .and_then(|clause| clause.alternative().ok())
            .and_then(|alternative| construction_expr(source, text, alternative, calls))
            .map(Arc::new),
    })
}

pub(super) fn construction_function(
    source: &SourceId,
    text: &str,
    function: &RFunctionDefinition,
    calls: &[LiveCall],
) -> Option<ConstructionExprKind> {
    Some(ConstructionExprKind::Function {
        parameters: function
            .parameters()
            .ok()?
            .items()
            .iter()
            .filter_map(std::result::Result::ok)
            .filter_map(|parameter| parameter.name().ok())
            .map(|name| ast_atom(text, &name))
            .collect(),
        body: Arc::new(construction_expr(
            source,
            text,
            function.body().ok()?,
            calls,
        )?),
    })
}

pub(super) fn construction_target(
    source: &SourceId,
    text: &str,
    target: AnyRExpression,
    calls: &[LiveCall],
) -> ConstructionTarget {
    match target {
        AnyRExpression::RIdentifier(identifier) => {
            identifier
                .name_token()
                .ok()
                .map_or(ConstructionTarget::Unknown, |name| {
                    ConstructionTarget::Local {
                        name: Atom::from(name.text_trimmed()),
                    }
                })
        }
        AnyRExpression::RExtractExpression(extract) => ConstructionTarget::Member {
            object: Arc::new(
                extract
                    .left()
                    .ok()
                    .and_then(|object| construction_expr(source, text, object, calls))
                    .unwrap_or_else(|| unknown_construction(source, &extract)),
            ),
            name: extract.right().ok().map(|name| ast_atom(text, &name)),
        },
        AnyRExpression::RSubset2(subset) => {
            let name = subset
                .arguments()
                .ok()
                .and_then(|arguments| {
                    arguments
                        .items()
                        .iter()
                        .filter_map(std::result::Result::ok)
                        .next()
                })
                .and_then(|argument| argument.value())
                .and_then(|value| construction_expr(source, text, value, calls))
                .and_then(|value| match value.kind {
                    ConstructionExprKind::String { value } => Some(value),
                    _ => None,
                });
            ConstructionTarget::Member {
                object: Arc::new(
                    subset
                        .function()
                        .ok()
                        .and_then(|object| construction_expr(source, text, object, calls))
                        .unwrap_or_else(|| unknown_construction(source, &subset)),
                ),
                name,
            }
        }
        AnyRExpression::RCall(call) => {
            let callee = call.function().ok().map(|callee| ast_text(text, &callee));
            if callee.as_deref() != Some("environment") {
                return ConstructionTarget::Unknown;
            }
            let closure = call
                .arguments()
                .ok()
                .and_then(|arguments| {
                    arguments
                        .items()
                        .iter()
                        .filter_map(std::result::Result::ok)
                        .next()
                })
                .and_then(|argument| argument.value())
                .and_then(|closure| construction_expr(source, text, closure, calls));
            closure.map_or(ConstructionTarget::Unknown, |closure| {
                ConstructionTarget::ClosureEnvironment {
                    closure: Arc::new(closure),
                }
            })
        }
        _ => ConstructionTarget::Unknown,
    }
}

pub(super) fn unknown_construction(
    source: &SourceId,
    node: &impl AstNode<Language = air_r_syntax::RLanguage>,
) -> ConstructionExpr {
    ConstructionExpr {
        kind: ConstructionExprKind::Unknown,
        span: ast_span(source, node),
    }
}
