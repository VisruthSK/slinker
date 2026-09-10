use crate::package::image::parse_package_image;
use crate::package::locator::fingerprint_strings;
use crate::package::{InstalledPackage, PackageId, PackageImage, PackageLocator};
use crate::{Error, RToolchain, Result, TargetEnvironment};
use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

const AIR_VERSION: &str = "0.11.0";
const ANALYSIS_SCHEMA: &str = "binding-linker-v1";

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SyntaxValidation {
    Accepted,
    Rejected(String),
}

pub trait PackageProvider {
    fn locate(&mut self, name: &str) -> Result<InstalledPackage>;
    fn image(&mut self, package: &InstalledPackage) -> Result<Arc<PackageImage>>;
    fn prefetch(&mut self, _packages: &[InstalledPackage], _jobs: usize) -> Result<()> {
        Ok(())
    }
    fn is_target_provided(&self, package: &InstalledPackage) -> bool;
    fn validate_syntax(
        &self,
        id: &PackageId,
        binding: &str,
        source: &str,
    ) -> Result<SyntaxValidation>;
}

pub struct PackageStore {
    toolchain: RToolchain,
    locator: PackageLocator,
    explicit_target: HashSet<String>,
    locations: HashMap<String, InstalledPackage>,
    images: HashMap<PackageId, Arc<PackageImage>>,
    cache_dir: PathBuf,
    work_dir: PathBuf,
    target_fingerprint: String,
    validations: AtomicUsize,
}

impl PackageStore {
    pub fn new(
        toolchain: RToolchain,
        target: TargetEnvironment,
        explicit_target: impl IntoIterator<Item = String>,
        work_dir: impl Into<PathBuf>,
    ) -> Result<Self> {
        let target_fingerprint = fingerprint_strings(
            std::iter::once(format!(
                "R={} OS={} ARCH={}",
                target.target.r_version, target.target.os, target.target.arch
            ))
            .chain(
                target
                    .libraries
                    .iter()
                    .map(|path| path.to_string_lossy().into_owned()),
            ),
        )
        .0;
        let cache_dir = default_cache_dir().join("analysis").join(ANALYSIS_SCHEMA);
        fs::create_dir_all(&cache_dir).map_err(|source| Error::Io {
            path: cache_dir.clone(),
            source,
        })?;
        let work_dir = work_dir.into();
        fs::create_dir_all(&work_dir).map_err(|source| Error::Io {
            path: work_dir.clone(),
            source,
        })?;
        Ok(Self {
            toolchain,
            locator: PackageLocator::new(target),
            explicit_target: explicit_target.into_iter().collect(),
            locations: HashMap::new(),
            images: HashMap::new(),
            cache_dir,
            work_dir,
            target_fingerprint,
            validations: AtomicUsize::new(0),
        })
    }

    pub fn target(&self) -> &TargetEnvironment {
        self.locator.target()
    }

    fn cache_path(&self, package: &InstalledPackage) -> PathBuf {
        let library = package.id.library.to_string_lossy().into_owned();
        let key = fingerprint_strings([
            self.target_fingerprint.as_str(),
            AIR_VERSION,
            ANALYSIS_SCHEMA,
            package.id.name.as_str(),
            package.id.version.as_str(),
            library.as_str(),
            package.id.image_fingerprint.0.as_str(),
        ]);
        self.cache_dir
            .join(format!("{}-{}.hrm", package.id.name, key.0))
    }

    fn inspect_image(&self, package: &InstalledPackage, output: &Path) -> Result<()> {
        if let Some(parent) = output.parent() {
            fs::create_dir_all(parent).map_err(|source| Error::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        let mut args = Vec::<OsString>::new();
        args.push(package.id.library.as_os_str().to_os_string());
        args.push(OsString::from(&package.id.name));
        args.push(output.as_os_str().to_os_string());
        for library in &self.locator.target().libraries {
            args.push(library.as_os_str().to_os_string());
        }
        self.toolchain
            .run_runtime_timeout(
                &self.work_dir,
                "inspect-image",
                args,
                false,
                300,
                format!(
                    "package: {} {}; phase: installed-image inspection",
                    package.id.name, package.id.version
                ),
            )
            .map_err(|error| {
                Error::Analysis(format!(
                    "installed-image inspection failed for {} {}: {error}",
                    package.id.name, package.id.version
                ))
            })?;
        Ok(())
    }
}

impl PackageStore {
    fn load_cached_image(
        &mut self,
        package: &InstalledPackage,
    ) -> Result<Option<Arc<PackageImage>>> {
        if let Some(image) = self.images.get(&package.id) {
            return Ok(Some(Arc::clone(image)));
        }
        let cache = self.cache_path(package);
        if !cache.is_file() {
            return Ok(None);
        }
        let text = fs::read_to_string(&cache).map_err(|source| Error::Io {
            path: cache.clone(),
            source,
        })?;
        match parse_package_image(&text, package.clone()) {
            Ok(image) => {
                let image = Arc::new(image);
                self.images.insert(package.id.clone(), Arc::clone(&image));
                Ok(Some(image))
            }
            Err(_) => {
                let _ = fs::remove_file(cache);
                Ok(None)
            }
        }
    }
}

impl PackageProvider for PackageStore {
    fn locate(&mut self, name: &str) -> Result<InstalledPackage> {
        if let Some(package) = self.locations.get(name) {
            return Ok(package.clone());
        }
        eprintln!("[hrm] index {name}");
        let package = self.locator.locate(name)?;
        self.locations.insert(name.to_owned(), package.clone());
        Ok(package)
    }

    fn image(&mut self, package: &InstalledPackage) -> Result<Arc<PackageImage>> {
        if let Some(image) = self.load_cached_image(package)? {
            return Ok(image);
        }
        let cache = self.cache_path(package);
        eprintln!("[hrm] inspect {}", package.id.name);
        self.inspect_image(package, &cache)?;
        let text = fs::read_to_string(&cache).map_err(|source| Error::Io {
            path: cache.clone(),
            source,
        })?;
        let image = Arc::new(parse_package_image(&text, package.clone())?);
        self.images.insert(package.id.clone(), Arc::clone(&image));
        Ok(image)
    }

    fn prefetch(&mut self, packages: &[InstalledPackage], jobs: usize) -> Result<()> {
        let mut missing = Vec::new();
        for package in packages {
            if self.load_cached_image(package)?.is_none() {
                missing.push(package.clone());
            }
        }
        if missing.len() < 2 || jobs <= 1 {
            return Ok(());
        }
        let manifest = self.work_dir.join("image-prefetch.hrm");
        let mut text = String::new();
        for library in &self.locator.target().libraries {
            text.push_str("LIB\t");
            text.push_str(&encode_hex(library.to_string_lossy().as_bytes()));
            text.push('\n');
        }
        for package in &missing {
            text.push_str("JOB\t");
            text.push_str(&encode_hex(package.id.library.to_string_lossy().as_bytes()));
            text.push('\t');
            text.push_str(&encode_hex(package.id.name.as_bytes()));
            text.push('\t');
            text.push_str(&encode_hex(
                self.cache_path(package).to_string_lossy().as_bytes(),
            ));
            text.push('\n');
        }
        fs::write(&manifest, text).map_err(|source| Error::Io {
            path: manifest.clone(),
            source,
        })?;
        eprintln!(
            "[hrm] inspect {} demanded packages in parallel",
            missing.len()
        );
        let job_count = jobs.min(missing.len()).to_string();
        self.toolchain
            .run_runtime_timeout(
                &self.work_dir,
                "inspect-image-batch",
                [manifest.as_os_str(), std::ffi::OsStr::new(&job_count)],
                false,
                600,
                format!(
                    "phase: installed-image batch inspection; packages: {}",
                    missing
                        .iter()
                        .map(|package| package.id.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            )
            .map_err(|error| {
                Error::Analysis(format!("installed-image batch inspection failed: {error}"))
            })?;
        for package in &missing {
            let cache = self.cache_path(package);
            let text = fs::read_to_string(&cache).map_err(|source| Error::Io {
                path: cache.clone(),
                source,
            })?;
            let image = Arc::new(parse_package_image(&text, package.clone())?);
            self.images.insert(package.id.clone(), image);
        }
        Ok(())
    }

    fn is_target_provided(&self, package: &InstalledPackage) -> bool {
        self.explicit_target.contains(&package.id.name)
            || package
                .description
                .get("Priority")
                .is_some_and(|priority| priority.eq_ignore_ascii_case("base"))
    }

    fn validate_syntax(
        &self,
        id: &PackageId,
        binding: &str,
        source: &str,
    ) -> Result<SyntaxValidation> {
        let ordinal = self.validations.fetch_add(1, Ordering::Relaxed);
        let dir = self.work_dir.join("syntax-validation");
        fs::create_dir_all(&dir).map_err(|source_error| Error::Io {
            path: dir.clone(),
            source: source_error,
        })?;
        let source_path = dir.join(format!("{ordinal}.txt"));
        let result_path = dir.join(format!("{ordinal}.result"));
        fs::write(&source_path, source).map_err(|source_error| Error::Io {
            path: source_path.clone(),
            source: source_error,
        })?;
        let args = [source_path.as_os_str(), result_path.as_os_str()];
        let result = self
            .toolchain
            .run_runtime_timeout(
                &self.work_dir,
                "validate-syntax",
                args,
                false,
                30,
                format!(
                    "package: {}; binding: {binding}; phase: target-R syntax validation",
                    id.name
                ),
            )
            .map_err(|error| {
                Error::Analysis(format!(
                    "target-R syntax validation failed for {}::{binding}: {error}",
                    id.name
                ))
            });
        let _ = fs::remove_file(&source_path);
        result?;
        let text = fs::read_to_string(&result_path).map_err(|source_error| Error::Io {
            path: result_path.clone(),
            source: source_error,
        })?;
        let _ = fs::remove_file(&result_path);
        if text.trim() == "OK" {
            Ok(SyntaxValidation::Accepted)
        } else if let Some(encoded) = text.trim().strip_prefix("ERROR\t") {
            Ok(SyntaxValidation::Rejected(decode_hex(encoded)?))
        } else {
            Err(Error::Analysis(format!(
                "invalid syntax-validation result {text:?}"
            )))
        }
    }
}

fn default_cache_dir() -> PathBuf {
    if let Some(path) = std::env::var_os("HRM_CACHE_DIR") {
        return PathBuf::from(path);
    }
    if cfg!(windows) {
        if let Some(path) = std::env::var_os("LOCALAPPDATA") {
            return PathBuf::from(path).join("heRmetic").join("cache");
        }
    } else if let Some(path) = std::env::var_os("XDG_CACHE_HOME") {
        return PathBuf::from(path).join("heRmetic");
    } else if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home).join(".cache").join("heRmetic");
    }
    std::env::temp_dir().join("heRmetic-cache")
}

fn encode_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

fn decode_hex(value: &str) -> Result<String> {
    if value.len() % 2 != 0 {
        return Err(Error::Analysis("odd-length syntax diagnostic".into()));
    }
    let bytes = value.as_bytes();
    let mut output = Vec::with_capacity(bytes.len() / 2);
    let mut index = 0;
    while index < bytes.len() {
        let high = nibble(bytes[index])
            .ok_or_else(|| Error::Analysis("invalid syntax diagnostic".into()))?;
        let low = nibble(bytes[index + 1])
            .ok_or_else(|| Error::Analysis("invalid syntax diagnostic".into()))?;
        output.push((high << 4) | low);
        index += 2;
    }
    String::from_utf8(output)
        .map_err(|error| Error::Analysis(format!("invalid syntax diagnostic UTF-8: {error}")))
}

fn nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}
