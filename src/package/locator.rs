use crate::{Description, Error, Result, TargetEnvironment};
use sha2::{Digest as Sha2Digest, Sha256};
use std::fs::{self, File};
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct Digest(pub String);

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct PackageId {
    pub name: String,
    pub version: String,
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
        let installed = self.target.package(name).ok_or_else(|| {
            Error::Analysis(format!(
                "installed package `{name}` is not present in the selected target library universe"
            ))
        })?;
        let library = fs::canonicalize(&installed.library).unwrap_or_else(|_| installed.library.clone());
        let root = fs::canonicalize(installed.library.join(name))
            .unwrap_or_else(|_| installed.library.join(name));
        let description_path = root.join("DESCRIPTION");
        let description_text = fs::read_to_string(&description_path).map_err(|source| Error::Io {
            path: description_path.clone(),
            source,
        })?;
        let description = Description::parse(&description_text).map_err(|source| Error::Metadata {
            path: description_path,
            source,
        })?;
        let version = description.version().unwrap_or(&installed.version).to_owned();
        let fingerprint = fingerprint_image(&root, name)?;
        Ok(InstalledPackage {
            id: PackageId {
                name: name.to_owned(),
                version,
                library,
                root,
                image_fingerprint: fingerprint,
            },
            description,
        })
    }
}

fn fingerprint_image(root: &Path, _package: &str) -> Result<Digest> {
    let mut files = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory).map_err(|source| Error::Io { path: directory.clone(), source })? {
            let entry = entry.map_err(|source| Error::Io { path: directory.clone(), source })?;
            let path = entry.path();
            let file_type = entry.file_type().map_err(|source| Error::Io { path: path.clone(), source })?;
            if file_type.is_dir() {
                pending.push(path);
            } else if file_type.is_file() {
                files.push(path);
            }
        }
    }
    files.sort_by(|left, right| {
        left.strip_prefix(root).unwrap_or(left).cmp(right.strip_prefix(root).unwrap_or(right))
    });

    let mut hash = Sha256::new();
    hash.update(b"heRmetic-installed-image-v1\0");
    for path in files {
        let relative = path.strip_prefix(root).unwrap_or(&path).to_string_lossy();
        hash.update(relative.as_bytes());
        hash.update([0]);
        let file = File::open(&path).map_err(|source| Error::Io { path: path.clone(), source })?;
        let mut reader = BufReader::new(file);
        let mut buffer = [0u8; 64 * 1024];
        loop {
            let read = reader.read(&mut buffer).map_err(|source| Error::Io { path: path.clone(), source })?;
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
    hash.update(b"heRmetic-key-v1\0");
    for value in values {
        hash.update(value.as_ref().as_bytes());
        hash.update([0]);
    }
    Digest(format!("{:x}", hash.finalize()))
}
