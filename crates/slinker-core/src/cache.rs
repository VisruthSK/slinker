use crate::{Error, Result};
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
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

impl Writer {
    fn submit(&self, job: WriteJob) {
        let mut sender = self.sender.lock().expect("cache writer");
        let channel = sender.get_or_insert_with(|| {
            let (channel, jobs) = mpsc::channel::<WriteJob>();
            *self.handles.lock().expect("cache writer handles") =
                vec![thread::spawn(move || jobs.iter().for_each(|job| job()))];
            channel
        });
        let _ = channel.send(job);
    }

    fn finish(&self) {
        self.sender.lock().expect("cache writer").take();
        for handle in self.handles.lock().expect("cache writer handles").drain(..) {
            let _ = handle.join();
        }
    }
}

const PACK_MAGIC: &[u8; 6] = b"SLKP1\n";
const PACK_EXTENSION: &str = "pack";
const COMPACTION_THRESHOLD: usize = 8;

#[derive(Debug, Default)]
struct Loaded {
    packs: Vec<Vec<u8>>,
    entries: HashMap<String, (usize, usize, usize)>,
    files: Vec<PathBuf>,
}

impl Loaded {
    fn open(directory: &Path) -> Self {
        let mut loaded = Self::default();
        let Ok(listing) = fs::read_dir(directory) else {
            return loaded;
        };
        let mut files = listing
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .filter(|path| {
                path.extension()
                    .is_some_and(|value| value == PACK_EXTENSION)
            })
            .collect::<Vec<_>>();
        files.sort();
        for file in files {
            if let Ok(bytes) = fs::read(&file) {
                loaded.add_pack(bytes);
                loaded.files.push(file);
            }
        }
        loaded
    }

    fn add_pack(&mut self, bytes: Vec<u8>) {
        let pack = self.packs.len();
        let mut at = PACK_MAGIC.len();
        if bytes.get(..at) != Some(PACK_MAGIC.as_slice()) {
            return;
        }
        while let Some((name, start, end)) = Self::next_entry(&bytes, &mut at) {
            if let Ok(name) = std::str::from_utf8(&bytes[name.0..name.1]) {
                self.entries
                    .entry(name.to_owned())
                    .or_insert((pack, start, end));
            }
        }
        self.packs.push(bytes);
    }

    fn next_entry(bytes: &[u8], at: &mut usize) -> Option<((usize, usize), usize, usize)> {
        let length = |from: usize| -> Option<usize> {
            let raw = bytes.get(from..from + 4)?;
            usize::try_from(u32::from_le_bytes(raw.try_into().ok()?)).ok()
        };
        let name_length = length(*at)?;
        let name = (*at + 4, *at + 4 + name_length);
        let data_length = length(name.1)?;
        let data = (name.1 + 4, name.1 + 4 + data_length);
        if data.1 > bytes.len() {
            return None;
        }
        *at = data.1;
        Some((name, data.0, data.1))
    }

    fn get(&self, name: &str) -> Option<&[u8]> {
        let &(pack, start, end) = self.entries.get(name)?;
        self.packs.get(pack)?.get(start..end)
    }
}

fn write_pack<'a>(
    writer: &mut impl Write,
    entries: impl Iterator<Item = (&'a str, &'a [u8])>,
) -> io::Result<()> {
    writer.write_all(PACK_MAGIC)?;
    for (name, data) in entries {
        for part in [name.as_bytes(), data] {
            let length = u32::try_from(part.len()).unwrap_or(u32::MAX);
            writer.write_all(&length.to_le_bytes())?;
            writer.write_all(part)?;
        }
    }
    Ok(())
}

fn encode_pack<'a>(entries: impl Iterator<Item = (&'a str, &'a [u8])>) -> Vec<u8> {
    let mut bytes = Vec::new();
    write_pack(&mut bytes, entries).expect("writing to a Vec never fails");
    bytes
}

#[derive(Debug)]
struct Shared {
    directory: Option<PathBuf>,
    loaded: Loaded,
    pending: Mutex<BTreeMap<String, Vec<u8>>>,
}

#[derive(Debug)]
pub struct Cache {
    shared: Arc<Shared>,
    writer: Writer,
}

impl Cache {
    pub fn new(location: CacheLocation, schema: &str) -> Result<Self> {
        let (root, explicit) = match location {
            CacheLocation::Disabled => return Ok(Self::at(None)),
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
            return Ok(Self::at(Some(fallback)));
        }
        Ok(Self::at(Some(analysis)))
    }

    fn at(directory: Option<PathBuf>) -> Self {
        let loaded = directory.as_deref().map(Loaded::open).unwrap_or_default();
        Self {
            shared: Arc::new(Shared {
                directory,
                loaded,
                pending: Mutex::new(BTreeMap::new()),
            }),
            writer: Writer::default(),
        }
    }

    pub fn read<T: DeserializeOwned>(&self, name: &str) -> Option<T> {
        self.shared.directory.as_ref()?;
        if let Some(bytes) = self.shared.pending.lock().expect("cache pending").get(name) {
            return serde_json::from_slice(bytes).ok();
        }
        serde_json::from_slice(self.shared.loaded.get(name)?).ok()
    }

    #[must_use]
    pub fn entries(&self) -> Vec<(String, usize)> {
        let mut entries = self
            .shared
            .loaded
            .entries
            .iter()
            .map(|(name, (_, start, end))| (name.clone(), end - start))
            .collect::<Vec<_>>();
        entries.sort();
        entries
    }

    pub fn remove_where(&self, remove: impl Fn(&str) -> bool) -> Result<usize> {
        let Some(directory) = &self.shared.directory else {
            return Ok(0);
        };
        let loaded = &self.shared.loaded;
        let kept = loaded
            .entries
            .keys()
            .filter(|name| !remove(name))
            .filter_map(|name| Some((name.as_str(), loaded.get(name)?)))
            .collect::<BTreeMap<_, _>>();
        let removed = loaded.entries.len() - kept.len();
        if removed == 0 {
            return Ok(0);
        }
        if !kept.is_empty() {
            let stem = format!("{}-retained", std::process::id());
            let temporary = directory.join(format!("{stem}.tmp"));
            let published = directory.join(format!("{stem}.{PACK_EXTENSION}"));
            fs::write(&temporary, encode_pack(kept.into_iter())).map_err(|source| Error::Io {
                path: temporary.clone(),
                source,
            })?;
            fs::rename(&temporary, &published).map_err(|source| Error::Io {
                path: published,
                source,
            })?;
        }
        for file in &loaded.files {
            fs::remove_file(file).map_err(|source| Error::Io {
                path: file.clone(),
                source,
            })?;
        }
        Ok(removed)
    }

    pub fn publish_deferred<T: Serialize + Send + 'static>(&self, name: String, value: T) {
        if self.shared.directory.is_none() {
            return;
        }
        let shared = Arc::clone(&self.shared);
        self.writer
            .submit(Box::new(move || shared.remember(&name, &value)));
    }

    pub fn publish<T: Serialize>(&self, name: &str, value: &T) {
        if self.shared.directory.is_some() {
            self.shared.remember(name, value);
        }
    }
}

impl Shared {
    fn remember(&self, name: &str, value: &impl Serialize) {
        if self.loaded.entries.contains_key(name) {
            return;
        }
        let Ok(bytes) = serde_json::to_vec(value) else {
            return;
        };
        self.pending
            .lock()
            .expect("cache pending")
            .entry(name.to_owned())
            .or_insert(bytes);
    }

    fn flush(&self) {
        let Some(directory) = &self.directory else {
            return;
        };
        let pending = std::mem::take(&mut *self.pending.lock().expect("cache pending"));
        if pending.is_empty() {
            return;
        }
        let compact = self.loaded.files.len() >= COMPACTION_THRESHOLD;
        let mut entries = pending
            .iter()
            .map(|(name, data)| (name.as_str(), data.as_slice()))
            .collect::<BTreeMap<_, _>>();
        if compact {
            for name in self.loaded.entries.keys() {
                if let Some(data) = self.loaded.get(name) {
                    entries.entry(name.as_str()).or_insert(data);
                }
            }
        }
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let stem = format!(
            "{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        );
        let temporary = directory.join(format!("{stem}.tmp"));
        let published = directory.join(format!("{stem}.{PACK_EXTENSION}"));
        if fs::File::create(&temporary)
            .map(BufWriter::new)
            .and_then(|mut file| {
                write_pack(&mut file, entries.into_iter())?;
                file.flush()
            })
            .and_then(|()| fs::rename(&temporary, &published))
            .is_err()
        {
            let _ = fs::remove_file(&temporary);
            return;
        }
        if compact {
            for file in &self.loaded.files {
                let _ = fs::remove_file(file);
            }
        }
    }
}

impl Drop for Cache {
    fn drop(&mut self) {
        self.writer.finish();
        self.shared.flush();
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaDirectory {
    pub schema: String,
    pub directory: PathBuf,
    pub packs: usize,
    pub bytes: u64,
}

#[must_use]
pub fn cache_root(location: &CacheLocation) -> Option<PathBuf> {
    match location {
        CacheLocation::Disabled => None,
        CacheLocation::Directory(root) => Some(root.clone()),
        CacheLocation::Default => Some(default_root()),
    }
}

fn analysis_directory(location: &CacheLocation) -> Option<PathBuf> {
    cache_root(location).map(|root| root.join("analysis"))
}

#[must_use]
pub fn schema_directories(location: &CacheLocation) -> Vec<SchemaDirectory> {
    let Some(analysis) = analysis_directory(location) else {
        return Vec::new();
    };
    let Ok(listing) = fs::read_dir(&analysis) else {
        return Vec::new();
    };
    let mut schemas = listing
        .filter_map(std::result::Result::ok)
        .filter(|entry| entry.path().is_dir())
        .map(|entry| {
            let files = fs::read_dir(entry.path())
                .map(|listing| {
                    listing
                        .filter_map(std::result::Result::ok)
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            SchemaDirectory {
                schema: entry.file_name().to_string_lossy().into_owned(),
                directory: entry.path(),
                packs: files
                    .iter()
                    .filter(|file| {
                        file.path()
                            .extension()
                            .is_some_and(|value| value == PACK_EXTENSION)
                    })
                    .count(),
                bytes: files
                    .iter()
                    .filter_map(|file| file.metadata().ok())
                    .map(|metadata| metadata.len())
                    .sum(),
            }
        })
        .collect::<Vec<_>>();
    schemas.sort_by(|left, right| left.schema.cmp(&right.schema));
    schemas
}

pub fn clear_schemas(location: &CacheLocation, keep: impl Fn(&str) -> bool) -> Result<u64> {
    let mut removed = 0;
    for schema in schema_directories(location) {
        if keep(&schema.schema) {
            continue;
        }
        fs::remove_dir_all(&schema.directory).map_err(|source| Error::Io {
            path: schema.directory.clone(),
            source,
        })?;
        removed += schema.bytes;
    }
    Ok(removed)
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
}
