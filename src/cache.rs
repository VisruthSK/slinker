use crate::{Error, Result};
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Disposable, schema-scoped analysis cache.
#[derive(Debug)]
pub struct Cache {
    analysis: PathBuf,
}

impl Cache {
    /// Select and initialize the cache root for one analysis schema.
    ///
    /// An explicitly configured root is validated eagerly. Automatically selected caches may
    /// fall back to a process-private temporary directory.
    pub fn new(schema: &str) -> Result<Self> {
        let explicit = std::env::var_os("SLINKER_CACHE_DIR").map(PathBuf::from);
        let root = explicit.clone().unwrap_or_else(default_root);
        let analysis = root.join("analysis").join(schema);
        if let Err(source) = fs::create_dir_all(&analysis) {
            if explicit.is_some() {
                return Err(Error::Io {
                    path: analysis,
                    source,
                });
            }
            let fallback = std::env::temp_dir()
                .join(format!("slinker-cache-{}", std::process::id()))
                .join("analysis")
                .join(schema);
            fs::create_dir_all(&fallback).map_err(|source| Error::Io {
                path: fallback.clone(),
                source,
            })?;
            return Ok(Self { analysis: fallback });
        }
        Ok(Self { analysis })
    }

    /// Return the schema-scoped path for an immutable cache object.
    pub fn path(&self, name: impl AsRef<Path>) -> PathBuf {
        self.analysis.join(name)
    }

    /// Read a typed cache object. Every I/O or decoding problem is a cache miss.
    pub fn read<T: DeserializeOwned>(&self, path: &Path) -> Option<T> {
        fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
    }

    /// Publish a typed object atomically without replacing an existing destination.
    ///
    /// Publication is best effort because computed semantic data never depends on the cache.
    pub fn publish<T: Serialize>(&self, path: &Path, value: &T) {
        let Ok(bytes) = serde_json::to_vec(value) else {
            return;
        };
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let temporary = path.with_extension(format!(
            "tmp-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let result = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .and_then(|mut file| {
                file.write_all(&bytes)?;
                file.flush()?;
                drop(file);
                fs::hard_link(&temporary, path)?;
                fs::remove_file(&temporary)
            });
        if result.is_err() {
            let _ = fs::remove_file(temporary);
        }
    }
}

fn default_root() -> PathBuf {
    if cfg!(windows) {
        if let Some(path) = std::env::var_os("LOCALAPPDATA") {
            return PathBuf::from(path).join("slinker").join("cache");
        }
    } else if let Some(path) = std::env::var_os("XDG_CACHE_HOME") {
        return PathBuf::from(path).join("slinker");
    } else if let Some(path) = std::env::var_os("HOME") {
        return PathBuf::from(path).join(".cache").join("slinker");
    }
    std::env::temp_dir().join(format!("slinker-cache-{}", std::process::id()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};
    use std::sync::Arc;

    #[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
    struct Entry {
        value: usize,
    }

    fn cache() -> Cache {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let analysis = std::env::temp_dir().join(format!(
            "slinker-cache-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&analysis).expect("create test cache");
        Cache { analysis }
    }

    #[test]
    fn corrupt_entry_is_a_miss() {
        let cache = cache();
        let path = cache.path("entry");
        fs::write(&path, b"{truncated").expect("write corrupt entry");

        assert_eq!(cache.read::<Entry>(&path), None);
    }

    #[test]
    fn concurrent_publication_keeps_one_complete_immutable_entry() {
        let cache = Arc::new(cache());
        let path = cache.path("entry");
        let writers = (0..8)
            .map(|value| {
                let cache = Arc::clone(&cache);
                let path = path.clone();
                std::thread::spawn(move || cache.publish(&path, &Entry { value }))
            })
            .collect::<Vec<_>>();
        for writer in writers {
            writer.join().expect("cache writer");
        }

        let winner = cache.read::<Entry>(&path).expect("complete winner");
        assert!(winner.value < 8);
        cache.publish(&path, &Entry { value: 99 });
        assert_eq!(cache.read::<Entry>(&path), Some(winner));
    }
}
