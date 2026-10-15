use super::*;
use crate::syntax::{OakParser, SourceKey, Sources};

fn call(text: &str) -> CallSite {
    let mut sources = Sources::default();
    let source = sources.add("root", SourceKey::Binding("run".into()), text);
    let parsed = OakParser.parse_binding(source, text).unwrap();
    parsed.expressions[0]
        .calls
        .iter()
        .find(|call| call.callee == "f")
        .unwrap()
        .clone()
}

#[test]
fn dots_prevent_partial_and_positional_matching_to_later_formals() {
    let call = call("run <- function() f(1L, n='right')");
    assert_eq!(matched_arg_index(&call, &["...", "nm"], "nm"), None);
}

#[test]
fn exact_names_after_dots_still_match() {
    let call = call("run <- function() f(1L, nm='right')");
    assert_eq!(matched_arg_index(&call, &["...", "nm"], "nm"), Some(1));
}

#[test]
fn checked_matching_preserves_r_errors_and_exact_match_precedence() {
    let formals = ["alpha", "alpine", "...", "tail"];
    for (text, expected) in [
        (
            "run <- function() f(alph=1, alph=2)",
            "formal argument `alpha` matched by multiple actual arguments",
        ),
        (
            "run <- function() f(al=1)",
            "argument `al` matches multiple formal arguments",
        ),
    ] {
        let call = call(text);
        let result = match_arguments(
            call.arguments
                .iter()
                .map(|argument| argument.name.as_deref()),
            &formals,
            MatchScope::Signature,
        );
        assert_eq!(result.err().unwrap().to_string(), expected);
    }
    let call = call("run <- function() f(alph=1, alpha=2, tail=3)");
    let matched = match_arguments(
        call.arguments
            .iter()
            .map(|argument| argument.name.as_deref()),
        &formals,
        MatchScope::Signature,
    )
    .unwrap();
    assert_eq!(matched.argument(0), Some(1));
    assert_eq!(matched.argument(3), Some(2));
}
