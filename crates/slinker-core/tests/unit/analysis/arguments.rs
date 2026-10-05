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
