use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceEntry {
    pub path: PathBuf,
    pub text: String,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Sources {
    entries: Vec<SourceEntry>,
}

impl Sources {
    pub fn add(&mut self, path: impl Into<PathBuf>, text: String) -> SourceId {
        let id = SourceId(self.entries.len());
        self.entries.push(SourceEntry { path: path.into(), text });
        id
    }

    pub fn get(&self, id: &SourceId) -> Option<&SourceEntry> {
        self.entries.get(id.0)
    }

    pub fn path(&self, id: &SourceId) -> Option<&Path> {
        self.get(id).map(|entry| entry.path.as_path())
    }
}
