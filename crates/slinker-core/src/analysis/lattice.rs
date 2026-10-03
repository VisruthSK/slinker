pub(super) trait Lattice {
    fn join(&mut self, other: &Self) -> bool;
}

pub(super) trait Bounded: Lattice {
    fn bottom() -> Self;
}

#[cfg(test)]
#[path = "../../tests/unit/analysis/lattice.rs"]
pub(super) mod laws;
