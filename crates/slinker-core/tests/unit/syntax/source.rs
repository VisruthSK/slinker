use super::*;

#[test]
fn location_reports_one_based_line_and_column_within_the_binding_source() {
    let mut sources = Sources::default();
    let id = sources.add(
        "pkg",
        SourceKey::Binding("f".into()),
        "function() {\n  g(é, h())\n}",
    );
    let start = "function() {\n  g(é, ".len();

    let location = sources
        .location(&Span::new(id, start, start + 3))
        .expect("span lies inside the source");

    assert_eq!(location.to_string(), "pkg::f:2:8");
}
