use super::text_offset;
use crate::package::Atom;
use crate::syntax::facts::StaticArg;
use crate::syntax::source::TextRange;
use air_r_syntax::{
    AnyRArgumentName, AnyRExpression, AnyRParameterName, AnyRValue, RBinaryExpression, RCall,
    RForStatement, RFunctionDefinition, RIdentifier, RIfStatement, RLanguage, RNamespaceExpression,
    RRoot, RStringValue, RSyntaxKind,
};
use biome_rowan::{AstNode, AstSeparatedList};
use oak_core::syntax_ext::{RIdentifierExt, RStringValueExt};
use oak_semantic::semantic_index::DefinitionKind;
use std::collections::{BTreeSet, HashMap};

pub(super) struct Census {
    pub(super) calls: Vec<RCall>,
    pub(super) expressions: Vec<AnyRExpression>,
    pub(super) namespaces: HashMap<usize, RNamespaceExpression>,
    by_callee: HashMap<(usize, usize), usize>,
    pub(super) functions: Vec<FunctionRegion>,
    pub(super) fors: Vec<ForRegion>,
    pub(super) ifs: Vec<IfRegion>,
}

#[derive(Debug, Clone)]
pub(super) struct FunctionRegion {
    pub(super) function_start: usize,
    pub(super) formals: TextRange,
    pub(super) body: TextRange,
    pub(super) parameters: BTreeSet<String>,
}

#[derive(Debug, Clone)]
pub(super) struct ForRegion {
    pub(super) variable: String,
    pub(super) variable_start: usize,
    pub(super) body: TextRange,
}

#[derive(Debug, Clone)]
pub(super) struct IfRegion {
    pub(super) if_start: usize,
    pub(super) condition: AnyRExpression,
    pub(super) then_branch: TextRange,
    pub(super) else_branch: Option<TextRange>,
}

impl Census {
    pub(super) fn of(root: &RRoot) -> Result<Self, String> {
        let mut census = Self {
            calls: Vec::new(),
            expressions: Vec::new(),
            namespaces: HashMap::new(),
            by_callee: HashMap::new(),
            functions: Vec::new(),
            fors: Vec::new(),
            ifs: Vec::new(),
        };
        for node in root.syntax().descendants() {
            if let Some(expression) = AnyRExpression::cast(node.clone()) {
                census.expressions.push(expression);
            }
            match node.kind() {
                RSyntaxKind::R_IDENTIFIER => {
                    if RIdentifier::cast(node).is_some_and(|name| identifier_name(&name).is_none())
                    {
                        return Err(
                            "escaped identifiers have no established Oak binding identity".into(),
                        );
                    }
                }
                RSyntaxKind::R_NAMESPACE_EXPRESSION => {
                    if let Some(namespace) = RNamespaceExpression::cast(node) {
                        census
                            .namespaces
                            .insert(node_range(&namespace).start, namespace);
                    }
                }
                RSyntaxKind::R_CALL => {
                    if let Some(call) = RCall::cast(node) {
                        census.add_call(call)?;
                    }
                }
                RSyntaxKind::R_FUNCTION_DEFINITION => {
                    census.functions.extend(
                        RFunctionDefinition::cast(node).and_then(|node| function_region(&node)),
                    );
                }
                RSyntaxKind::R_FOR_STATEMENT => {
                    census
                        .fors
                        .extend(RForStatement::cast(node).and_then(|node| for_region(&node)));
                }
                RSyntaxKind::R_IF_STATEMENT => {
                    census.ifs.extend(
                        RIfStatement::cast(node).and_then(|statement| if_region(&statement)),
                    );
                }
                _ => {}
            }
        }
        Ok(census)
    }

    fn add_call(&mut self, call: RCall) -> Result<(), String> {
        let arguments = call.arguments().map_err(|error| error.to_string())?;
        for argument in arguments.items().iter() {
            let argument = argument.map_err(|error| error.to_string())?;
            if let Some(clause) = argument.name_clause() {
                let name = clause.name().map_err(|error| error.to_string())?;
                if argument_name(&name).is_none() {
                    return Err("argument name has no established literal value".into());
                }
            }
        }
        if let Ok(function) = call.function() {
            let range = node_range(&function);
            self.by_callee
                .entry((range.start, range.end))
                .or_insert(self.calls.len());
        }
        self.calls.push(call);
        Ok(())
    }

    pub(super) fn call_of(&self, callee_start: usize, callee_end: usize) -> Option<&RCall> {
        let call = self
            .calls
            .get(*self.by_callee.get(&(callee_start, callee_end))?)?;
        Some(call)
    }

    pub(super) fn is_callee(&self, start: usize, end: usize) -> bool {
        self.by_callee.contains_key(&(start, end))
    }

    pub(super) fn is_replacement_target(&self, callee_start: usize, callee_end: usize) -> bool {
        let Some(call) = self
            .by_callee
            .get(&(callee_start, callee_end))
            .and_then(|index| self.calls.get(*index))
        else {
            return false;
        };
        call.syntax()
            .parent()
            .and_then(RBinaryExpression::cast)
            .is_some_and(|binary| {
                binary
                    .left()
                    .is_ok_and(|left| left.syntax() == call.syntax())
                    && binary
                        .operator()
                        .is_ok_and(|operator| matches!(operator.text_trimmed(), "<-" | "<<-"))
            })
    }
}

pub(super) struct Assignment {
    pub(super) value: AnyRExpression,
    pub(super) value_first: bool,
}

pub(super) fn assignment_of(root: &RRoot, kind: &DefinitionKind) -> Option<Assignment> {
    let (DefinitionKind::Assignment(pointer) | DefinitionKind::SuperAssignment(pointer)) = kind
    else {
        return None;
    };
    let binary = pointer.to_node(root.syntax());
    let value_first = matches!(
        binary.operator().ok()?.kind(),
        RSyntaxKind::ASSIGN_RIGHT | RSyntaxKind::SUPER_ASSIGN_RIGHT
    );
    let value = if value_first {
        binary.left()
    } else {
        binary.right()
    }
    .ok()?;
    Some(Assignment { value, value_first })
}

pub(super) fn node_range(node: &impl AstNode<Language = RLanguage>) -> TextRange {
    let range = node.syntax().text_trimmed_range();
    TextRange::new(text_offset(range.start()), text_offset(range.end()))
}

pub(super) fn argument_name(name: &AnyRArgumentName) -> Option<String> {
    match name {
        AnyRArgumentName::RIdentifier(identifier) => identifier_name(identifier),
        AnyRArgumentName::RDots(_) => Some("...".to_owned()),
        AnyRArgumentName::RDotDotI(index) => Some(index.syntax().text_trimmed().to_string()),
        AnyRArgumentName::RStringValue(value) => string_value(value),
        AnyRArgumentName::RNullExpression(_) => Some("NULL".into()),
    }
}

pub(super) fn static_arg_of(expression: &AnyRExpression) -> Option<StaticArg> {
    match expression {
        AnyRExpression::RTrueExpression(_) => Some(StaticArg::Logical(true)),
        AnyRExpression::RFalseExpression(_) => Some(StaticArg::Logical(false)),
        AnyRExpression::AnyRValue(AnyRValue::RStringValue(value)) => {
            string_value(value).map(|string| StaticArg::String(Atom::from(string)))
        }
        AnyRExpression::RIdentifier(node) => {
            identifier_name(node).map(|name| StaticArg::Symbol(name.into()))
        }
        AnyRExpression::RDots(_) => Some(StaticArg::Symbol("...".into())),
        AnyRExpression::RDotDotI(node) => Some(StaticArg::Symbol(
            node.syntax().text_trimmed().to_string().into(),
        )),
        _ => None,
    }
}

pub(super) fn identifier_name(identifier: &RIdentifier) -> Option<String> {
    let name = identifier.name_text();
    (!name.contains(['\\', '`'])).then_some(name)
}

// Air has already lexed and validated this entire token. Only literal value
// decoding belongs here; expression boundaries and names come from AST nodes.
fn string_value(value: &RStringValue) -> Option<String> {
    let token = value.value_token().ok()?;
    let text = token.text_trimmed();
    if text.starts_with(['r', 'R']) {
        let open = text.find(['(', '[', '{'])?;
        return text
            .get(open + 1..text.len().checked_sub(open)?)
            .map(str::to_owned);
    }
    let content = value.string_text()?;
    let mut characters = content.chars();
    let mut decoded = String::new();
    while let Some(character) = characters.next() {
        decoded.push(if character == '\\' {
            match characters.next()? {
                '\\' => '\\',
                '\'' => '\'',
                '"' => '"',
                'n' => '\n',
                'r' => '\r',
                't' => '\t',
                'b' => '\u{0008}',
                'f' => '\u{000c}',
                'a' => '\u{0007}',
                'v' => '\u{000b}',
                _ => return None,
            }
        } else {
            character
        });
    }
    Some(decoded)
}

pub(super) fn node_name(node: &impl AstNode<Language = RLanguage>) -> Option<String> {
    match AnyRExpression::cast(node.syntax().clone())? {
        AnyRExpression::RIdentifier(identifier) => identifier_name(&identifier),
        AnyRExpression::AnyRValue(AnyRValue::RStringValue(value)) => string_value(&value),
        _ => None,
    }
}

fn function_region(function: &RFunctionDefinition) -> Option<FunctionRegion> {
    let parameters = function.parameters().ok()?;
    let open = node_range_of_token(&parameters.l_paren_token().ok()?);
    let close = node_range_of_token(&parameters.r_paren_token().ok()?);
    let body = function.body().ok()?;
    let names = parameters
        .items()
        .iter()
        .filter_map(std::result::Result::ok)
        .filter_map(|parameter| parameter.name().ok())
        .filter_map(|name| parameter_name(&name))
        .collect::<BTreeSet<_>>();
    Some(FunctionRegion {
        function_start: node_range(function).start,
        formals: TextRange::new(open.end, close.start),
        body: node_range(&body),
        parameters: names,
    })
}

pub(super) fn parameter_name(name: &AnyRParameterName) -> Option<String> {
    match name {
        AnyRParameterName::RIdentifier(identifier) => identifier_name(identifier),
        AnyRParameterName::RDots(_) => Some("...".to_owned()),
        AnyRParameterName::RDotDotI(node) => Some(node.syntax().text_trimmed().to_string()),
    }
}

fn for_region(statement: &RForStatement) -> Option<ForRegion> {
    let variable = statement.variable().ok()?;
    let body = statement.body().ok()?;
    Some(ForRegion {
        variable: identifier_name(&variable)?,
        variable_start: node_range(&variable).start,
        body: node_range(&body),
    })
}

fn if_region(statement: &RIfStatement) -> Option<IfRegion> {
    let then_branch = node_range(&statement.consequence().ok()?);
    let else_branch = statement
        .else_clause()
        .and_then(|clause| clause.alternative().ok())
        .map(|alternative| node_range(&alternative));
    Some(IfRegion {
        if_start: node_range(statement).start,
        condition: statement.condition().ok()?,
        then_branch,
        else_branch,
    })
}

fn node_range_of_token(token: &air_r_syntax::RSyntaxToken) -> TextRange {
    let range = token.text_trimmed_range();
    TextRange::new(text_offset(range.start()), text_offset(range.end()))
}

#[cfg(test)]
#[path = "../../../tests/unit/syntax/oak/census.rs"]
mod tests;
