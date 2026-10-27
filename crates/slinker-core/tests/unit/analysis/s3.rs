use super::*;
use crate::syntax::{OakParser, SourceId};

#[test]
fn partial_selector_matching_uses_the_named_argument_class() {
    let parsed = OakParser
        .parse_binding(
            SourceId(0),
            "run <- function(x, y) { declare(slinker(x=s3('a'), y=s3('b'))); gen(ob=y, x) }",
        )
        .unwrap();
    let call = parsed.expressions[0]
        .calls
        .iter()
        .find(|call| call.callee == "gen")
        .unwrap();
    let invocation = Invocation::from_call(&parsed, call);
    let definition = GenericDefinition {
        callable: CallableId {
            package: PackageId::from_index(0),
            binding: "gen".into(),
        },
        selector: "object".into(),
        formals: vec!["object".into(), "other".into()],
    };
    assert_eq!(
        selector_domain(&invocation, &definition),
        Some(vec![vec!["b".into()]])
    );
}
