pub use r_description::Description;
pub use r_metadata::{Priority, Relation, RequirementVersion, Version, VersionRequirement};

use std::collections::BTreeSet;

pub fn intersect_requirements(
    package: &str,
    requirements: &[Relation],
) -> Result<Vec<Relation>, String> {
    let conflict = || {
        let listed = requirements
            .iter()
            .map(Relation::to_string)
            .collect::<Vec<_>>()
            .join(", ");
        format!("no version of `{package}` satisfies every requirement: {listed}")
    };
    let mut lower: Option<(Version, bool)> = None;
    let mut upper: Option<(Version, bool)> = None;
    let mut exact: Option<Version> = None;
    let mut excluded = BTreeSet::new();
    for relation in requirements {
        let version = |required: &RequirementVersion| match required {
            RequirementVersion::Version(version) => Ok(version.clone()),
            RequirementVersion::Revision(_) => Err(format!(
                "`{relation}` constrains an R source revision, not a package version"
            )),
        };
        match relation.requirement() {
            VersionRequirement::Any => {}
            VersionRequirement::GreaterThanEqual(required) => {
                tighten(
                    &mut lower,
                    version(required)?,
                    true,
                    std::cmp::Ordering::Greater,
                );
            }
            VersionRequirement::GreaterThan(required) => {
                tighten(
                    &mut lower,
                    version(required)?,
                    false,
                    std::cmp::Ordering::Greater,
                );
            }
            VersionRequirement::LessThanEqual(required) => {
                tighten(
                    &mut upper,
                    version(required)?,
                    true,
                    std::cmp::Ordering::Less,
                );
            }
            VersionRequirement::LessThan(required) => {
                tighten(
                    &mut upper,
                    version(required)?,
                    false,
                    std::cmp::Ordering::Less,
                );
            }
            VersionRequirement::Equal(required) => {
                let required = version(required)?;
                if exact
                    .replace(required.clone())
                    .is_some_and(|known| known != required)
                {
                    return Err(conflict());
                }
            }
            VersionRequirement::NotEqual(required) => {
                excluded.insert(version(required)?);
            }
        }
    }
    let admits = |candidate: &Version| {
        admits_bounds(candidate, lower.as_ref(), upper.as_ref()) && !excluded.contains(candidate)
    };
    let relation = |requirement| {
        Relation::new(package, requirement).expect("package name came from a parsed relation")
    };
    if let Some(exact) = exact {
        return if admits(&exact) {
            Ok(vec![relation(VersionRequirement::Equal(exact.into()))])
        } else {
            Err(conflict())
        };
    }
    if let (Some((low, low_inclusive)), Some((high, high_inclusive))) = (&lower, &upper)
        && (low > high || (low == high && !(*low_inclusive && *high_inclusive)))
    {
        return Err(conflict());
    }
    let excluded = excluded
        .iter()
        .filter(|version| admits_bounds(version, lower.as_ref(), upper.as_ref()))
        .cloned()
        .collect::<Vec<_>>();
    let mut merged = Vec::new();
    if let Some((version, inclusive)) = lower {
        merged.push(relation(if inclusive {
            VersionRequirement::GreaterThanEqual(version.into())
        } else {
            VersionRequirement::GreaterThan(version.into())
        }));
    }
    if let Some((version, inclusive)) = upper {
        merged.push(relation(if inclusive {
            VersionRequirement::LessThanEqual(version.into())
        } else {
            VersionRequirement::LessThan(version.into())
        }));
    }
    merged.extend(
        excluded
            .into_iter()
            .map(|version| relation(VersionRequirement::NotEqual(version.into()))),
    );
    if merged.is_empty() {
        merged.push(relation(VersionRequirement::Any));
    }
    Ok(merged)
}

fn tighten(
    bound: &mut Option<(Version, bool)>,
    version: Version,
    inclusive: bool,
    stricter: std::cmp::Ordering,
) {
    let replace = bound.as_ref().is_none_or(|(current, current_inclusive)| {
        version.cmp(current) == stricter
            || (version == *current && *current_inclusive && !inclusive)
    });
    if replace {
        *bound = Some((version, inclusive));
    }
}

fn admits_bounds(
    candidate: &Version,
    lower: Option<&(Version, bool)>,
    upper: Option<&(Version, bool)>,
) -> bool {
    lower.is_none_or(|(bound, inclusive)| candidate > bound || (*inclusive && candidate == bound))
        && upper.is_none_or(|(bound, inclusive)| {
            candidate < bound || (*inclusive && candidate == bound)
        })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RelationField {
    Imports,
    Depends,
    LinkingTo,
    Suggests,
}

impl RelationField {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Imports => "Imports",
            Self::Depends => "Depends",
            Self::LinkingTo => "LinkingTo",
            Self::Suggests => "Suggests",
        }
    }
}

pub fn relations(
    description: &Description,
    field: RelationField,
) -> std::result::Result<Vec<Relation>, String> {
    let parsed = match field {
        RelationField::Imports => description.imports_parsed(),
        RelationField::Depends => description.depends_parsed(),
        RelationField::LinkingTo => description.linking_to_parsed(),
        RelationField::Suggests => description.suggests_parsed(),
    };

    if let Some(issue) = parsed.issues().first() {
        return Err(format!(
            "invalid {field_name} field: {error}",
            field_name = field.as_str(),
            error = issue.error,
        ));
    }

    Ok(parsed.values().cloned().collect())
}

#[cfg(test)]
mod tests {
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
        let description = Description::parse(
            "Package: example\nVersion: 1.0\nImports: cli, broken (=> 1.0), glue\n",
        );

        let error = relations(&description, RelationField::Imports).unwrap_err();
        assert!(error.starts_with("invalid Imports field:"));
    }
}
