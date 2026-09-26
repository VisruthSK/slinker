use crate::{Error, Result};
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CacheLocation {
    Default,
    Directory(PathBuf),
    Disabled,
}

/// Disposable, schema-scoped analysis cache.
#[derive(Debug)]
pub struct Cache {
    analysis: Option<PathBuf>,
}

impl Cache {
    /// Select and initialize the cache root for one analysis schema.
    ///
    /// An explicitly configured root is validated eagerly. Automatically selected caches may
    /// fall back to a process-private temporary directory.
    pub fn new(location: CacheLocation, schema: &str) -> Result<Self> {
        let (root, explicit) = match location {
            CacheLocation::Disabled => return Ok(Self { analysis: None }),
            CacheLocation::Directory(root) => (root, true),
            CacheLocation::Default => (default_root(), false),
        };
        let analysis = root.join("analysis").join(schema);
        if let Err(source) = fs::create_dir_all(&analysis) {
            if explicit {
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
            return Ok(Self {
                analysis: Some(fallback),
            });
        }
        Ok(Self {
            analysis: Some(analysis),
        })
    }

    /// Read a typed cache object. Every I/O or decoding problem is a cache miss.
    pub fn read<T: DeserializeOwned>(&self, name: &str) -> Option<T> {
        fs::read(self.analysis.as_ref()?.join(name))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
    }

    /// Publish a typed object atomically without replacing an existing destination.
    ///
    /// Publication is best effort because computed semantic data never depends on the cache.
    pub fn publish<T: Serialize>(&self, name: &str, value: &T) {
        let Some(analysis) = &self.analysis else {
            return;
        };
        let path = &analysis.join(name);
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
        let cache = cache(root.clone());
        fs::write(root.join("analysis/schema/entry"), b"{truncated").expect("write corrupt entry");

        assert_eq!(cache.read::<Entry>("entry"), None);
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
}
