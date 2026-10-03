use super::{Bounded, Lattice};
use std::fmt::Debug;

pub(crate) fn join_of<T: Lattice + Clone>(left: &T, right: &T) -> T {
    let mut joined = left.clone();
    joined.join(right);
    joined
}

pub(crate) fn assert_lattice_laws<T>(samples: &[T])
where
    T: Lattice + Bounded + Clone + Eq + Debug,
{
    let bottom = T::bottom();
    for a in samples {
        assert_eq!(&join_of(a, a), a, "idempotent {a:?}");
        assert_eq!(&join_of(&bottom, a), a, "bottom is the identity {a:?}");
        assert_eq!(&join_of(a, &bottom), a, "bottom is the identity {a:?}");
        let mut same = a.clone();
        assert!(!same.join(a), "joining self reports no growth {a:?}");
        for b in samples {
            let ab = join_of(a, b);
            assert_eq!(ab, join_of(b, a), "commutative {a:?} {b:?}");
            assert_eq!(join_of(a, &ab), ab, "{a:?} is below the join with {b:?}");
            assert_eq!(join_of(b, &ab), ab, "{b:?} is below the join with {a:?}");
            let mut grown = a.clone();
            assert_eq!(
                grown.join(b),
                &ab != a,
                "growth report matches change {a:?} {b:?}"
            );
            for c in samples {
                assert_eq!(
                    join_of(&join_of(a, b), c),
                    join_of(a, &join_of(b, c)),
                    "associative {a:?} {b:?} {c:?}"
                );
            }
        }
    }
}
