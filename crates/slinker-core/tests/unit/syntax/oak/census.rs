use super::*;
use air_r_parser::{RParserOptions, parse};

fn calls(text: &str) -> Vec<crate::syntax::CallSite> {
    super::super::OakParser
        .parse_binding(crate::syntax::SourceId(0), text)
        .unwrap()
        .expressions
        .into_iter()
        .next()
        .unwrap()
        .calls
}

#[test]
fn trailing_comma_adds_no_argument_but_leading_hole_does() {
    assert_eq!(calls("f(x,)")[0].arguments.len(), 1);
    assert_eq!(calls("f(,)")[0].arguments.len(), 1);
    assert_eq!(calls("f(,x)")[0].arguments.len(), 2);
    assert_eq!(calls("f(x,,y)")[0].arguments.len(), 3);
    assert!(calls("f()")[0].arguments.is_empty());
}

#[test]
fn argument_names_come_from_name_clauses() {
    let call = &calls("f(a = 1, \"b c\" = 2, `d` = 3, NULL = 4, x == y)")[0];
    let names = call
        .arguments
        .iter()
        .map(|argument| argument.name.as_deref())
        .collect::<Vec<_>>();
    assert_eq!(
        names,
        [Some("a"), Some("b c"), Some("d"), Some("NULL"), None]
    );
}

#[test]
fn language_constants_are_not_symbol_arguments() {
    let call = &calls("f(TRUE, NULL, T, ..., \"s\")")[0];
    let values = call
        .arguments
        .iter()
        .map(|argument| argument.value.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        values,
        [
            Some(StaticArg::Logical(true)),
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
    let census = Census::of(&parsed.tree()).unwrap();
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
    assert_eq!(slice(node_range(&branch.condition)), "a");
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
    let census = Census::of(&parsed.tree()).unwrap();
    assert!(census.is_replacement_target(0, 5));
    let second = text.rfind("names").unwrap();
    assert!(!census.is_replacement_target(second, second + 5));
}
