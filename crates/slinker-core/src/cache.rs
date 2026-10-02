use crate::{Error, Result};
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CacheLocation {
    Default,
    Directory(PathBuf),
    Disabled,
}

type WriteJob = Box<dyn FnOnce() + Send>;

#[derive(Debug, Default)]
struct Writer {
    sender: Mutex<Option<mpsc::Sender<WriteJob>>>,
    handles: Mutex<Vec<thread::JoinHandle<()>>>,
}

const WRITER_THREADS: usize = 4;

impl Writer {
    fn submit(&self, job: WriteJob) {
        let mut sender = self.sender.lock().expect("cache writer");
        let channel = sender.get_or_insert_with(|| {
            let (channel, jobs) = mpsc::channel::<WriteJob>();
            let jobs = Arc::new(Mutex::new(jobs));
            *self.handles.lock().expect("cache writer handles") = (0..WRITER_THREADS)
                .map(|_| {
                    let jobs = Arc::clone(&jobs);
                    thread::spawn(move || {
                        loop {
                            let received = jobs.lock().expect("cache jobs").recv();
                            let Ok(job) = received else {
                                break;
                            };
                            job();
                        }
                    })
                })
                .collect();
            channel
        });
        let _ = channel.send(job);
    }
}

impl Drop for Writer {
    fn drop(&mut self) {
        self.sender.lock().expect("cache writer").take();
        for handle in self.handles.lock().expect("cache writer handles").drain(..) {
            let _ = handle.join();
        }
    }
}

#[derive(Debug)]
pub struct Cache {
    analysis: Option<PathBuf>,
    writer: Writer,
}

impl Cache {
    pub fn new(location: CacheLocation, schema: &str) -> Result<Self> {
        let (root, explicit) = match location {
            CacheLocation::Disabled => {
                return Ok(Self {
                    analysis: None,
                    writer: Writer::default(),
                });
            }
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
                writer: Writer::default(),
            });
        }
        Ok(Self {
            analysis: Some(analysis),
            writer: Writer::default(),
        })
    }

    pub fn read<T: DeserializeOwned>(&self, name: &str) -> Option<T> {
        fs::read(self.analysis.as_ref()?.join(name))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
    }

    pub fn publish_deferred<T: Serialize + Send + 'static>(&self, name: String, value: T) {
        let Some(analysis) = self.analysis.clone() else {
            return;
        };
        self.writer.submit(Box::new(move || {
            if let Ok(bytes) = serde_json::to_vec(&value) {
                write_atomically(&analysis.join(name), &bytes);
            }
        }));
    }

    pub fn publish<T: Serialize>(&self, name: &str, value: &T) {
        let Some(analysis) = &self.analysis else {
            return;
        };
        let Ok(bytes) = serde_json::to_vec(value) else {
            return;
        };
        write_atomically(&analysis.join(name), &bytes);
    }
}

fn write_atomically(path: &std::path::Path, bytes: &[u8]) {
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
            file.write_all(bytes)?;
            file.flush()?;
            drop(file);
            fs::hard_link(&temporary, path)?;
            fs::remove_file(&temporary)
        });
    if result.is_err() {
        let _ = fs::remove_file(temporary);
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
