use crate::metadata::{Description, Relation, RelationField, relations};

const ARTIFACT_FIELDS: [&str; 8] = [
    "Collate",
    "Collate.unix",
    "Collate.windows",
    "Packaged",
    "Repository",
    "Date/Publication",
    "MD5sum",
    "Built",
];

pub fn generated_description(
    source: &str,
    linked: impl Fn(&str) -> bool,
    imports: &[Relation],
) -> Result<String, Vec<String>> {
    let original = Description::parse(source);
    let mut problems = Vec::new();
    for field in [RelationField::Depends, RelationField::LinkingTo] {
        match relations(&original, field) {
            Ok(values) => problems.extend(
                values
                    .iter()
                    .filter(|relation| linked(relation.package()))
                    .map(|relation| {
                        format!(
                            "{} names Linked package `{}`",
                            field.as_str(),
                            relation.package()
                        )
                    }),
            ),
            Err(error) => problems.push(error),
        }
    }
    let suggests = relations(&original, RelationField::Suggests).unwrap_or_else(|error| {
        problems.push(error);
        Vec::new()
    });
    if !problems.is_empty() {
        return Err(problems);
    }

    let failed = |error: &dyn std::fmt::Display| vec![error.to_string()];
    let mut description = original
        .set_imports(imports)
        .map_err(|error| failed(&error))?;
    if suggests.iter().any(|relation| linked(relation.package())) {
        description = description
            .set_suggests(
                suggests
                    .iter()
                    .filter(|relation| !linked(relation.package())),
            )
            .map_err(|error| failed(&error))?;
    }
    for field in ARTIFACT_FIELDS {
        if description.field(field).is_some() {
            description = description
                .remove_all(field)
                .map_err(|error| failed(&error))?;
        }
    }
    Ok(description.to_string())
}

#[cfg(test)]
#[path = "../../tests/unit/source/description.rs"]
mod tests;
