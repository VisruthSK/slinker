use serde::Serialize;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    Index,
    Binding,
    Environment,
    Normalization,
    Dispatch,
}

impl std::fmt::Display for EntryKind {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Index => "index",
            Self::Binding => "binding",
            Self::Environment => "environment",
            Self::Normalization => "normalization",
            Self::Dispatch => "dispatch",
        })
    }
}

impl EntryKind {
    pub const ALL: [Self; 5] = [
        Self::Index,
        Self::Binding,
        Self::Environment,
        Self::Normalization,
        Self::Dispatch,
    ];

    fn suffix(self) -> &'static str {
        match self {
            Self::Index => ".index.slinker",
            Self::Binding => ".binding.slinker",
            Self::Environment => ".environment.slinker",
            Self::Normalization => ".syntax.slinker",
            Self::Dispatch => ".dispatch.slinker",
        }
    }

    fn per_package(self) -> bool {
        matches!(self, Self::Index | Self::Binding | Self::Environment)
    }

    pub(super) fn index_name(package: &str, key: &str) -> String {
        format!("{package}-{key}{}", Self::Index.suffix())
    }

    pub(super) fn member_name(self, package: &str, key: &str, member: &str) -> String {
        format!("{package}-{key}-{member}{}", self.suffix())
    }

    pub(super) fn normalization_name(key: &str) -> String {
        format!("normalized-{key}{}", Self::Normalization.suffix())
    }

    pub(super) fn dispatch_name(key: &str) -> String {
        format!("dispatch-{key}{}", Self::Dispatch.suffix())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EntryName {
    pub kind: EntryKind,
    pub package: Option<String>,
    pub key: String,
}

impl EntryName {
    pub fn parse(name: &str) -> Option<Self> {
        let kind = EntryKind::ALL
            .into_iter()
            .find(|kind| name.ends_with(kind.suffix()))?;
        let stem = name.strip_suffix(kind.suffix())?;
        if kind.per_package() {
            let (package, rest) = stem.split_once('-')?;
            let key = rest.split('-').next()?;
            Some(Self {
                kind,
                package: Some(package.to_owned()),
                key: key.to_owned(),
            })
        } else {
            let key = stem.split_once('-')?.1;
            Some(Self {
                kind,
                package: None,
                key: key.to_owned(),
            })
        }
    }
}

#[cfg(test)]
#[path = "../../tests/unit/package/cache_names.rs"]
mod tests;
