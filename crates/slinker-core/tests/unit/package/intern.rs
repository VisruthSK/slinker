use super::intern;
use std::sync::Arc;

#[test]
fn equal_text_shares_one_allocation() {
    assert!(Arc::ptr_eq(&intern("shared"), &intern("shared")));
    assert!(!Arc::ptr_eq(&intern("one"), &intern("two")));
}
