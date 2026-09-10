use std::collections::BTreeMap;
use std::fmt;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Description {
    fields: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Dependency {
    pub name: String,
    pub constraint: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MetadataError {
    ContinuationWithoutField { line: usize },
    MalformedField { line: usize, text: String },
    EmptyFieldName { line: usize },
    MalformedDependency { field: String, value: String },
}

impl Description {
    pub fn parse(text: &str) -> Result<Self, MetadataError> {
        let mut fields = BTreeMap::<String, String>::new();
        let mut current: Option<String> = None;

        for (index, raw) in text.lines().enumerate() {
            let line = index + 1;
            if raw.trim().is_empty() {
                current = None;
                continue;
            }

            if raw.starts_with(' ') || raw.starts_with('\t') {
                let key = current
                    .as_ref()
                    .ok_or(MetadataError::ContinuationWithoutField { line })?;
                let value = fields
                    .get_mut(key)
                    .ok_or(MetadataError::ContinuationWithoutField { line })?;
                if !value.is_empty() {
                    value.push('\n');
                }
                value.push_str(raw.trim());
                continue;
            }

            let (key, value) =
                raw.split_once(':')
                    .ok_or_else(|| MetadataError::MalformedField {
                        line,
                        text: raw.to_owned(),
                    })?;
            let key = key.trim();
            if key.is_empty() {
                return Err(MetadataError::EmptyFieldName { line });
            }
            let key = key.to_owned();
            fields.insert(key.clone(), value.trim().to_owned());
            current = Some(key);
        }

        Ok(Self { fields })
    }

    pub fn get(&self, field: &str) -> Option<&str> {
        self.fields.get(field).map(String::as_str)
    }

    pub fn package(&self) -> Option<&str> {
        self.get("Package")
    }

    pub fn version(&self) -> Option<&str> {
        self.get("Version")
    }

    pub fn dependencies(&self, field: &str) -> Result<Vec<Dependency>, MetadataError> {
        let Some(value) = self.get(field) else {
            return Ok(Vec::new());
        };
        parse_dependencies(field, value)
    }

    pub fn imports(&self) -> Result<Vec<Dependency>, MetadataError> {
        self.dependencies("Imports")
    }

    pub fn depends(&self) -> Result<Vec<Dependency>, MetadataError> {
        self.dependencies("Depends")
    }

    pub fn linking_to(&self) -> Result<Vec<Dependency>, MetadataError> {
        self.dependencies("LinkingTo")
    }

    pub fn suggests(&self) -> Result<Vec<Dependency>, MetadataError> {
        self.dependencies("Suggests")
    }

    pub fn fields(&self) -> impl Iterator<Item = (&str, &str)> {
        self.fields
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str()))
    }
}

fn parse_dependencies(field: &str, value: &str) -> Result<Vec<Dependency>, MetadataError> {
    let normalized = value.replace('\n', " ");
    let mut dependencies = Vec::new();

    for raw in split_top_level_commas(&normalized) {
        let item = raw.trim();
        if item.is_empty() {
            continue;
        }

        let (name, constraint) = match item.find('(') {
            None => (item, None),
            Some(open) => {
                if !item.ends_with(')') || open == 0 {
                    return Err(MetadataError::MalformedDependency {
                        field: field.to_owned(),
                        value: item.to_owned(),
                    });
                }
                let name = item[..open].trim();
                let constraint = item[open + 1..item.len() - 1].trim();
                if constraint.is_empty() {
                    return Err(MetadataError::MalformedDependency {
                        field: field.to_owned(),
                        value: item.to_owned(),
                    });
                }
                (name, Some(constraint.to_owned()))
            }
        };

        if name.is_empty() || name.chars().any(char::is_whitespace) {
            return Err(MetadataError::MalformedDependency {
                field: field.to_owned(),
                value: item.to_owned(),
            });
        }

        if name == "R" {
            continue;
        }
        dependencies.push(Dependency {
            name: name.to_owned(),
            constraint,
        });
    }

    Ok(dependencies)
}

fn split_top_level_commas(value: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start = 0usize;
    let mut depth = 0u32;
    for (index, ch) in value.char_indices() {
        match ch {
            '(' => depth = depth.saturating_add(1),
            ')' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                parts.push(&value[start..index]);
                start = index + 1;
            }
            _ => {}
        }
    }
    parts.push(&value[start..]);
    parts
}

impl fmt::Display for MetadataError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ContinuationWithoutField { line } => {
                write!(f, "DESCRIPTION continuation without a field at line {line}")
            }
            Self::MalformedField { line, text } => {
                write!(f, "malformed DESCRIPTION field at line {line}: {text}")
            }
            Self::EmptyFieldName { line } => {
                write!(f, "empty DESCRIPTION field name at line {line}")
            }
            Self::MalformedDependency { field, value } => {
                write!(f, "malformed {field} dependency entry: {value}")
            }
        }
    }
}

impl std::error::Error for MetadataError {}

#[cfg(test)]
mod tests {
    use super::{Dependency, Description};

    #[test]
    fn parses_folded_fields_and_constraints() {
        let description = Description::parse(
            "Package: root\nVersion: 1.2.3\nImports: foo (>= 1.0),\n    bar, stats\nDepends: R (>= 4.4), methods\n",
        )
        .unwrap();

        assert_eq!(description.package(), Some("root"));
        assert_eq!(
            description.imports().unwrap(),
            vec![
                Dependency {
                    name: "foo".into(),
                    constraint: Some(">= 1.0".into()),
                },
                Dependency {
                    name: "bar".into(),
                    constraint: None,
                },
                Dependency {
                    name: "stats".into(),
                    constraint: None,
                },
            ]
        );
        assert_eq!(description.depends().unwrap()[0].name, "methods");
    }
}
