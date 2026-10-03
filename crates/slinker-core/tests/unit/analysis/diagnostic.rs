use super::*;

fn primary(sites: &[(&str, Option<&str>)]) -> Diagnostic {
    Diagnostic {
        package: "gone".into(),
        binding: None,
        code: RejectCode::MissingDependency,
        message: String::new(),
        span: None,
        node: NodeId(0),
        evidence: sites
            .iter()
            .map(|(package, binding)| Evidence {
                package: (*package).into(),
                binding: binding.map(BindingName::from),
                span: None,
                detail: String::new(),
            })
            .collect(),
    }
}

#[test]
fn evidence_summary_names_distinct_sites_and_counts_the_rest() {
    assert_eq!(primary(&[]).evidence_summary(), None);
    assert_eq!(
        primary(&[("a", Some("f")), ("a", Some("f")), ("b", None)])
            .evidence_summary()
            .as_deref(),
        Some("a::f, b")
    );
    let many = (0..7)
        .map(|index| ("p", Some(["a", "b", "c", "d", "e", "f", "g"][index])))
        .collect::<Vec<_>>();
    assert_eq!(
        primary(&many).evidence_summary().as_deref(),
        Some("p::a, p::b, p::c, p::d and 3 more")
    );
}
