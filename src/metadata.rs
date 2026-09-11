//! Typed R package metadata.
//!
//! `r-description-parser` owns DESCRIPTION/DCF parsing. `r-metadata` owns the
//! semantic values. Slinker deliberately does not keep a second DCF parser or
//! duplicate dependency/version model here.

pub use r_description::Description;
pub use r_metadata::{Priority, Relation, Version};

/// Standard relationship fields slinker consumes for installed-package policy.
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

/// Parse one standard dependency field through `r-description-parser`.
///
/// The parser recovers valid entries around malformed ones, but slinker cannot
/// use a partial dependency list without changing package reachability. Treat
/// any retained issue as a metadata error and only return a complete typed list.
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

    #[test]
    fn malformed_relation_field_is_not_partially_accepted() {
        let description = Description::parse(
            "Package: example\nVersion: 1.0\nImports: cli, broken (=> 1.0), glue\n",
        );

        let error = relations(&description, RelationField::Imports).unwrap_err();
        assert!(error.starts_with("invalid Imports field:"));
    }
}
