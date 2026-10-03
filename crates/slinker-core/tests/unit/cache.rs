use super::*;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

fn encode_pack<N: AsRef<str>, D: AsRef<[u8]>>(entries: impl Iterator<Item = (N, D)>) -> Vec<u8> {
    let mut bytes = Vec::new();
    write_pack(&mut bytes, entries).expect("writing to a Vec never fails");
    bytes
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct Entry {
    value: usize,
}

fn root() -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    std::env::temp_dir().join(format!(
        "slinker-cache-test-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ))
}

fn cache(root: PathBuf) -> Cache {
    Cache::new(CacheLocation::Directory(root), "schema").expect("create test cache")
}

#[test]
fn corrupt_entry_is_a_miss() {
    let root = root();
    let directory = root.join("analysis/schema");
    fs::create_dir_all(&directory).expect("create cache directory");
    let corrupt = encode_pack([("entry", b"{truncated".as_slice())].into_iter());
    fs::write(directory.join("corrupt.pack"), corrupt).expect("write corrupt pack");

    assert_eq!(cache(root).read::<Entry>("entry"), None);
}

#[test]
fn published_entries_survive_a_reopen_and_compaction() {
    let root = root();
    for value in 0..(COMPACTION_THRESHOLD + 3) {
        let cache = cache(root.clone());
        cache.publish(&format!("entry-{value}"), &Entry { value });
    }
    let cache = cache(root.clone());
    for value in 0..(COMPACTION_THRESHOLD + 3) {
        assert_eq!(
            cache.read::<Entry>(&format!("entry-{value}")),
            Some(Entry { value })
        );
    }
    let packs = fs::read_dir(root.join("analysis/schema"))
        .expect("list cache")
        .filter_map(std::result::Result::ok)
        .filter(|entry| {
            entry
                .path()
                .extension()
                .is_some_and(|value| value == PACK_EXTENSION)
        })
        .count();
    assert!(packs <= COMPACTION_THRESHOLD, "{packs} packs");
}

#[test]
fn disabled_cache_never_returns_a_published_entry() {
    let cache = Cache::new(CacheLocation::Disabled, "schema").expect("disabled cache");
    cache.publish("entry", &Entry { value: 1 });

    assert_eq!(cache.read::<Entry>("entry"), None);
}

#[test]
fn concurrent_publication_keeps_one_complete_immutable_entry() {
    let cache = Arc::new(cache(root()));
    let writers = (0..8)
        .map(|value| {
            let cache = Arc::clone(&cache);
            std::thread::spawn(move || cache.publish("entry", &Entry { value }))
        })
        .collect::<Vec<_>>();
    for writer in writers {
        writer.join().expect("cache writer");
    }

    let winner = cache.read::<Entry>("entry").expect("complete winner");
    assert!(winner.value < 8);
    cache.publish("entry", &Entry { value: 99 });
    assert_eq!(cache.read::<Entry>("entry"), Some(winner));
}
#[test]
fn truncated_and_old_format_packs_are_ignored_without_hiding_complete_ones() {
    let root = root();
    let directory = root.join("analysis/schema");
    fs::create_dir_all(&directory).expect("create cache directory");
    let complete = encode_pack([("kept", br#"{"value":1}"#.as_slice())].into_iter());
    let mut truncated = encode_pack([("lost", br#"{"value":2}"#.as_slice())].into_iter());
    truncated.truncate(truncated.len() - 3);
    fs::write(directory.join("a.pack"), complete).expect("write complete pack");
    fs::write(directory.join("b.pack"), truncated).expect("write truncated pack");
    fs::write(directory.join("c.pack"), b"SLKP1\nentries without a footer")
        .expect("write old-format pack");

    let cache = cache(root);

    assert_eq!(cache.read::<Entry>("kept"), Some(Entry { value: 1 }));
    assert_eq!(cache.read::<Entry>("lost"), None);
    assert_eq!(cache.entries().len(), 1);
}

#[test]
fn a_pack_with_many_entries_is_indexed_from_its_footer() {
    let root = root();
    let directory = root.join("analysis/schema");
    fs::create_dir_all(&directory).expect("create cache directory");
    let values = (0..2000)
        .map(|value| {
            (
                format!("entry-{value}"),
                serde_json::to_vec(&Entry { value }).expect("serialize"),
            )
        })
        .collect::<Vec<_>>();
    fs::write(
        directory.join("many.pack"),
        encode_pack(
            values
                .iter()
                .map(|(name, data)| (name.as_str(), data.as_slice())),
        ),
    )
    .expect("write pack");

    let cache = cache(root);

    assert_eq!(cache.entries().len(), 2000);
    assert_eq!(
        cache.read::<Entry>("entry-1999"),
        Some(Entry { value: 1999 })
    );
    assert_eq!(cache.read::<Entry>("entry-0"), Some(Entry { value: 0 }));
}
