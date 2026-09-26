use super::graph::NodeId;
use crate::package::PackageId;
use crate::syntax::Span;

/// Free names analysis could not resolve, and the operations in retained code that can bind
/// names at run time. An unresolved name continues through the same global environment and search
/// path in the original and the generated package, so it behaves as the original unless one of
/// these operations could bind it.
#[derive(Default)]
pub(super) struct DynamicNames {
    creators: Vec<NameCreator>,
    unresolved: Vec<UnresolvedName>,
}

pub(super) struct NameCreator {
    pub(super) package: PackageId,
    pub(super) binding: String,
    pub(super) operation: &'static str,
    pub(super) name: CreatedName,
}

pub(super) enum CreatedName {
    Named(String),
    Any,
}

pub(super) struct UnresolvedName {
    pub(super) node: NodeId,
    pub(super) package: PackageId,
    pub(super) binding: Option<String>,
    pub(super) name: String,
    pub(super) span: Span,
}

impl NameCreator {
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

    /// Every unresolved name paired with a retained operation that could bind it at run time.
    pub(super) fn creatable(&self) -> impl Iterator<Item = (&UnresolvedName, &NameCreator)> {
        self.unresolved.iter().filter_map(|unresolved| {
            self.creators
                .iter()
                .find(|creator| creator.can_bind(&unresolved.name))
                .map(|creator| (unresolved, creator))
        })
    }
}
