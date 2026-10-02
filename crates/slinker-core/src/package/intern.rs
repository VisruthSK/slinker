use std::collections::HashSet;
use std::hash::{BuildHasher, RandomState};
use std::sync::{Arc, Mutex, OnceLock};

const SHARDS: usize = 64;

struct Interner {
    hasher: RandomState,
    shards: Vec<Mutex<HashSet<Arc<str>>>>,
}

fn interner() -> &'static Interner {
    static INTERNER: OnceLock<Interner> = OnceLock::new();
    INTERNER.get_or_init(|| Interner {
        hasher: RandomState::new(),
        shards: (0..SHARDS).map(|_| Mutex::new(HashSet::new())).collect(),
    })
}

pub(crate) fn intern(text: &str) -> Arc<str> {
    let interner = interner();
    let shard = (interner.hasher.hash_one(text) as usize) % SHARDS;
    let mut names = interner.shards[shard].lock().expect("name interner");
    if let Some(known) = names.get(text) {
        return Arc::clone(known);
    }
    let fresh: Arc<str> = Arc::from(text);
    names.insert(Arc::clone(&fresh));
    fresh
}

#[cfg(test)]
mod tests {
    use super::intern;
    use std::sync::Arc;

    #[test]
    fn equal_text_shares_one_allocation() {
        assert!(Arc::ptr_eq(&intern("shared"), &intern("shared")));
        assert!(!Arc::ptr_eq(&intern("one"), &intern("two")));
    }
}
