use crate::package::{Digest, PackageId};
use crate::syntax::{ParsedRFile, SourceId, SourceKey, Sources, Span};
use std::collections::HashMap;
use std::sync::Arc;

pub(super) type ParseKey = (PackageId, SourceKey);

#[derive(Clone, Debug)]
pub(super) enum ParseState {
    Parsed(Arc<ParsedRFile>),
    Blocked,
}

#[derive(Default)]
pub(super) struct ParseCache {
    states: HashMap<ParseKey, ParseState>,
    sources: Sources,
    source_ids: HashMap<ParseKey, SourceId>,
    normalized_shapes: HashMap<ParseKey, Digest>,
}

impl ParseCache {
    pub(super) fn state(&self, key: &ParseKey) -> Option<&ParseState> {
        self.states.get(key)
    }

    pub(super) fn contains(&self, key: &ParseKey) -> bool {
        self.states.contains_key(key)
    }

    pub(super) fn register(&mut self, key: ParseKey, package: &str, text: &Arc<str>) -> SourceId {
        let source = self.sources.add(package, key.1.clone(), Arc::clone(text));
        self.source_ids.insert(key, source);
        source
    }

    pub(super) fn registered(&self, key: &ParseKey) -> Option<(SourceId, Arc<str>)> {
        let source = self.source_ids.get(key)?;
        let text = Arc::clone(&self.sources.get(source)?.text);
        Some((*source, text))
    }

    pub(super) fn block(&mut self, key: ParseKey) {
        self.states.insert(key, ParseState::Blocked);
    }

    pub(super) fn store(&mut self, key: ParseKey, parsed: Arc<ParsedRFile>) {
        self.states.insert(key, ParseState::Parsed(parsed));
    }

    pub(super) fn record_shape(&mut self, key: ParseKey, shape: Digest) {
        self.normalized_shapes.insert(key, shape);
    }

    pub(super) fn shape(&self, key: &ParseKey) -> Option<&Digest> {
        self.normalized_shapes.get(key)
    }

    pub(super) fn sources(&self) -> &Sources {
        &self.sources
    }

    pub(super) fn text(&self, span: &Span) -> Option<&str> {
        self.sources.text(span)
    }

    pub(super) fn into_sources(self) -> Sources {
        self.sources
    }
}
