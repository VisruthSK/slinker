use super::super::lattice::laws::assert_lattice_laws;
use super::super::lattice::{Bounded, Lattice};
use super::AbstractValue;
use super::{ConstructionExpr, ConstructionExprKind};
use crate::syntax::{SourceId, Span};
use std::collections::BTreeMap;
use std::sync::Arc;

fn function(parameter: &str) -> AbstractValue {
    AbstractValue::Function {
        parameters: vec![crate::package::Atom::from(parameter)].into(),
        body: Arc::new(ConstructionExpr {
            kind: ConstructionExprKind::Null,
            span: Span::new(SourceId(0), 0, 0),
        }),
        captures: BTreeMap::new(),
    }
}

fn samples() -> Vec<AbstractValue> {
    vec![
        AbstractValue::Bottom,
        AbstractValue::Null,
        AbstractValue::Logical(true),
        AbstractValue::Logical(false),
        AbstractValue::Integer(1),
        AbstractValue::Integer(2),
        AbstractValue::String("a".into()),
        AbstractValue::String("b".into()),
        AbstractValue::Vector(vec![AbstractValue::Integer(1)]),
        AbstractValue::Vector(vec![AbstractValue::Integer(2)]),
        function("x"),
        function("y"),
        AbstractValue::Unknown,
    ]
}

#[test]
fn abstract_values_obey_the_lattice_laws() {
    assert_lattice_laws(&samples());
}

#[test]
fn distinct_exact_values_widen_to_unknown_and_unknown_is_absorbing() {
    let mut value = AbstractValue::String("a".into());
    assert!(value.join(&AbstractValue::String("b".into())));
    assert_eq!(value, AbstractValue::Unknown);
    for other in samples() {
        assert!(!value.join(&other));
        assert_eq!(value, AbstractValue::Unknown);
    }
}

#[test]
fn no_information_is_distinct_from_unknown() {
    assert_ne!(AbstractValue::bottom(), AbstractValue::Unknown);
    let mut value = AbstractValue::bottom();
    assert!(!value.join(&AbstractValue::Bottom));
    assert!(value.join(&AbstractValue::Null));
    assert_eq!(value, AbstractValue::Null);
}
