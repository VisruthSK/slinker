use crate::cache::{CacheLocation, cache_root};
use crate::package::{
    BindingName, Digest, Fingerprint, PackageIdentity, PackageLocator, PackageName,
    analysis_schema, fingerprint_strings, tree_digest,
};
use crate::{Error, Result, TargetEnvironment};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct IdentityToken {
    pub version: String,
    pub image_fingerprint: Digest,
}

impl From<&PackageIdentity> for IdentityToken {
    fn from(identity: &PackageIdentity) -> Self {
        Self {
            version: identity.version.to_string(),
            image_fingerprint: identity.image_fingerprint.clone(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ConsultedPackage {
    pub name: String,
    pub identity: Option<IdentityToken>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct BuildRecord {
    pub package: String,
    pub output: PathBuf,
    pub inputs: String,
    pub consulted: Vec<ConsultedPackage>,
    pub output_digest: String,
}

#[must_use]
pub fn consulted_packages(
    consulted: &[(PackageName, Option<PackageIdentity>)],
    root_package: &str,
) -> Vec<ConsultedPackage> {
    consulted
        .iter()
        .filter(|(name, _)| name.as_str() != root_package)
        .map(|(name, identity)| ConsultedPackage {
            name: name.as_str().to_owned(),
            identity: identity.as_ref().map(IdentityToken::from),
        })
        .collect()
}

pub fn inputs_digest(
    source: &Digest,
    target: &TargetEnvironment,
    linked: &[&str],
    external: &[&str],
) -> Digest {
    let manifest = std::env::var_os("SLINKER_NATIVE_SUMMARIES")
        .and_then(|path| fs::read(path).ok())
        .map_or_else(String::new, |bytes| Digest::of(bytes).as_str().to_owned());
    Fingerprint::new("slinker-build-inputs-v2")
        .field(env!("CARGO_PKG_VERSION"))
        .field(analysis_schema())
        .field(source.as_str())
        .field(target.r_home.to_string_lossy().as_bytes())
        .field(&target.target.r_version)
        .field(&target.target.os)
        .field(&target.target.arch)
        .field(manifest)
        .list(
            target
                .libraries
                .iter()
                .map(|library| library.to_string_lossy().into_owned()),
        )
        .list(target.base_bindings.iter().map(BindingName::as_str))
        .list(linked)
        .list(external)
        .finish()
}

#[derive(Debug)]
pub struct BuildState {
    directory: Option<PathBuf>,
}

impl BuildState {
    #[must_use]
    pub fn new(location: &CacheLocation) -> Self {
        Self {
            directory: cache_root(location).map(|root| root.join("builds")),
        }
    }

    fn record_path(&self, output: &Path) -> Option<PathBuf> {
        let key = fingerprint_strings([output.to_string_lossy().as_ref()]);
        Some(self.directory.as_ref()?.join(format!("{key}.json")))
    }

    #[must_use]
    pub fn load(&self, output: &Path) -> Option<BuildRecord> {
        let bytes = fs::read(self.record_path(output)?).ok()?;
        let record = serde_json::from_slice::<BuildRecord>(&bytes).ok()?;
        (record.output == output).then_some(record)
    }

    pub fn save(&self, record: &BuildRecord) -> Result<()> {
        let Some(path) = self.record_path(&record.output) else {
            return Ok(());
        };
        let io = |path: &Path| {
            let path = path.to_path_buf();
            move |source| Error::Io { path, source }
        };
        let directory = path.parent().unwrap_or(Path::new("."));
        fs::create_dir_all(directory).map_err(io(directory))?;
        let temporary = path.with_extension(format!("tmp-{}", std::process::id()));
        let bytes = serde_json::to_vec_pretty(record).map_err(|error| {
            Error::Analysis(format!("failed to encode the build record: {error}"))
        })?;
        fs::write(&temporary, bytes).map_err(io(&temporary))?;
        fs::rename(&temporary, &path).map_err(io(&path))
    }

    #[must_use]
    pub fn records(&self) -> Vec<BuildRecord> {
        let Some(directory) = &self.directory else {
            return Vec::new();
        };
        let Ok(listing) = fs::read_dir(directory) else {
            return Vec::new();
        };
        let mut records = listing
            .filter_map(std::result::Result::ok)
            .filter(|entry| {
                entry
                    .path()
                    .extension()
                    .is_some_and(|value| value == "json")
            })
            .filter_map(|entry| fs::read(entry.path()).ok())
            .filter_map(|bytes| serde_json::from_slice::<BuildRecord>(&bytes).ok())
            .collect::<Vec<_>>();
        records.sort_by(|left, right| left.output.cmp(&right.output));
        records
    }

    pub fn remove_packages(&self, names: &[String]) -> usize {
        let mut removed = 0;
        for record in self.records() {
            if names.contains(&record.package)
                && self
                    .record_path(&record.output)
                    .is_some_and(|path| fs::remove_file(path).is_ok())
            {
                removed += 1;
            }
        }
        removed
    }

    pub fn clear(&self) -> Result<usize> {
        let Some(directory) = &self.directory else {
            return Ok(0);
        };
        let removed = self.records().len();
        if directory.exists() {
            fs::remove_dir_all(directory).map_err(|source| Error::Io {
                path: directory.clone(),
                source,
            })?;
        }
        Ok(removed)
    }
}

pub fn is_up_to_date(
    record: &BuildRecord,
    inputs: &Digest,
    locator: &PackageLocator,
    root_package: &str,
) -> Result<bool> {
    if record.inputs != inputs.as_str() || record.package != root_package {
        return Ok(false);
    }
    for consulted in &record.consulted {
        let current = locator
            .locate(&consulted.name)?
            .map(|package| IdentityToken::from(&package.identity));
        if current != consulted.identity {
            return Ok(false);
        }
    }
    if !record.output.is_dir() {
        return Ok(false);
    }
    Ok(tree_digest(&record.output)?.as_str() == record.output_digest)
}
