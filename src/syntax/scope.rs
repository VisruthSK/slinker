//! Lexical-scope stage.
//!
//! Air extraction records only references that survive definite lexical
//! shadowing. This type makes that stage explicit at the linker boundary and
//! gives later scope refinements a home without coupling them to graph logic.

#[derive(Clone, Debug, Default)]
pub struct ScopeFacts {
    pub unresolved_names: Vec<String>,
}
