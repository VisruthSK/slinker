use super::graph::NodeId;
use crate::package::PackageId;
use crate::syntax::Span;

#[derive(Default)]
pub(super) struct DynamicNames {
    creators: Vec<NameCreator>,
    unresolved: Vec<UnresolvedName>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct NameCreator {
    pub(super) node: NodeId,
    pub(super) package: PackageId,
    pub(super) binding: String,
    pub(super) operation: &'static str,
    pub(super) name: CreatedName,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum CreatedName {
    Named(String),
    Any,
}

pub(super) struct UnresolvedName {
    pub(super) package: PackageId,
    pub(super) binding: Option<String>,
    pub(super) name: String,
    pub(super) span: Span,
}

impl NameCreator {
    pub(super) fn created_name(&self) -> Option<&str> {
        match &self.name {
            CreatedName::Named(created) => Some(created),
            CreatedName::Any => None,
        }
    }

    fn can_bind(&self, name: &str) -> bool {
        match &self.name {
            CreatedName::Named(created) => created == name,
            CreatedName::Any => true,
        }
    }
}

impl DynamicNames {
    pub(super) fn observe_creator(&mut self, creator: NameCreator) {
        self.creators.push(creator);
    }

    pub(super) fn observe_unresolved(&mut self, name: UnresolvedName) {
        self.unresolved.push(name);
    }

    pub(super) fn creatable(&self) -> impl Iterator<Item = (&UnresolvedName, &NameCreator)> {
        self.unresolved.iter().filter_map(|unresolved| {
            self.creators
                .iter()
                .find(|creator| creator.can_bind(&unresolved.name))
                .map(|creator| (unresolved, creator))
        })
    }
}
