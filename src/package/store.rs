use crate::package::image::parse_package_image;
use crate::package::index::parse_package_index;
use crate::package::locator::fingerprint_strings;
use crate::package::{InstalledPackage, PackageId, PackageImage, PackageIndex, PackageLocator};
use crate::toolchain::RRuntimeServer;
use crate::{Error, RToolchain, Result, TargetEnvironment};
use rayon::prelude::*;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

const AIR_VERSION: &str = "0.11.0";
const ANALYSIS_SCHEMA: &str = "slinker-object-environment-v3";

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SyntaxValidation {
    Accepted,
    Rejected(String),
}

pub trait PackageProvider {
    fn locate(&mut self, name: &str) -> Result<InstalledPackage>;
    fn locate_optional(&mut self, name: &str) -> Result<Option<InstalledPackage>>;
    fn locate_many(&mut self, names: &[String], _jobs: usize) -> Result<Vec<InstalledPackage>> {
        names.iter().map(|name| self.locate(name)).collect()
    }
    fn index(&mut self, package: &InstalledPackage) -> Result<Arc<PackageIndex>>;
    fn image(&mut self, package: &InstalledPackage) -> Result<Arc<PackageImage>>;
    fn prefetch_indexes(&mut self, _packages: &[InstalledPackage], _jobs: usize) -> Result<()> {
        Ok(())
    }
    fn prefetch(&mut self, _packages: &[InstalledPackage], _jobs: usize) -> Result<()> {
        Ok(())
    }
    fn is_target_provided(&self, package: &InstalledPackage) -> bool;
    fn is_base_binding(&self, name: &str) -> bool;
    fn resource_exists(&mut self, package: &InstalledPackage, path: &str) -> Result<bool> {
        Ok(self
            .index(package)?
            .files
            .iter()
            .any(|candidate| candidate == path))
    }
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
    explicit_target: HashSet<PackageId>,
    locations: HashMap<String, InstalledPackage>,
    indexes: HashMap<PackageId, Arc<PackageIndex>>,
    images: HashMap<PackageId, Arc<PackageImage>>,
    cache_dir: PathBuf,
    work_dir: PathBuf,
    target_fingerprint: String,
    jobs: usize,
    runtime_server: Option<RRuntimeServer>,
    locate_pool: Option<(usize, Arc<rayon::ThreadPool>)>,
    validations: AtomicUsize,
}

impl PackageStore {
    pub fn new(
        toolchain: RToolchain,
        target: TargetEnvironment,
        explicit_target: impl IntoIterator<Item = String>,
        jobs: usize,
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
        let locator = PackageLocator::new(target);
        let mut explicit_target_ids = HashSet::new();
        let mut locations = HashMap::new();
        for name in explicit_target {
            let package = locator.locate(&name)?;
            explicit_target_ids.insert(package.id.clone());
            locations.insert(name, package);
        }
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
            locator,
            explicit_target: explicit_target_ids,
            locations,
            indexes: HashMap::new(),
            images: HashMap::new(),
            cache_dir,
            work_dir,
            target_fingerprint,
            jobs: jobs.max(1),
            runtime_server: None,
            locate_pool: None,
            validations: AtomicUsize::new(0),
        })
    }

    pub fn target(&self) -> &TargetEnvironment {
        self.locator.target()
    }

    fn cache_key(&self, package: &InstalledPackage) -> String {
        let library = package.id.library.to_string_lossy().into_owned();
        let key = fingerprint_strings([
            self.target_fingerprint.as_str(),
            AIR_VERSION,
            ANALYSIS_SCHEMA,
            package.id.name.as_str(),
            &package.id.version.to_string(),
            library.as_str(),
            package.id.image_fingerprint.0.as_str(),
        ]);
        key.0
    }

    fn index_cache_path(&self, package: &InstalledPackage) -> PathBuf {
        self.cache_dir.join(format!(
            "{}-{}.index.slinker",
            package.id.name,
            self.cache_key(package)
        ))
    }

    fn image_cache_path(&self, package: &InstalledPackage) -> PathBuf {
        self.cache_dir.join(format!(
            "{}-{}.image.slinker",
            package.id.name,
            self.cache_key(package)
        ))
    }

    fn persist_index_from_image(&self, package: &InstalledPackage, image_text: &str) -> Result<()> {
        let path = self.index_cache_path(package);
        if path.is_file() {
            return Ok(());
        }
        let mut index = String::new();
        for line in image_text.lines() {
            if line.is_empty() {
                continue;
            }
            let kind = line.split('\t').next().unwrap_or_default();
            match kind {
                "HEADER" | "EXPORT" | "IMPORT_ALL" | "IMPORT_EXCEPT" | "IMPORT_FROM"
                | "DATASET" | "S3" | "DYNLIB" | "NATIVE_SYMBOL" | "FILE" | "PACKAGE_ISSUE" => {
                    index.push_str(line);
                    index.push('\n');
                }
                "BINDING" => {
                    if let Some(name) = line.split('\t').nth(1) {
                        index.push_str("BINDING_NAME\t");
                        index.push_str(name);
                        index.push('\n');
                    }
                }
                "CLOSURE" | "NESTED_CLOSURE" | "BINDING_ISSUE" => {}
                _ => {}
            }
        }
        fs::write(&path, index).map_err(|source| Error::Io { path, source })
    }

    fn runtime_server(&mut self) -> Result<&mut RRuntimeServer> {
        if self.runtime_server.is_none() {
            let server = self
                .toolchain
                .spawn_runtime_server(&self.work_dir, self.jobs)
                .map_err(|error| {
                    Error::Analysis(format!("failed to start persistent R runtime: {error}"))
                })?;
            self.runtime_server = Some(server);
        }
        Ok(self
            .runtime_server
            .as_mut()
            .expect("runtime server initialized"))
    }

    fn write_manifest(
        &self,
        name: &str,
        packages: &[(InstalledPackage, PathBuf)],
    ) -> Result<PathBuf> {
        let manifest = self.work_dir.join(format!("{name}.slinker"));
        let mut text = String::new();
        for library in &self.locator.target().libraries {
            text.push_str("LIB\t");
            text.push_str(&encode_hex(library.to_string_lossy().as_bytes()));
            text.push('\n');
        }
        for (package, output) in packages {
            text.push_str("JOB\t");
            text.push_str(&encode_hex(package.id.library.to_string_lossy().as_bytes()));
            text.push('\t');
            text.push_str(&encode_hex(package.id.name.as_bytes()));
            text.push('\t');
            text.push_str(&encode_hex(output.to_string_lossy().as_bytes()));
            text.push('\n');
        }
        fs::write(&manifest, text).map_err(|source| Error::Io {
            path: manifest.clone(),
            source,
        })?;
        Ok(manifest)
    }

    fn inspect_index(&mut self, package: &InstalledPackage, output: &Path) -> Result<()> {
        if let Some(parent) = output.parent() {
            fs::create_dir_all(parent).map_err(|source| Error::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        let manifest =
            self.write_manifest("index-request", &[(package.clone(), output.to_path_buf())])?;
        self.runtime_server()?
            .request(
                "INDEX",
                &manifest,
                60,
                format!(
                    "package: {} {}; phase: installed-image index",
                    package.id.name, package.id.version
                ),
            )
            .map_err(|error| {
                Error::Analysis(format!(
                    "installed-image index failed for {} {}: {error}",
                    package.id.name, package.id.version
                ))
            })
    }

    fn inspect_image(&mut self, package: &InstalledPackage, output: &Path) -> Result<()> {
        if let Some(parent) = output.parent() {
            fs::create_dir_all(parent).map_err(|source| Error::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        let manifest =
            self.write_manifest("image-request", &[(package.clone(), output.to_path_buf())])?;
        self.runtime_server()?
            .request(
                "IMAGE",
                &manifest,
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
            })
    }

    fn load_cached_index(
        &mut self,
        package: &InstalledPackage,
    ) -> Result<Option<Arc<PackageIndex>>> {
        if let Some(index) = self.indexes.get(&package.id) {
            return Ok(Some(Arc::clone(index)));
        }
        let cache = self.index_cache_path(package);
        if !cache.is_file() {
            return Ok(None);
        }
        let text = fs::read_to_string(&cache).map_err(|source| Error::Io {
            path: cache.clone(),
            source,
        })?;
        match parse_package_index(&text, package.clone()) {
            Ok(index) => {
                let index = Arc::new(index);
                self.indexes.insert(package.id.clone(), Arc::clone(&index));
                Ok(Some(index))
            }
            Err(_) => {
                let _ = fs::remove_file(cache);
                Ok(None)
            }
        }
    }

    fn load_cached_image(
        &mut self,
        package: &InstalledPackage,
    ) -> Result<Option<Arc<PackageImage>>> {
        if let Some(image) = self.images.get(&package.id) {
            return Ok(Some(Arc::clone(image)));
        }
        let cache = self.image_cache_path(package);
        if !cache.is_file() {
            return Ok(None);
        }
        let text = fs::read_to_string(&cache).map_err(|source| Error::Io {
            path: cache.clone(),
            source,
        })?;
        match parse_package_image(&text, package.clone()) {
            Ok(image) => {
                self.persist_index_from_image(package, &text)?;
                let image = Arc::new(image);
                self.indexes
                    .insert(package.id.clone(), Arc::new(image.index.clone()));
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
        eprintln!("[slinker] index {name}");
        let package = self.locator.locate(name)?;
        self.locations.insert(name.to_owned(), package.clone());
        Ok(package)
    }

    fn locate_optional(&mut self, name: &str) -> Result<Option<InstalledPackage>> {
        if let Some(package) = self.locations.get(name) {
            return Ok(Some(package.clone()));
        }
        let Some(package) = self.locator.locate_optional(name)? else {
            return Ok(None);
        };
        eprintln!("[slinker] index {name}");
        self.locations.insert(name.to_owned(), package.clone());
        Ok(Some(package))
    }

    fn locate_many(&mut self, names: &[String], jobs: usize) -> Result<Vec<InstalledPackage>> {
        if names.is_empty() {
            return Ok(Vec::new());
        }

        let mut missing = Vec::new();
        let mut seen = HashSet::new();
        for name in names {
            if !self.locations.contains_key(name) && seen.insert(name.clone()) {
                missing.push(name.clone());
            }
        }

        if !missing.is_empty() {
            let locator = self.locator.clone();
            let locate = || {
                missing
                    .par_iter()
                    .map(|name| (name.clone(), locator.locate(name)))
                    .collect::<Vec<_>>()
            };
            let results = if jobs > 1 && missing.len() > 1 {
                let threads = jobs.min(missing.len());
                let rebuild = self
                    .locate_pool
                    .as_ref()
                    .is_none_or(|(configured, _)| *configured != threads);
                if rebuild {
                    let pool = rayon::ThreadPoolBuilder::new()
                        .num_threads(threads)
                        .thread_name(|index| format!("slinker-locate-{index}"))
                        .build()
                        .map_err(|error| {
                            Error::Analysis(format!(
                                "failed to create package locator pool: {error}"
                            ))
                        })?;
                    self.locate_pool = Some((threads, Arc::new(pool)));
                }
                self.locate_pool
                    .as_ref()
                    .expect("locator pool initialized")
                    .1
                    .install(locate)
            } else {
                missing
                    .iter()
                    .map(|name| (name.clone(), self.locator.locate(name)))
                    .collect()
            };

            for (name, package) in results {
                let package = package?;
                eprintln!("[slinker] index {name}");
                self.locations.insert(name, package);
            }
        }

        names
            .iter()
            .map(|name| {
                self.locations.get(name).cloned().ok_or_else(|| {
                    Error::Analysis(format!("package `{name}` disappeared after location"))
                })
            })
            .collect()
    }

    fn index(&mut self, package: &InstalledPackage) -> Result<Arc<PackageIndex>> {
        if let Some(index) = self.load_cached_index(package)? {
            return Ok(index);
        }
        if let Some(image) = self.images.get(&package.id) {
            let index = Arc::new(image.index.clone());
            self.indexes.insert(package.id.clone(), Arc::clone(&index));
            return Ok(index);
        }
        let cache = self.index_cache_path(package);
        self.inspect_index(package, &cache)?;
        let text = fs::read_to_string(&cache).map_err(|source| Error::Io {
            path: cache.clone(),
            source,
        })?;
        let index = Arc::new(parse_package_index(&text, package.clone())?);
        self.indexes.insert(package.id.clone(), Arc::clone(&index));
        Ok(index)
    }

    fn image(&mut self, package: &InstalledPackage) -> Result<Arc<PackageImage>> {
        if let Some(image) = self.load_cached_image(package)? {
            return Ok(image);
        }
        let cache = self.image_cache_path(package);
        eprintln!("[slinker] inspect {}", package.id.name);
        self.inspect_image(package, &cache)?;
        let text = fs::read_to_string(&cache).map_err(|source| Error::Io {
            path: cache.clone(),
            source,
        })?;
        let image = Arc::new(parse_package_image(&text, package.clone())?);
        self.persist_index_from_image(package, &text)?;
        self.indexes
            .insert(package.id.clone(), Arc::new(image.index.clone()));
        self.images.insert(package.id.clone(), Arc::clone(&image));
        Ok(image)
    }

    fn prefetch_indexes(&mut self, packages: &[InstalledPackage], _jobs: usize) -> Result<()> {
        let mut missing = Vec::new();
        for package in packages {
            if self.load_cached_index(package)?.is_none() {
                missing.push(package.clone());
            }
        }
        if missing.is_empty() {
            return Ok(());
        }

        let jobs = missing
            .iter()
            .map(|package| (package.clone(), self.index_cache_path(package)))
            .collect::<Vec<_>>();
        let manifest = self.write_manifest("index-prefetch", &jobs)?;
        if missing.len() > 1 {
            eprintln!(
                "[slinker] index {} demanded packages in parallel",
                missing.len()
            );
        }
        self.runtime_server()?
            .request(
                "INDEX",
                &manifest,
                180,
                format!(
                    "phase: installed-image index batch; packages: {}",
                    missing
                        .iter()
                        .map(|package| package.id.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            )
            .map_err(|error| {
                Error::Analysis(format!("installed-image index batch failed: {error}"))
            })?;

        for package in &missing {
            let cache = self.index_cache_path(package);
            let text = fs::read_to_string(&cache).map_err(|source| Error::Io {
                path: cache.clone(),
                source,
            })?;
            let index = Arc::new(parse_package_index(&text, package.clone())?);
            self.indexes.insert(package.id.clone(), index);
        }
        Ok(())
    }

    fn prefetch(&mut self, packages: &[InstalledPackage], _jobs: usize) -> Result<()> {
        let mut missing = Vec::new();
        for package in packages {
            if self.load_cached_image(package)?.is_none() {
                missing.push(package.clone());
            }
        }
        if missing.is_empty() {
            return Ok(());
        }

        let jobs = missing
            .iter()
            .map(|package| (package.clone(), self.image_cache_path(package)))
            .collect::<Vec<_>>();
        let manifest = self.write_manifest("image-prefetch", &jobs)?;
        if missing.len() > 1 {
            eprintln!(
                "[slinker] inspect {} demanded packages in parallel",
                missing.len()
            );
        }
        self.runtime_server()?
            .request(
                "IMAGE",
                &manifest,
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
            let cache = self.image_cache_path(package);
            let text = fs::read_to_string(&cache).map_err(|source| Error::Io {
                path: cache.clone(),
                source,
            })?;
            let image = Arc::new(parse_package_image(&text, package.clone())?);
            self.persist_index_from_image(package, &text)?;
            self.indexes
                .insert(package.id.clone(), Arc::new(image.index.clone()));
            self.images.insert(package.id.clone(), image);
        }
        Ok(())
    }

    fn resource_exists(&mut self, package: &InstalledPackage, path: &str) -> Result<bool> {
        use std::path::Component;
        if path.is_empty() {
            return Ok(package.id.root.is_dir());
        }
        let relative = Path::new(path);
        if relative.is_absolute()
            || relative
                .components()
                .any(|component| !matches!(component, Component::Normal(_) | Component::CurDir))
        {
            return Ok(false);
        }
        Ok(package.id.root.join(relative).exists())
    }

    fn is_target_provided(&self, package: &InstalledPackage) -> bool {
        self.explicit_target.contains(&package.id)
            || matches!(
                package.description.priority_parsed(),
                Some(Ok(crate::metadata::Priority::Base))
            )
    }

    fn is_base_binding(&self, name: &str) -> bool {
        self.locator.target().base_bindings.contains(name)
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
    if let Some(path) = std::env::var_os("SLINKER_CACHE_DIR") {
        return PathBuf::from(path);
    }
    if cfg!(windows) {
        if let Some(path) = std::env::var_os("LOCALAPPDATA") {
            return PathBuf::from(path).join("slinker").join("cache");
        }
    } else if let Some(path) = std::env::var_os("XDG_CACHE_HOME") {
        return PathBuf::from(path).join("slinker");
    } else if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home).join(".cache").join("slinker");
    }
    std::env::temp_dir().join("slinker-cache")
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
