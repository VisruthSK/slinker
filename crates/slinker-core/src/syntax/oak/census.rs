use super::scan::{
    ForRegion, FunctionRegion, IfRegion, RawArgument, RawCall, static_string, static_symbol,
};
use super::text_offset;
use crate::package::Atom;
use crate::syntax::facts::StaticArg;
use crate::syntax::source::TextRange;
use air_r_syntax::{
    AnyRArgumentName, AnyRExpression, AnyRParameterName, AnyRValue, RArgument, RBinaryExpression,
    RCall, RForStatement, RFunctionDefinition, RIfStatement, RLanguage, RRoot, RSyntaxKind,
};
use biome_rowan::{AstNode, AstSeparatedList};
use std::collections::{BTreeSet, HashMap};

pub(super) struct Census {
    calls: Vec<RCall>,
    by_callee: HashMap<(usize, usize), usize>,
    pub(super) functions: Vec<FunctionRegion>,
    pub(super) fors: Vec<ForRegion>,
    pub(super) ifs: Vec<IfRegion>,
}

impl Census {
    pub(super) fn of(root: &RRoot) -> Self {
        let mut census = Self {
            calls: Vec::new(),
            by_callee: HashMap::new(),
            functions: Vec::new(),
            fors: Vec::new(),
            ifs: Vec::new(),
        };
        for node in root.syntax().descendants() {
            match node.kind() {
                RSyntaxKind::R_CALL => {
                    if let Some(call) = RCall::cast(node) {
                        census.add_call(call);
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
                    census
                        .ifs
                        .extend(RIfStatement::cast(node).and_then(|node| if_region(&node)));
                }
                _ => {}
            }
        }
        census
    }

    fn add_call(&mut self, call: RCall) {
        if let Ok(function) = call.function() {
            let range = node_range(&function);
            self.by_callee
                .entry((range.start, range.end))
                .or_insert(self.calls.len());
        }
        self.calls.push(call);
    }

    pub(super) fn call_of(&self, callee_start: usize, callee_end: usize) -> Option<RawCall> {
        let call = self
            .calls
            .get(*self.by_callee.get(&(callee_start, callee_end))?)?;
        raw_call(call)
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

pub(super) fn node_range(node: &impl AstNode<Language = RLanguage>) -> TextRange {
    let range = node.syntax().text_trimmed_range();
    TextRange::new(text_offset(range.start()), text_offset(range.end()))
}

pub(super) fn raw_call(call: &RCall) -> Option<RawCall> {
    let range = node_range(call);
    let mut args = call
        .arguments()
        .ok()?
        .items()
        .iter()
        .filter_map(std::result::Result::ok)
        .map(|argument| raw_argument(&argument))
        .collect::<Vec<_>>();
    if args.last().is_some_and(RawArgument::is_hole) {
        args.pop();
    }
    Some(RawCall {
        start: range.start,
        end: range.end,
        args,
    })
}

fn raw_argument(argument: &RArgument) -> RawArgument {
    let name = argument
        .name_clause()
        .and_then(|clause| clause.name().ok())
        .and_then(|name| argument_name(&name));
    let value = argument.value();
    RawArgument {
        name,
        value: value.as_ref().map_or_else(
            || {
                let end = node_range(argument).end;
                TextRange::new(end, end)
            },
            node_range,
        ),
        static_arg: value.as_ref().and_then(static_arg_of),
        logical: value.as_ref().and_then(logical_literal),
    }
}

fn argument_name(name: &AnyRArgumentName) -> Option<String> {
    match name {
        AnyRArgumentName::RIdentifier(identifier) => {
            static_symbol(identifier.syntax().text_trimmed().to_string().as_str())
        }
        AnyRArgumentName::RDots(_) => Some("...".to_owned()),
        AnyRArgumentName::RDotDotI(index) => Some(index.syntax().text_trimmed().to_string()),
        AnyRArgumentName::RStringValue(value) => {
            static_string(&value.syntax().text_trimmed().to_string())
        }
        AnyRArgumentName::RNullExpression(_) => None,
    }
}

pub(super) fn static_arg_of(expression: &AnyRExpression) -> Option<StaticArg> {
    match expression {
        AnyRExpression::AnyRValue(AnyRValue::RStringValue(value)) => {
            static_string(&value.syntax().text_trimmed().to_string())
                .map(|string| StaticArg::String(Atom::from(string)))
        }
        AnyRExpression::RIdentifier(node) => symbol_arg(&node.syntax().text_trimmed().to_string()),
        AnyRExpression::RDots(node) => symbol_arg(&node.syntax().text_trimmed().to_string()),
        AnyRExpression::RDotDotI(node) => symbol_arg(&node.syntax().text_trimmed().to_string()),
        _ => None,
    }
}

fn logical_literal(expression: &AnyRExpression) -> Option<bool> {
    match expression {
        AnyRExpression::RTrueExpression(_) => Some(true),
        AnyRExpression::RFalseExpression(_) => Some(false),
        _ => None,
    }
}

fn symbol_arg(text: &str) -> Option<StaticArg> {
    static_symbol(text).map(|symbol| StaticArg::Symbol(Atom::from(symbol)))
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
        .filter_map(|name| match name {
            AnyRParameterName::RIdentifier(node) => {
                static_symbol(&node.syntax().text_trimmed().to_string())
            }
            AnyRParameterName::RDots(_) => Some("...".to_owned()),
            AnyRParameterName::RDotDotI(node) => Some(node.syntax().text_trimmed().to_string()),
        })
        .collect::<BTreeSet<_>>();
    Some(FunctionRegion {
        function_start: node_range(function).start,
        formals: TextRange::new(open.end, close.start),
        body: node_range(&body),
        parameters: names,
    })
}

fn for_region(statement: &RForStatement) -> Option<ForRegion> {
    let variable = statement.variable().ok()?;
    let body = statement.body().ok()?;
    Some(ForRegion {
        variable: static_symbol(&variable.syntax().text_trimmed().to_string())?,
        variable_start: node_range(&variable).start,
        body: node_range(&body),
    })
}

fn if_region(statement: &RIfStatement) -> Option<IfRegion> {
    let open = node_range_of_token(&statement.l_paren_token().ok()?);
    let close = node_range_of_token(&statement.r_paren_token().ok()?);
    let then_branch = node_range(&statement.consequence().ok()?);
    let else_branch = statement
        .else_clause()
        .and_then(|clause| clause.alternative().ok())
        .map(|alternative| node_range(&alternative));
    Some(IfRegion {
        if_start: node_range(statement).start,
        condition: TextRange::new(open.end, close.start),
        then_branch,
        else_branch,
    })
}

fn node_range_of_token(token: &air_r_syntax::RSyntaxToken) -> TextRange {
    let range = token.text_trimmed_range();
    TextRange::new(text_offset(range.start()), text_offset(range.end()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use air_r_parser::{RParserOptions, parse};

    fn raw_calls(text: &str) -> Vec<RawCall> {
        let parsed = parse(text, RParserOptions::default());
        parsed
            .tree()
            .syntax()
            .descendants()
            .filter_map(RCall::cast)
            .filter_map(|call| raw_call(&call))
            .collect()
    }

    #[test]
    fn trailing_comma_adds_no_argument_but_leading_hole_does() {
        assert_eq!(raw_calls("f(x,)")[0].args.len(), 1);
        assert_eq!(raw_calls("f(,)")[0].args.len(), 1);
        assert_eq!(raw_calls("f(,x)")[0].args.len(), 2);
        assert_eq!(raw_calls("f(x,,y)")[0].args.len(), 3);
        assert!(raw_calls("f()")[0].args.is_empty());
    }

    #[test]
    fn argument_names_come_from_name_clauses() {
        let call = &raw_calls("f(a = 1, \"b c\" = 2, `d` = 3, x == y)")[0];
        let names = call
            .args
            .iter()
            .map(|argument| argument.name.as_deref())
            .collect::<Vec<_>>();
        assert_eq!(names, [Some("a"), Some("b c"), Some("d"), None]);
    }

    #[test]
    fn language_constants_are_not_symbol_arguments() {
        let call = &raw_calls("f(TRUE, NULL, T, ..., \"s\")")[0];
        let values = call
            .args
            .iter()
            .map(|argument| argument.static_arg.clone())
            .collect::<Vec<_>>();
        assert_eq!(
            values,
            [
                None,
                None,
                Some(StaticArg::Symbol("T".into())),
                Some(StaticArg::Symbol("...".into())),
                Some(StaticArg::String("s".into())),
            ]
        );
    }

    #[test]
    fn regions_are_the_syntax_nodes_not_scanned_text() {
        let text = "f <- function(a, b = 2) {\n  if (a) 1 else 2\n  for (i in b) i\n  \\(z) z\n}\n";
        let parsed = parse(text, RParserOptions::default());
        let census = Census::of(&parsed.tree());
        let slice = |range: TextRange| &text[range.start..range.end];

        assert_eq!(census.functions.len(), 2);
        assert_eq!(
            census.functions[0].parameters,
            BTreeSet::from(["a".to_owned(), "b".to_owned()])
        );
        assert_eq!(slice(census.functions[0].formals), "a, b = 2");
        assert_eq!(
            census.functions[1].parameters,
            BTreeSet::from(["z".to_owned()])
        );

        let branch = &census.ifs[0];
        assert_eq!(slice(branch.condition), "a");
        assert_eq!(slice(branch.then_branch), "1");
        assert_eq!(branch.else_branch.map(slice), Some("2"));

        let loop_region = &census.fors[0];
        assert_eq!(loop_region.variable, "i");
        assert_eq!(slice(loop_region.body), "i");
    }

    #[test]
    fn replacement_targets_are_calls_on_the_left_of_an_assignment() {
        let text = "names(x) <- v\ny <- names(x)\n";
        let parsed = parse(text, RParserOptions::default());
        let census = Census::of(&parsed.tree());
        assert!(census.is_replacement_target(0, 5));
        let second = text.rfind("names").unwrap();
        assert!(!census.is_replacement_target(second, second + 5));
    }
}
