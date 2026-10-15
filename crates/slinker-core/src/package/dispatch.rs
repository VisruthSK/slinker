use super::GenericName;
use serde::{Deserialize, Serialize};

/// Target-R observation of a callable's S3 dispatch.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DispatchObservation {
    NoDispatch,
    Generics {
        first: GenericName,
        rest: Vec<GenericName>,
    },
    UnknownDispatch,
}

impl DispatchObservation {
    pub fn from_generics(generics: impl IntoIterator<Item = GenericName>) -> Self {
        let mut generics = generics.into_iter();
        match generics.next() {
            Some(first) => Self::Generics {
                first,
                rest: generics.collect(),
            },
            None => Self::NoDispatch,
        }
    }
}
