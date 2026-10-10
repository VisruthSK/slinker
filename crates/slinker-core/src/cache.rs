use crate::package::Digest;
use crate::{Error, Result};
use rusqlite::{Connection, OptionalExtension};
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CacheLocation {
    Default,
    Directory(PathBuf),
    Disabled,
}

#[derive(Debug)]
pub struct Cache {
    database: Option<Mutex<Connection>>,
}

impl Cache {
    pub fn new(location: CacheLocation, schema: &str) -> Result<Self> {
        let (root, explicit) = match location {
            CacheLocation::Disabled => return Ok(Self { database: None }),
            CacheLocation::Directory(root) => (root, true),
            CacheLocation::Default => (default_root(), false),
        };
        let mut directory = root.join("analysis").join(schema);
        if let Err(source) = fs::create_dir_all(&directory) {
            if explicit {
                return Err(Error::Io {
                    path: directory,
                    source,
                });
            }
            directory = std::env::temp_dir()
                .join(format!("slinker-cache-{}", std::process::id()))
                .join("analysis")
                .join(schema);
            fs::create_dir_all(&directory).map_err(|source| Error::Io {
                path: directory.clone(),
                source,
            })?;
        }
        // These artifacts are disposable. A damaged or unavailable database is a cache miss.
        let database = Connection::open(directory.join("cache.sqlite")).and_then(|database| {
            database.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL; CREATE TABLE IF NOT EXISTS entries (name TEXT PRIMARY KEY, bytes BLOB NOT NULL, digest TEXT NOT NULL) WITHOUT ROWID; BEGIN DEFERRED;")?;
            Ok(Mutex::new(database))
        }).ok();
        Ok(Self { database })
    }

    pub fn read<T: DeserializeOwned>(&self, name: &str) -> Option<T> {
        let database = self.database.as_ref()?.lock().ok()?;
        let (bytes, digest): (Vec<u8>, String) = database
            .prepare_cached("SELECT bytes, digest FROM entries WHERE name=?1")
            .ok()?
            .query_row([name], |row| Ok((row.get(0)?, row.get(1)?)))
            .optional()
            .ok()??;
        drop(database);
        if Digest::of(&bytes).as_str() != digest {
            return None;
        }
        serde_json::from_slice(&bytes).ok()
    }

    #[must_use]
    pub fn entries(&self) -> Vec<(String, usize)> {
        let Some(database) = self
            .database
            .as_ref()
            .and_then(|database| database.lock().ok())
        else {
            return Vec::new();
        };
        let Ok(mut statement) =
            database.prepare_cached("SELECT name, length(bytes) FROM entries ORDER BY name")
        else {
            return Vec::new();
        };
        let Ok(rows) = statement.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        }) else {
            return Vec::new();
        };
        rows.filter_map(|row| {
            let (name, length) = row.ok()?;
            Some((name, usize::try_from(length).ok()?))
        })
        .collect()
    }

    pub fn remove_where(&self, remove: impl Fn(&str) -> bool) -> Result<usize> {
        let names = self
            .entries()
            .into_iter()
            .map(|(name, _)| name)
            .filter(|name| remove(name))
            .collect::<Vec<_>>();
        let Some(database) = self.database.as_ref() else {
            return Ok(0);
        };
        let database = database
            .lock()
            .expect("cache database lock is never poisoned");
        let delete = || -> rusqlite::Result<usize> {
            let mut statement = database.prepare_cached("DELETE FROM entries WHERE name=?1")?;
            let mut removed = 0;
            for name in names {
                removed += statement.execute([name])?;
            }
            database.execute_batch("COMMIT; BEGIN DEFERRED;")?;
            Ok(removed)
        };
        delete()
            .map_err(|error| Error::Analysis(format!("failed to remove cached artifacts: {error}")))
    }

    pub fn publish<T: Serialize>(&self, name: &str, value: &T) {
        let Some(database) = self.database.as_ref() else {
            return;
        };
        let Ok(bytes) = serde_json::to_vec(value) else {
            return;
        };
        let digest = Digest::of(&bytes);
        let Ok(database) = database.lock() else {
            return;
        };
        if let Ok(mut statement) = database
            .prepare_cached("INSERT OR IGNORE INTO entries(name,bytes,digest) VALUES (?1,?2,?3)")
        {
            let _ = statement.execute((name, bytes, digest.as_str()));
        }
    }
}

impl Drop for Cache {
    fn drop(&mut self) {
        if let Some(database) = self
            .database
            .as_mut()
            .and_then(|database| database.get_mut().ok())
        {
            let _ = database.execute_batch("COMMIT");
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaDirectory {
    pub schema: String,
    pub directory: PathBuf,
    pub files: usize,
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
                files: files
                    .iter()
                    .filter(|file| {
                        file.path()
                            .extension()
                            .is_some_and(|value| value == "sqlite")
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
