use super::graph::NodeId;
use crate::package::{BindingName, PackageId};
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
    pub(super) binding: BindingName,
    pub(super) operation: CreatorOperation,
    pub(super) name: CreatedName,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(super) enum CreatorOperation {
    Assign,
    DelayedAssign,
    MakeActiveBinding,
    List2env,
    EnvironmentAssign,
    SuperAssign,
    DynLibRegistration,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum CreatedName {
    Named(BindingName),
    Any,
}

pub(super) struct UnresolvedName {
    pub(super) package: PackageId,
    pub(super) binding: Option<BindingName>,
    pub(super) name: BindingName,
    pub(super) span: Span,
}

impl std::fmt::Display for CreatorOperation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Assign => "assign",
            Self::DelayedAssign => "delayedAssign",
            Self::MakeActiveBinding => "makeActiveBinding",
            Self::List2env => "list2env",
            Self::EnvironmentAssign => "environment<-",
            Self::SuperAssign => "<<-",
            Self::DynLibRegistration => "useDynLib(.registration = TRUE)",
        })
    }
}

impl NameCreator {
    pub(super) fn created_name(&self) -> Option<&BindingName> {
        match &self.name {
            CreatedName::Named(created) => Some(created),
            CreatedName::Any => None,
        }
    }

    fn can_bind(&self, name: &BindingName) -> bool {
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
