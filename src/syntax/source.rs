use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SourceId(pub usize);

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
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceOrigin {
    File(PathBuf),
    InstalledBinding { package: String, binding: String },
}

impl SourceOrigin {
    pub fn display(&self) -> String {
        match self {
            Self::File(path) => path.display().to_string(),
            Self::InstalledBinding { package, binding } => format!("{package}::{binding}"),
        }
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
    pub fn add(&mut self, origin: SourceOrigin, text: impl Into<Arc<str>>) -> SourceId {
        let id = SourceId(self.entries.len());
        self.entries.push(SourceEntry {
            origin,
            text: text.into(),
        });
        id
    }

    pub fn add_file(&mut self, path: impl Into<PathBuf>, text: impl Into<Arc<str>>) -> SourceId {
        self.add(SourceOrigin::File(path.into()), text)
    }

    pub fn add_binding(
        &mut self,
        package: impl Into<String>,
        binding: impl Into<String>,
        text: impl Into<Arc<str>>,
    ) -> SourceId {
        self.add(
            SourceOrigin::InstalledBinding {
                package: package.into(),
                binding: binding.into(),
            },
            text,
        )
    }

    pub fn get(&self, id: &SourceId) -> Option<&SourceEntry> {
        self.entries.get(id.0)
    }

    pub fn display(&self, id: &SourceId) -> Option<String> {
        self.get(id).map(|entry| entry.origin.display())
    }
}
