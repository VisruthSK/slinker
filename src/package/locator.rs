use crate::{Description, Error, Result, TargetEnvironment};
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
        let library =
            fs::canonicalize(&installed.library).unwrap_or_else(|_| installed.library.clone());
        let root = fs::canonicalize(installed.library.join(name))
            .unwrap_or_else(|_| installed.library.join(name));
        let description_path = root.join("DESCRIPTION");
        let description_text =
            fs::read_to_string(&description_path).map_err(|source| Error::Io {
                path: description_path.clone(),
                source,
            })?;
        let description =
            Description::parse(&description_text).map_err(|source| Error::Metadata {
                path: description_path,
                source,
            })?;
        let version = description
            .version()
            .unwrap_or(&installed.version)
            .to_owned();
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

fn fingerprint_image(root: &Path, package: &str) -> Result<Digest> {
    let files = [
        root.join("DESCRIPTION"),
        root.join("Meta").join("package.rds"),
        root.join("Meta").join("nsInfo.rds"),
        root.join("R").join(format!("{package}.rdx")),
        root.join("R").join(format!("{package}.rdb")),
        root.join("R").join("sysdata.rdx"),
        root.join("R").join("sysdata.rdb"),
        root.join("data").join("Rdata.rdx"),
        root.join("data").join("Rdata.rdb"),
    ];
    let mut hash = Fnv64::new();
    for path in files {
        if !path.is_file() {
            continue;
        }
        hash.update(path.to_string_lossy().as_bytes());
        let file = File::open(&path).map_err(|source| Error::Io {
            path: path.clone(),
            source,
        })?;
        let mut reader = BufReader::new(file);
        let mut buffer = [0u8; 64 * 1024];
        loop {
            let read = reader.read(&mut buffer).map_err(|source| Error::Io {
                path: path.clone(),
                source,
            })?;
            if read == 0 {
                break;
            }
            hash.update(&buffer[..read]);
        }
    }
    Ok(Digest(format!("{:016x}", hash.finish())))
}

pub(crate) fn fingerprint_strings(values: impl IntoIterator<Item = impl AsRef<str>>) -> Digest {
    let mut hash = Fnv64::new();
    for value in values {
        hash.update(value.as_ref().as_bytes());
        hash.update(&[0]);
    }
    Digest(format!("{:016x}", hash.finish()))
}

struct Fnv64(u64);

impl Fnv64 {
    fn new() -> Self {
        Self(0xcbf29ce484222325)
    }

    fn update(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.0 ^= u64::from(*byte);
            self.0 = self.0.wrapping_mul(0x100000001b3);
        }
    }

    fn finish(self) -> u64 {
        self.0
    }
}
