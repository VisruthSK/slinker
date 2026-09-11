use crate::{Description, Error, Result, TargetEnvironment, Version};
use sha2::{Digest as Sha2Digest, Sha256};
use std::fs::{self, File};
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct Digest(pub String);

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct PackageId {
    pub name: String,
    pub version: Version,
    pub library: PathBuf,
    pub root: PathBuf,
    pub image_fingerprint: Digest,
}

#[derive(Clone, Debug)]
pub struct InstalledPackage {
    pub id: PackageId,
    pub description: Description,
}

#[derive(Clone, Debug)]
pub struct PackageLocator {
    target: TargetEnvironment,
}

impl PackageLocator {
    pub fn new(target: TargetEnvironment) -> Self {
        Self { target }
    }

    pub fn target(&self) -> &TargetEnvironment {
        &self.target
    }

    pub fn locate(&self, name: &str) -> Result<InstalledPackage> {
        self.locate_optional(name)?.ok_or_else(|| {
            Error::Analysis(format!(
                "installed package `{name}` is not present in the selected target library universe"
            ))
        })
    }

    pub fn locate_optional(&self, name: &str) -> Result<Option<InstalledPackage>> {
        for candidate_library in &self.target.libraries {
            let candidate_root = candidate_library.join(name);
            let description_path = candidate_root.join("DESCRIPTION");
            if !description_path.is_file() {
                continue;
            }
            let library = fs::canonicalize(candidate_library)
                .unwrap_or_else(|_| candidate_library.clone());
            let root = fs::canonicalize(&candidate_root).unwrap_or(candidate_root);
            let description_text = fs::read_to_string(&description_path).map_err(|source| Error::Io {
                path: description_path.clone(),
                source,
            })?;
            let description = Description::parse(&description_text);
            let declared_name = description
                .package()
                .ok_or_else(|| Error::Metadata {
                    path: description_path.clone(),
                    message: "missing Package field".to_owned(),
                })?;
            if declared_name.as_str() != name {
                return Err(Error::Metadata {
                    path: description_path.clone(),
                    message: format!(
                        "installed directory name `{name}` disagrees with DESCRIPTION Package `{}`",
                        declared_name.as_str(),
                    ),
                });
            }
            let version = description
                .version_parsed()
                .ok_or_else(|| Error::Metadata {
                    path: description_path.clone(),
                    message: "missing Version field".to_owned(),
                })?
                .map_err(|error| Error::Metadata {
                    path: description_path.clone(),
                    message: format!("invalid Version field: {error}"),
                })?;
            let fingerprint = fingerprint_image(&root)?;
            return Ok(Some(InstalledPackage {
                id: PackageId {
                    name: name.to_owned(),
                    version,
                    library,
                    root,
                    image_fingerprint: fingerprint,
                },
                description,
            }));
        }
        Ok(None)
    }
}

fn fingerprint_image(root: &Path) -> Result<Digest> {
    struct Entry {
        path: PathBuf,
        relative: String,
        size: u64,
        modified_ns: Option<u128>,
    }

    let mut files = Vec::<Entry>::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory).map_err(|source| Error::Io { path: directory.clone(), source })? {
            let entry = entry.map_err(|source| Error::Io { path: directory.clone(), source })?;
            let path = entry.path();
            let file_type = entry.file_type().map_err(|source| Error::Io { path: path.clone(), source })?;
            if file_type.is_dir() {
                pending.push(path);
            } else if file_type.is_file() {
                let metadata = entry.metadata().map_err(|source| Error::Io { path: path.clone(), source })?;
                let relative = path.strip_prefix(root).unwrap_or(&path).to_string_lossy().into_owned();
                let modified_ns = metadata
                    .modified()
                    .ok()
                    .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                    .map(|duration| duration.as_nanos());
                files.push(Entry { path, relative, size: metadata.len(), modified_ns });
            }
        }
    }
    files.sort_by(|left, right| left.relative.cmp(&right.relative));

    // The artifact fingerprint itself remains a SHA-256 over file contents.
    // The manifest is only a cache validator, allowing unchanged installed
    // images to avoid rereading every byte on each `slinker analyze` invocation.
    let mut manifest = Sha256::new();
    manifest.update(b"slinker-installed-manifest-v1\0");
    let mut cacheable = true;
    for entry in &files {
        manifest.update(entry.relative.as_bytes());
        manifest.update([0]);
        manifest.update(entry.size.to_le_bytes());
        match entry.modified_ns {
            Some(value) => manifest.update(value.to_le_bytes()),
            None => {
                cacheable = false;
                manifest.update([0xff; 16]);
            }
        }
    }
    let manifest = format!("{:x}", manifest.finalize());
    let cache = fingerprint_cache_path(root);
    if cacheable {
        if let Ok(text) = fs::read_to_string(&cache) {
            let mut fields = text.trim().split('\t');
            if fields.next() == Some(manifest.as_str()) {
                if let Some(fingerprint) = fields.next() {
                    if fingerprint.len() == 64 && fingerprint.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                        return Ok(Digest(fingerprint.to_owned()));
                    }
                }
            }
        }
    }

    let mut hash = Sha256::new();
    hash.update(b"slinker-installed-image-v2\0");
    for entry in &files {
        hash.update(entry.relative.as_bytes());
        hash.update([0]);
        let file = File::open(&entry.path).map_err(|source| Error::Io { path: entry.path.clone(), source })?;
        let mut reader = BufReader::new(file);
        let mut buffer = [0u8; 128 * 1024];
        loop {
            let read = reader.read(&mut buffer).map_err(|source| Error::Io { path: entry.path.clone(), source })?;
            if read == 0 {
                break;
            }
            hash.update(&buffer[..read]);
        }
        hash.update([0xff]);
    }
    let fingerprint = format!("{:x}", hash.finalize());

    if cacheable {
        if let Some(parent) = cache.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let temporary = cache.with_extension(format!("tmp-{}", std::process::id()));
        if fs::write(&temporary, format!("{manifest}\t{fingerprint}\n")).is_ok() {
            if fs::rename(&temporary, &cache).is_err() {
                let _ = fs::remove_file(&cache);
                let _ = fs::rename(&temporary, &cache);
            }
        }
        let _ = fs::remove_file(temporary);
    }
    Ok(Digest(fingerprint))
}

fn fingerprint_cache_path(root: &Path) -> PathBuf {
    let base = if let Some(path) = std::env::var_os("SLINKER_CACHE_DIR") {
        PathBuf::from(path)
    } else if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir)
            .join("slinker")
            .join("cache")
    } else if let Some(path) = std::env::var_os("XDG_CACHE_HOME") {
        PathBuf::from(path).join("slinker")
    } else if let Some(home) = std::env::var_os("HOME") {
        PathBuf::from(home).join(".cache").join("slinker")
    } else {
        std::env::temp_dir().join("slinker-cache")
    };
    let mut key = Sha256::new();
    key.update(b"slinker-fingerprint-path-v1\0");
    key.update(root.to_string_lossy().as_bytes());
    base.join("fingerprints").join(format!("{:x}.slinker", key.finalize()))
}

pub(crate) fn fingerprint_strings(values: impl IntoIterator<Item = impl AsRef<str>>) -> Digest {
    let mut hash = Sha256::new();
    hash.update(b"slinker-key-v1\0");
    for value in values {
        hash.update(value.as_ref().as_bytes());
        hash.update([0]);
    }
    Digest(format!("{:x}", hash.finalize()))
}
