use crate::package::{Digest, InstalledPackage, PackageIdentity, PackageLocation};
use crate::{Description, Error, Result, TargetEnvironment};
use sha2::{Digest as _, Sha256};
use std::fs::{self, File};
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};

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

    pub fn locate(&self, name: &str) -> Result<Option<InstalledPackage>> {
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
                    path: description_path,
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
            let image_fingerprint = fingerprint_image(&root)?;
            return Ok(Some(InstalledPackage {
                identity: PackageIdentity {
                    name: name.into(),
                    version,
                    image_fingerprint,
                },
                location: PackageLocation { library, root },
                description,
            }));
        }
        Ok(None)
    }
}

pub(crate) fn fingerprint_image(root: &Path) -> Result<Digest> {
    let _timer = crate::profile::time(crate::profile::Probe::PackageFingerprint);
    let mut files = Vec::<(String, PathBuf)>::new();
    let mut pending = vec![root.to_path_buf()];
    let io = |path: &Path| {
        let path = path.to_path_buf();
        move |source| Error::Io { path, source }
    };
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory).map_err(io(&directory))? {
            let entry = entry.map_err(io(&directory))?;
            let path = entry.path();
            let file_type = entry.file_type().map_err(io(&path))?;
            if file_type.is_dir() {
                pending.push(path);
            } else if file_type.is_file() {
                let relative = path
                    .strip_prefix(root)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .into_owned();
                files.push((relative, path));
            }
        }
    }
    files.sort_unstable_by(|left, right| left.0.cmp(&right.0));

    let mut hash = Sha256::new();
    hash.update(b"slinker-installed-image-v2\0");
    let mut buffer = vec![0u8; 128 * 1024];
    for (relative, path) in &files {
        hash.update(relative.as_bytes());
        hash.update([0]);
        crate::profile::count(crate::profile::Count::FingerprintFiles, 1);
        let mut reader = BufReader::new(File::open(path).map_err(io(path))?);
        loop {
            let read = reader.read(&mut buffer).map_err(io(path))?;
            if read == 0 {
                break;
            }
            crate::profile::count(crate::profile::Count::FingerprintBytes, read as u64);
            hash.update(&buffer[..read]);
        }
        hash.update([0xff]);
    }
    Ok(Digest::finish(hash))
}

pub(crate) fn fingerprint_strings(values: impl IntoIterator<Item = impl AsRef<str>>) -> Digest {
    let mut hash = Sha256::new();
    hash.update(b"slinker-key-v1\0");
    for value in values {
        hash.update(value.as_ref().as_bytes());
        hash.update([0]);
    }
    Digest::finish(hash)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_fingerprint_reads_current_bytes_even_when_length_is_unchanged() {
        let root = tempfile::tempdir().expect("fixture root");
        let path = root.path().join("object.rdb");
        fs::write(&path, b"before").expect("write first image");
        let before = fingerprint_image(root.path()).expect("fingerprint first image");
        fs::write(&path, b"after!").expect("rewrite same-length image");
        let after = fingerprint_image(root.path()).expect("fingerprint changed image");

        assert_ne!(before, after);
    }

    #[test]
    fn identity_excludes_physical_location() {
        let first = tempfile::tempdir().expect("first library");
        let second = tempfile::tempdir().expect("second library");
        for library in [first.path(), second.path()] {
            let root = library.join("fixture");
            fs::create_dir(&root).expect("package root");
            fs::write(
                root.join("DESCRIPTION"),
                "Package: fixture\nVersion: 1.0.0\n",
            )
            .expect("DESCRIPTION");
        }
        let locate = |library: &Path| {
            PackageLocator::new(TargetEnvironment {
                r_home: PathBuf::new(),
                target: crate::Target {
                    r_version: String::new(),
                    os: String::new(),
                    arch: String::new(),
                },
                libraries: vec![library.to_path_buf()],
                base_bindings: Default::default(),
            })
            .locate("fixture")
            .expect("locate fixture")
            .expect("fixture present")
        };

        let (first, second) = (locate(first.path()), locate(second.path()));

        assert_eq!(first.identity, second.identity);
        assert_ne!(first.location, second.location);
    }
}
