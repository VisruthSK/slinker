use super::*;

fn relation(value: &str) -> Relation {
    value.parse().expect("relation")
}

#[test]
fn linked_dependencies_leave_and_external_contracts_replace_imports() {
    let source = "Package: root\nVersion: 1.0.0\nImports:\n    linked,\n    dplyr (>= 1.0.0)\nSuggests: linked, testthat\nCollate: 'b.R' 'a.R'\nRepository: CRAN\nLicense: MIT\n";

    let generated = generated_description(
        source,
        |package| package == "linked",
        &[relation("dplyr (>= 1.1.0)")],
    )
    .expect("supported transformation");
    let parsed = Description::parse(&generated);

    let imports = relations(&parsed, RelationField::Imports).expect("imports");
    let suggests = relations(&parsed, RelationField::Suggests).expect("suggests");
    assert_eq!(imports, [relation("dplyr (>= 1.1.0)")]);
    assert_eq!(suggests, [relation("testthat")]);
    assert!(parsed.field("Collate").is_none());
    assert!(parsed.field("Repository").is_none());
    assert!(generated.contains("License: MIT"));
}

#[test]
fn untouched_fields_keep_their_original_text() {
    let source = "Package: root\nVersion: 1.0.0\nDescription: A long\n    wrapped description.\nSuggests: testthat\n";

    let generated =
        generated_description(source, |_| false, &[]).expect("supported transformation");

    assert_eq!(generated, source);
}

#[test]
fn attachment_and_native_links_to_linked_packages_block() {
    let source = "Package: root\nVersion: 1.0.0\nDepends: R (>= 4.0), linked\nLinkingTo: native\n";

    let problems = generated_description(
        source,
        |package| matches!(package, "linked" | "native"),
        &[],
    )
    .expect_err("unsupported transformation");

    assert_eq!(
        problems,
        [
            "Depends names Linked package `linked`",
            "LinkingTo names Linked package `native`"
        ]
    );
}
