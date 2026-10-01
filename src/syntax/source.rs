use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SourceId(pub(crate) usize);

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Span {
    pub source: SourceId,
    pub start: usize,
    pub end: usize,
}

impl Span {
    pub fn new(source: SourceId, start: usize, end: usize) -> Self {
        Self { source, start, end }
    }

    pub fn range(&self) -> TextRange {
        TextRange::new(self.start, self.end)
    }

    pub fn overlaps(&self, other: &Self) -> bool {
        self.source == other.source && self.range().overlaps(other.range())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TextRange {
    pub start: usize,
    pub end: usize,
}

impl TextRange {
    pub fn new(start: usize, end: usize) -> Self {
        Self { start, end }
    }

    pub fn contains_range(self, inner: Self) -> bool {
        self.start <= inner.start && inner.end <= self.end
    }

    pub fn overlaps(self, other: Self) -> bool {
        self.start < other.end && other.start < self.end
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SourceKey {
    Binding(String),
    Private {
        environment: String,
        binding: String,
    },
    Closure {
        owner: Box<SourceKey>,
        path: String,
        environment: String,
    },
    Runtime,
}

impl SourceKey {
    pub fn namespace_binding(&self) -> Option<&str> {
        match self {
            Self::Binding(name) => Some(name),
            Self::Private { .. } | Self::Closure { .. } | Self::Runtime => None,
        }
    }
}

impl std::fmt::Display for SourceKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Binding(name) => f.write_str(name),
            Self::Private {
                environment,
                binding,
            } => write!(f, "{environment}${binding}"),
            Self::Closure {
                owner,
                path,
                environment,
            } => write!(f, "{owner}{path}@{environment}"),
            Self::Runtime => f.write_str("runtime"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SourceOrigin {
    pub package: String,
    pub key: SourceKey,
}

impl SourceOrigin {
    pub fn display(&self) -> String {
        format!("{}::{}", self.package, self.key)
    }
}

#[derive(Debug, Clone)]
pub struct SourceEntry {
    pub origin: SourceOrigin,
    pub text: Arc<str>,
}

#[derive(Debug, Default, Clone)]
pub struct Sources {
    entries: Vec<SourceEntry>,
}

impl Sources {
    pub fn add(
        &mut self,
        package: impl Into<String>,
        key: SourceKey,
        text: impl Into<Arc<str>>,
    ) -> SourceId {
        let id = SourceId(self.entries.len());
        self.entries.push(SourceEntry {
            origin: SourceOrigin {
                package: package.into(),
                key,
            },
            text: text.into(),
        });
        id
    }

    pub fn origin(&self, id: &SourceId) -> &SourceOrigin {
        &self.entries[id.0].origin
    }

    pub fn text(&self, span: &Span) -> Option<&str> {
        self.get(&span.source)?.text.get(span.start..span.end)
    }

    pub fn get(&self, id: &SourceId) -> Option<&SourceEntry> {
        self.entries.get(id.0)
    }

    pub fn display(&self, id: &SourceId) -> String {
        self.origin(id).display()
    }

    pub fn location(&self, span: &Span) -> Option<SourceLocation> {
        let entry = self.get(&span.source)?;
        let before = entry.text.get(..span.start)?;
        let line_start = before.rfind('\n').map_or(0, |newline| newline + 1);
        Some(SourceLocation {
            source: entry.origin.display(),
            line: before.matches('\n').count() + 1,
            column: before[line_start..].chars().count() + 1,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct SourceLocation {
    pub source: String,
    pub line: usize,
    pub column: usize,
}

impl std::fmt::Display for SourceLocation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}:{}", self.source, self.line, self.column)
    }
}

#[cfg(test)]
mod tests {
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
}
