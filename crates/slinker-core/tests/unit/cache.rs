use super::*;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct Entry {
    value: usize,
}

fn cache(directory: &std::path::Path) -> Cache {
    Cache::new(CacheLocation::Directory(directory.into()), "schema").unwrap()
}

#[test]
fn published_entries_survive_independent_invocations() {
    let directory = tempfile::tempdir().unwrap();
    for value in 0..12 {
        cache(directory.path()).publish(&format!("entry-{value}"), &Entry { value });
    }
    let cache = cache(directory.path());
    for value in 0..12 {
        assert_eq!(
            cache.read::<Entry>(&format!("entry-{value}")),
            Some(Entry { value })
        );
    }
    assert_eq!(cache.entries().len(), 12);
}

#[test]
fn deletion_preserves_other_entries_after_reopening() {
    let directory = tempfile::tempdir().unwrap();
    {
        let cache = cache(directory.path());
        for value in 0..4 {
            cache.publish(&format!("entry-{value}"), &Entry { value });
        }
    }
    assert_eq!(
        cache(directory.path())
            .remove_where(|name| name == "entry-1")
            .unwrap(),
        1
    );
    let cache = cache(directory.path());
    assert_eq!(cache.read::<Entry>("entry-1"), None);
    for value in [0, 2, 3] {
        assert_eq!(
            cache.read::<Entry>(&format!("entry-{value}")),
            Some(Entry { value })
        );
    }
}

#[test]
fn concurrent_publication_keeps_one_complete_immutable_entry() {
    let directory = tempfile::tempdir().unwrap();
    let cache = Arc::new(cache(directory.path()));
    let writers = (0..8)
        .map(|value| {
            let cache = Arc::clone(&cache);
            std::thread::spawn(move || cache.publish("entry", &Entry { value }))
        })
        .collect::<Vec<_>>();
    for writer in writers {
        writer.join().unwrap();
    }
    let winner = cache.read::<Entry>("entry").unwrap();
    assert!(winner.value < 8);
    cache.publish("entry", &Entry { value: 99 });
    assert_eq!(cache.read::<Entry>("entry"), Some(winner.clone()));
    drop(cache);
    assert_eq!(
        self::cache(directory.path()).read::<Entry>("entry"),
        Some(winner)
    );
}

#[test]
fn malformed_serialized_entry_is_a_miss_without_hiding_valid_entries() {
    let directory = tempfile::tempdir().unwrap();
    {
        cache(directory.path()).publish("valid", &Entry { value: 42 });
    }
    let database = Connection::open(directory.path().join("analysis/schema/cache.sqlite")).unwrap();
    database
        .execute(
            "INSERT INTO entries(name,bytes,digest) VALUES (?1,?2,?3)",
            (
                "corrupt",
                b"{truncated".as_slice(),
                Digest::of(b"{truncated").as_str(),
            ),
        )
        .unwrap();
    drop(database);
    let cache = cache(directory.path());
    assert_eq!(cache.read::<Entry>("corrupt"), None);
    assert_eq!(cache.read::<Entry>("valid"), Some(Entry { value: 42 }));
}

#[test]
fn damaged_database_is_a_disposable_cache_miss() {
    let directory = tempfile::tempdir().unwrap();
    fs::create_dir_all(directory.path().join("analysis/schema")).unwrap();
    fs::write(
        directory.path().join("analysis/schema/cache.sqlite"),
        b"not a database",
    )
    .unwrap();
    let cache = cache(directory.path());
    assert_eq!(cache.read::<Entry>("entry"), None);
    cache.publish("entry", &Entry { value: 42 });
    assert!(cache.entries().is_empty());
}

#[test]
fn changed_bytes_that_still_decode_are_not_trusted() {
    let directory = tempfile::tempdir().unwrap();
    {
        cache(directory.path()).publish("entry", &Entry { value: 42 });
    }
    let database = Connection::open(directory.path().join("analysis/schema/cache.sqlite")).unwrap();
    let changed = serde_json::to_vec(&Entry { value: 43 }).unwrap();
    database
        .execute("UPDATE entries SET bytes=?1 WHERE name='entry'", [changed])
        .unwrap();
    drop(database);
    assert_eq!(cache(directory.path()).read::<Entry>("entry"), None);
}

#[test]
fn disabled_cache_never_retains_entries() {
    let cache = Cache::new(CacheLocation::Disabled, "schema").unwrap();
    cache.publish("entry", &Entry { value: 42 });
    assert_eq!(cache.read::<Entry>("entry"), None);
    assert_eq!(cache.remove_where(|_| true).unwrap(), 0);
    assert!(cache.entries().is_empty());
}
