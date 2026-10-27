use super::*;

#[test]
fn relations_use_typed_description_accessors() {
    let description = Description::parse(
        "Package: example\nVersion: 1.0\nImports: cli (>= 3.0), glue\nSuggests: testthat\n",
    );

    let imports = relations(&description, RelationField::Imports).unwrap();
    assert_eq!(imports.len(), 2);
    assert_eq!(imports[0].package(), "cli");
    assert_eq!(imports[1].package(), "glue");

    let suggests = relations(&description, RelationField::Suggests).unwrap();
    assert_eq!(suggests.len(), 1);
    assert_eq!(suggests[0].package(), "testthat");
}

fn requirements(values: &[&str]) -> Vec<Relation> {
    values
        .iter()
        .map(|value| value.parse().expect("relation"))
        .collect()
}

fn rendered(package: &str, values: &[&str]) -> Result<Vec<String>, String> {
    intersect_requirements(package, &requirements(values))
        .map(|merged| merged.iter().map(Relation::to_string).collect())
}

#[test]
fn requirement_intersection_keeps_the_strongest_bounds() {
    assert_eq!(
        rendered(
            "dplyr",
            &[
                "dplyr (>= 1.0.0)",
                "dplyr",
                "dplyr (>= 1.1.0)",
                "dplyr (< 2.0.0)"
            ]
        ),
        Ok(vec![
            "dplyr (>= 1.1.0)".to_owned(),
            "dplyr (< 2.0.0)".to_owned()
        ])
    );
    assert_eq!(
        rendered("cli", &["cli (>= 3.0)", "cli (> 3.0)"]),
        Ok(vec!["cli (> 3.0)".to_owned()])
    );
    assert_eq!(rendered("glue", &["glue"]), Ok(vec!["glue".to_owned()]));
}

#[test]
fn requirement_intersection_collapses_exact_versions_and_prunes_exclusions() {
    assert_eq!(
        rendered("rlang", &["rlang (>= 1.0)", "rlang (== 1.1.0)"]),
        Ok(vec!["rlang (== 1.1.0)".to_owned()])
    );
    assert_eq!(
        rendered(
            "rlang",
            &["rlang (>= 1.0)", "rlang (!= 0.9)", "rlang (!= 1.2)"]
        ),
        Ok(vec![
            "rlang (>= 1.0)".to_owned(),
            "rlang (!= 1.2)".to_owned()
        ])
    );
}

#[test]
fn incompatible_requirements_are_rejected_deterministically() {
    for values in [
        &["dplyr (>= 2.0)", "dplyr (< 1.5)"][..],
        &["dplyr (>= 1.0)", "dplyr (< 1.0)"],
        &["dplyr (== 1.0)", "dplyr (== 1.1)"],
        &["dplyr (== 1.0)", "dplyr (!= 1.0)"],
        &["dplyr (>= 1.0)", "dplyr (<= 1.0)", "dplyr (!= 1.0)"],
    ] {
        assert!(rendered("dplyr", values).is_err(), "{values:?}");
    }
    assert_eq!(
        rendered("dplyr", &["dplyr (>= 1.0)", "dplyr (<= 1.0)"]),
        Ok(vec![
            "dplyr (>= 1.0)".to_owned(),
            "dplyr (<= 1.0)".to_owned()
        ])
    );
}

#[test]
fn malformed_relation_field_is_not_partially_accepted() {
    let description =
        Description::parse("Package: example\nVersion: 1.0\nImports: cli, broken (=> 1.0), glue\n");

    let error = relations(&description, RelationField::Imports).unwrap_err();
    assert!(error.starts_with("invalid Imports field:"));
}
