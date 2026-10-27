use crate::analysis::{Diagnostic, LinkIr, RejectCode};
use crate::package::{BindingName, PackageName};
use crate::syntax::{SourceLocation, Sources};
use serde::Serialize;
use std::fmt;

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize)]
pub struct Blocker {
    pub code: Option<RejectCode>,
    pub package: Option<PackageName>,
    pub binding: Option<BindingName>,
    pub message: String,
    pub location: Option<SourceLocation>,
    pub reached_from: Option<String>,
}

impl Blocker {
    pub(super) fn preflight(message: String) -> Self {
        Self {
            code: None,
            package: None,
            binding: None,
            message,
            location: None,
            reached_from: None,
        }
    }

    fn from_diagnostic(diagnostic: &Diagnostic, sources: &Sources) -> Self {
        Self {
            code: Some(diagnostic.code),
            package: Some(diagnostic.package.clone()),
            binding: diagnostic.binding.clone(),
            message: diagnostic.message.clone(),
            location: diagnostic
                .span
                .as_ref()
                .and_then(|span| sources.location(span)),
            reached_from: diagnostic.evidence_summary(),
        }
    }
}

impl fmt::Display for Blocker {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let (Some(code), Some(package)) = (self.code, &self.package) {
            write!(formatter, "{code:?} in {package}")?;
            if let Some(binding) = &self.binding {
                write!(formatter, "::{binding}")?;
            }
            formatter.write_str(": ")?;
        }
        formatter.write_str(&self.message)?;
        if let Some(location) = &self.location {
            write!(formatter, " @ {location}")?;
        }
        if let Some(reached_from) = &self.reached_from {
            write!(formatter, " (reached from {reached_from})")?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct BlockerGroup {
    pub code: Option<RejectCode>,
    pub blockers: Vec<Blocker>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct BuildReport {
    groups: Vec<BlockerGroup>,
}

impl BuildReport {
    pub(super) fn new(mut blockers: Vec<Blocker>) -> Self {
        blockers.sort();
        blockers.dedup();
        let mut groups: Vec<BlockerGroup> = Vec::new();
        for blocker in blockers {
            match groups.iter_mut().find(|group| group.code == blocker.code) {
                Some(group) => group.blockers.push(blocker),
                None => groups.push(BlockerGroup {
                    code: blocker.code,
                    blockers: vec![blocker],
                }),
            }
        }
        Self { groups }
    }

    pub(super) fn preflight(messages: Vec<String>) -> Self {
        Self::new(messages.into_iter().map(Blocker::preflight).collect())
    }

    pub fn analysis_blockers(ir: &LinkIr) -> Vec<Blocker> {
        ir.blockers()
            .iter()
            .map(|diagnostic| Blocker::from_diagnostic(diagnostic, ir.sources()))
            .collect()
    }

    pub fn from_analysis(ir: &LinkIr) -> Self {
        Self::new(Self::analysis_blockers(ir))
    }

    pub fn groups(&self) -> &[BlockerGroup] {
        &self.groups
    }

    pub fn blockers(&self) -> impl Iterator<Item = &Blocker> {
        self.groups.iter().flat_map(|group| &group.blockers)
    }

    pub fn is_empty(&self) -> bool {
        self.groups.is_empty()
    }
}

impl std::error::Error for BuildReport {}

impl fmt::Display for BuildReport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (position, group) in self.groups.iter().enumerate() {
            if position > 0 {
                formatter.write_str("\n")?;
            }
            match group.code {
                Some(code) => write!(formatter, "  {code:?} ({})", group.blockers.len())?,
                None => write!(formatter, "  preflight ({})", group.blockers.len())?,
            }
            for blocker in &group.blockers {
                write!(formatter, "\n    - {blocker}")?;
            }
        }
        Ok(())
    }
}
