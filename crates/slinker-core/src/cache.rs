use crate::{Error, Result};
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::io::{self, BufWriter, Read, Seek, SeekFrom, Write};
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

const PACK_MAGIC: &[u8; 6] = b"SLKP2\n";
const TRAILER_MAGIC: &[u8; 4] = b"SLKE";
const TRAILER_LENGTH: u64 = 16;
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
        let mut file = file;
        let size = file.metadata().ok()?.len();
        if size < PACK_MAGIC.len() as u64 + TRAILER_LENGTH {
            return None;
        }
        let mut magic = [0u8; PACK_MAGIC.len()];
        file.seek(SeekFrom::Start(0)).ok()?;
        file.read_exact(&mut magic).ok()?;
        let mut trailer = [0u8; TRAILER_LENGTH as usize];
        file.seek(SeekFrom::Start(size - TRAILER_LENGTH)).ok()?;
        file.read_exact(&mut trailer).ok()?;
        let footer_start = u64::from_le_bytes(trailer[..8].try_into().ok()?);
        let count = u32::from_le_bytes(trailer[8..12].try_into().ok()?);
        let footer_end = size - TRAILER_LENGTH;
        if &magic != PACK_MAGIC
            || &trailer[12..] != TRAILER_MAGIC
            || footer_start < PACK_MAGIC.len() as u64
            || footer_start > footer_end
        {
            return None;
        }
        let mut footer = vec![0u8; usize::try_from(footer_end - footer_start).ok()?];
        file.seek(SeekFrom::Start(footer_start)).ok()?;
        file.read_exact(&mut footer).ok()?;
        let mut entries = Vec::with_capacity(usize::try_from(count).ok()?);
        let mut rest = footer.as_slice();
        for _ in 0..count {
            let name_length =
                usize::try_from(u32::from_le_bytes(take(&mut rest, 4)?.try_into().ok()?)).ok()?;
            let name = String::from_utf8(take(&mut rest, name_length)?.to_vec()).ok();
            let start = u64::from_le_bytes(take(&mut rest, 8)?.try_into().ok()?);
            let length = u32::from_le_bytes(take(&mut rest, 4)?.try_into().ok()?);
            if start < PACK_MAGIC.len() as u64 || start + u64::from(length) > footer_start {
                return None;
            }
            if let Some(name) = name {
                entries.push((name, start, usize::try_from(length).ok()?));
            }
        }
        Some(entries)
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

fn take<'a>(bytes: &mut &'a [u8], length: usize) -> Option<&'a [u8]> {
    let (head, tail) = bytes.split_at_checked(length)?;
    *bytes = tail;
    Some(head)
}

fn write_pack<N: AsRef<str>, D: AsRef<[u8]>>(
    writer: &mut impl Write,
    entries: impl Iterator<Item = (N, D)>,
) -> io::Result<()> {
    let too_large = |what| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("cache {what} too large"),
        )
    };
    writer.write_all(PACK_MAGIC)?;
    let mut at = PACK_MAGIC.len() as u64;
    let mut footer = Vec::new();
    let mut count = 0u32;
    for (name, data) in entries {
        let (name, data) = (name.as_ref().as_bytes(), data.as_ref());
        let name_length = u32::try_from(name.len()).map_err(|_| too_large("entry name"))?;
        let data_length = u32::try_from(data.len()).map_err(|_| too_large("entry"))?;
        writer.write_all(data)?;
        footer.extend_from_slice(&name_length.to_le_bytes());
        footer.extend_from_slice(name);
        footer.extend_from_slice(&at.to_le_bytes());
        footer.extend_from_slice(&data_length.to_le_bytes());
        at += u64::from(data_length);
        count += 1;
    }
    writer.write_all(&footer)?;
    writer.write_all(&at.to_le_bytes())?;
    writer.write_all(&count.to_le_bytes())?;
    writer.write_all(TRAILER_MAGIC)
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
#[path = "../tests/unit/cache.rs"]
mod tests;
