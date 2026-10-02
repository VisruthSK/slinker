use crate::{Error, Result};
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::io::{self, BufReader, BufWriter, Read, Seek, SeekFrom, Write};
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

#[derive(Debug)]
struct Entry {
    pack: usize,
    start: u64,
    length: usize,
}

#[derive(Debug, Default)]
struct Loaded {
    packs: Vec<Mutex<fs::File>>,
    entries: HashMap<String, Entry>,
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
            loaded.add_pack(&file);
            loaded.files.push(file);
        }
        loaded
    }

    fn add_pack(&mut self, path: &Path) {
        let Ok(file) = fs::File::open(path) else {
            return;
        };
        let Some(entries) = Self::scan(&file) else {
            return;
        };
        let pack = self.packs.len();
        for (name, start, length) in entries {
            self.entries.entry(name).or_insert(Entry {
                pack,
                start,
                length,
            });
        }
        self.packs.push(Mutex::new(file));
    }

    fn scan(file: &fs::File) -> Option<Vec<(String, u64, usize)>> {
        let size = file.metadata().ok()?.len();
        let mut reader = BufReader::new(file);
        let mut magic = [0u8; PACK_MAGIC.len()];
        reader.read_exact(&mut magic).ok()?;
        if &magic != PACK_MAGIC {
            return None;
        }
        let mut at = PACK_MAGIC.len() as u64;
        let mut entries = Vec::new();
        while let Some((name, start, length)) = Self::next_entry(&mut reader, &mut at, size) {
            if let Some(name) = name {
                entries.push((name, start, length));
            }
        }
        Some(entries)
    }

    fn next_entry(
        reader: &mut BufReader<&fs::File>,
        at: &mut u64,
        size: u64,
    ) -> Option<(Option<String>, u64, usize)> {
        let mut word = [0u8; 4];
        reader.read_exact(&mut word).ok()?;
        let name_length = u64::from(u32::from_le_bytes(word));
        if *at + 4 + name_length + 4 > size {
            return None;
        }
        let mut name = vec![0u8; usize::try_from(name_length).ok()?];
        reader.read_exact(&mut name).ok()?;
        reader.read_exact(&mut word).ok()?;
        let data_length = u64::from(u32::from_le_bytes(word));
        let start = *at + 4 + name_length + 4;
        if start + data_length > size {
            return None;
        }
        reader
            .seek_relative(i64::try_from(data_length).ok()?)
            .ok()?;
        *at = start + data_length;
        Some((
            String::from_utf8(name).ok(),
            start,
            usize::try_from(data_length).ok()?,
        ))
    }

    fn get(&self, name: &str) -> Option<Vec<u8>> {
        let entry = self.entries.get(name)?;
        let mut file = self.packs.get(entry.pack)?.lock().ok()?;
        file.seek(SeekFrom::Start(entry.start)).ok()?;
        let mut data = vec![0u8; entry.length];
        file.read_exact(&mut data).ok()?;
        Some(data)
    }
}

fn write_pack<N: AsRef<str>, D: AsRef<[u8]>>(
    writer: &mut impl Write,
    entries: impl Iterator<Item = (N, D)>,
) -> io::Result<()> {
    writer.write_all(PACK_MAGIC)?;
    for (name, data) in entries {
        for part in [name.as_ref().as_bytes(), data.as_ref()] {
            let length = u32::try_from(part.len()).unwrap_or(u32::MAX);
            writer.write_all(&length.to_le_bytes())?;
            writer.write_all(part)?;
        }
    }
    Ok(())
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
        serde_json::from_slice(&self.shared.loaded.get(name)?).ok()
    }

    #[must_use]
    pub fn entries(&self) -> Vec<(String, usize)> {
        let mut entries = self
            .shared
            .loaded
            .entries
            .iter()
            .map(|(name, entry)| (name.clone(), entry.length))
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
            .collect::<BTreeSet<_>>();
        let removed = loaded.entries.len() - kept.len();
        if removed == 0 {
            return Ok(0);
        }
        if !kept.is_empty() {
            let stem = format!("{}-retained", std::process::id());
            let temporary = directory.join(format!("{stem}.tmp"));
            let published = directory.join(format!("{stem}.{PACK_EXTENSION}"));
            let rewrite = fs::File::create(&temporary)
                .map(BufWriter::new)
                .and_then(|mut file| {
                    write_pack(
                        &mut file,
                        kept.iter()
                            .filter_map(|name| Some((name.as_str(), loaded.get(name)?))),
                    )?;
                    file.flush()
                });
            rewrite.map_err(|source| Error::Io {
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
        let names = pending
            .keys()
            .map(String::as_str)
            .chain(
                self.loaded
                    .entries
                    .keys()
                    .map(String::as_str)
                    .filter(|_| compact),
            )
            .collect::<BTreeSet<_>>();
        let entries = names.into_iter().filter_map(|name| {
            let data = match pending.get(name) {
                Some(data) => Cow::Borrowed(data.as_slice()),
                None => Cow::Owned(self.loaded.get(name)?),
            };
            Some((name, data))
        });
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
                write_pack(&mut file, entries)?;
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

    fn encode_pack<N: AsRef<str>, D: AsRef<[u8]>>(
        entries: impl Iterator<Item = (N, D)>,
    ) -> Vec<u8> {
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
}
