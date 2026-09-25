use crate::{Description, Error, Result, TargetEnvironment, Version};
use sha2::{Digest as Sha2Digest, Sha256};
use std::fs::{self, File};
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct Digest(pub String);

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct PackageId {
    pub name: String,
    pub version: Version,
    pub image_fingerprint: Digest,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackageLocation {
    pub library: PathBuf,
    pub root: PathBuf,
}

#[derive(Clone, Debug)]
pub struct InstalledPackage {
    pub id: PackageId,
    pub location: PackageLocation,
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
            let library = dunce::canonicalize(candidate_library)
                .unwrap_or_else(|_| candidate_library.clone());
            let root = dunce::canonicalize(&candidate_root).unwrap_or(candidate_root);
            let description_text =
                fs::read_to_string(&description_path).map_err(|source| Error::Io {
                    path: description_path.clone(),
                    source,
                })?;
            let description = Description::parse(&description_text);
            let declared_name = description.package().ok_or_else(|| Error::Metadata {
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
                    image_fingerprint: fingerprint,
                },
                location: PackageLocation { library, root },
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
    }

    let mut files = Vec::<Entry>::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory).map_err(|source| Error::Io {
            path: directory.clone(),
            source,
        })? {
            let entry = entry.map_err(|source| Error::Io {
                path: directory.clone(),
                source,
            })?;
            let path = entry.path();
            let file_type = entry.file_type().map_err(|source| Error::Io {
                path: path.clone(),
                source,
            })?;
            if file_type.is_dir() {
                pending.push(path);
            } else if file_type.is_file() {
                let relative = path
                    .strip_prefix(root)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .into_owned();
                files.push(Entry { path, relative });
            }
        }
    }
    files.sort_by(|left, right| left.relative.cmp(&right.relative));

    let mut hash = Sha256::new();
    hash.update(b"slinker-installed-image-v2\0");
    for entry in &files {
        hash.update(entry.relative.as_bytes());
        hash.update([0]);
        let file = File::open(&entry.path).map_err(|source| Error::Io {
            path: entry.path.clone(),
            source,
        })?;
        let mut reader = BufReader::new(file);
        let mut buffer = [0u8; 128 * 1024];
        loop {
            let read = reader.read(&mut buffer).map_err(|source| Error::Io {
                path: entry.path.clone(),
                source,
            })?;
            if read == 0 {
                break;
            }
            hash.update(&buffer[..read]);
        }
        hash.update([0xff]);
    }
    Ok(Digest(format!("{:x}", hash.finalize())))
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    #[test]
    fn image_fingerprint_reads_current_bytes_even_when_length_is_unchanged() {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "slinker-fingerprint-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&root).expect("create fixture root");
        let path = root.join("object.rdb");
        fs::write(&path, b"before").expect("write first image");
        let before = fingerprint_image(&root).expect("fingerprint first image");
        fs::write(&path, b"after!").expect("rewrite same-length image");
        let after = fingerprint_image(&root).expect("fingerprint changed image");

        assert_ne!(before, after);
        fs::remove_dir_all(root).expect("remove fixture root");
    }
}
