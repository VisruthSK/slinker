use crate::cache::Cache;
use crate::package::locator::fingerprint_strings;
use crate::package::{
    InstalledPackage, LifecycleMetadata, NativeFacts, NativeRoutineSummary, NativeSafety,
    PackageId, PackageImage, PackageIndex, PackageLocator,
};
use crate::r_worker::client::WorkerClient;
use crate::r_worker::protocol::{WorkerBinding, WorkerPackageIndex};
use crate::{Error, Result, TargetEnvironment};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

const AIR_VERSION: &str = "0.11.0";
const ANALYSIS_SCHEMA: &str = "slinker-analysis-v4";

#[derive(Deserialize, Serialize)]
struct CachedIndex {
    schema: String,
    target: String,
    package_fingerprint: String,
    index: WorkerPackageIndex,
}

#[derive(Deserialize, Serialize)]
struct CachedBinding {
    schema: String,
    target: String,
    package_fingerprint: String,
    binding_name: String,
    binding: WorkerBinding,
}

#[derive(Debug, Default, Deserialize)]
struct NativeSummaryManifest {
    schema: u32,
    #[serde(default)]
    packages: Vec<NativePackageSummary>,
}

#[derive(Debug, Deserialize)]
struct NativePackageSummary {
    package: String,
    version: String,
    image_fingerprint: String,
    components: Vec<NativeComponentSummary>,
}

#[derive(Debug, Deserialize)]
struct NativeComponentSummary {
    component: String,
    #[serde(flatten)]
    safety: NativeSummarySafety,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "safety", rename_all = "snake_case")]
enum NativeSummarySafety {
    Safe {
        #[serde(default)]
        callbacks: Vec<String>,
    },
    Summarized {
        routines: Vec<NativeRoutineSummaryRecord>,
    },
    Unsupported {
        effects: Vec<String>,
    },
}

#[derive(Debug, Deserialize)]
struct NativeRoutineSummaryRecord {
    selector: String,
    #[serde(default)]
    callback_arguments: Vec<usize>,
}

impl NativeSummaryManifest {
    fn load() -> Result<Self> {
        let Some(path) = std::env::var_os("SLINKER_NATIVE_SUMMARIES") else {
            return Ok(Self::default());
        };
        let path = PathBuf::from(path);
        let text = fs::read_to_string(&path).map_err(|source| Error::Io {
            path: path.clone(),
            source,
        })?;
        let manifest: Self = serde_json::from_str(&text).map_err(|error| {
            Error::Analysis(format!(
                "invalid native summary manifest {}: {error}",
                path.display()
            ))
        })?;
        if manifest.schema != 1 {
            return Err(Error::Analysis(format!(
                "unsupported native summary manifest schema {} in {}",
                manifest.schema,
                path.display()
            )));
        }
        manifest.validate()?;
        Ok(manifest)
    }

    fn validate(&self) -> Result<()> {
        let mut packages = HashSet::new();
        for package in &self.packages {
            let key = (
                package.package.as_str(),
                package.version.as_str(),
                package.image_fingerprint.as_str(),
            );
            if !packages.insert(key) {
                return Err(Error::Analysis(format!(
                    "duplicate native summary package identity {} {} {}",
                    package.package, package.version, package.image_fingerprint
                )));
            }
            let mut components = HashSet::new();
            for component in &package.components {
                if !components.insert(component.component.as_str()) {
                    return Err(Error::Analysis(format!(
                        "duplicate native component summary {}::{}",
                        package.package, component.component
                    )));
                }
                if let NativeSummarySafety::Summarized { routines } = &component.safety {
                    let mut selectors = HashSet::new();
                    for routine in routines {
                        if routine.selector.is_empty() || !selectors.insert(&routine.selector) {
                            return Err(Error::Analysis(format!(
                                "empty or duplicate native selector in {}::{}",
                                package.package, component.component
                            )));
                        }
                        if routine.callback_arguments.contains(&0) {
                            return Err(Error::Analysis(format!(
                                "native callback positions are one-based in {}::{} `{}`",
                                package.package, component.component, routine.selector
                            )));
                        }
                    }
                }
            }
        }
        Ok(())
    }

    fn apply(&self, index: &mut PackageIndex) {
        let Some(package) = self.packages.iter().find(|summary| {
            summary.package == index.package.id.name
                && summary.version == index.package.id.version.as_ref()
                && summary.image_fingerprint == index.package.id.image_fingerprint.0
        }) else {
            return;
        };
        for summary in &package.components {
            let Some(component) = index
                .dynlibs
                .iter_mut()
                .find(|component| component.name == summary.component)
            else {
                continue;
            };
            component.safety = match &summary.safety {
                NativeSummarySafety::Safe { callbacks } => NativeSafety::Safe(NativeFacts {
                    callbacks: callbacks.clone(),
                }),
                NativeSummarySafety::Summarized { routines } => NativeSafety::Summarized(
                    routines
                        .iter()
                        .map(|routine| NativeRoutineSummary {
                            selector: routine.selector.clone(),
                            callback_arguments: routine.callback_arguments.clone(),
                        })
                        .collect(),
                ),
                NativeSummarySafety::Unsupported { effects } => {
                    NativeSafety::Unsupported(effects.clone())
                }
            };
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SyntaxValidation {
    Accepted,
    Rejected(String),
}

pub trait PackageProvider {
    fn target_environment(&self) -> Option<&TargetEnvironment> {
        None
    }
    fn locate(&mut self, name: &str) -> Result<InstalledPackage>;
    fn locate_optional(&mut self, name: &str) -> Result<Option<InstalledPackage>>;
    fn locate_many(&mut self, names: &[String], _jobs: usize) -> Result<Vec<InstalledPackage>> {
        names.iter().map(|name| self.locate(name)).collect()
    }
    fn index(&mut self, package: &InstalledPackage) -> Result<Arc<PackageIndex>>;
    fn binding_image(
        &mut self,
        package: &InstalledPackage,
        name: &str,
    ) -> Result<Arc<PackageImage>>;
    fn prefetch_indexes(&mut self, _packages: &[InstalledPackage], _jobs: usize) -> Result<()> {
        Ok(())
    }
    fn resource_exists(&mut self, package: &InstalledPackage, path: &str) -> Result<bool> {
        Ok(self
            .index(package)?
            .files
            .iter()
            .any(|candidate| candidate == path))
    }
    fn validate_syntax(
        &mut self,
        id: &PackageId,
        binding: &str,
        source: &str,
    ) -> Result<SyntaxValidation>;
    fn normalize_syntax(&mut self, source: &str) -> Result<String>;
}

pub struct PackageStore {
    locator: PackageLocator,
    locations: HashMap<String, InstalledPackage>,
    indexes: HashMap<PackageId, Arc<PackageIndex>>,
    bindings: HashMap<(PackageId, String), Arc<PackageImage>>,
    cache: Cache,
    r_home: PathBuf,
    target_fingerprint: String,
    worker: Option<WorkerClient>,
    locate_pool: Option<(usize, Arc<rayon::ThreadPool>)>,
    native_summaries: NativeSummaryManifest,
}

impl PackageStore {
    pub fn new(r_home: PathBuf, target: TargetEnvironment) -> Result<Self> {
        let target_fingerprint = fingerprint_strings(
            std::iter::once(target.r_home.to_string_lossy().into_owned())
                .chain(std::iter::once(format!(
                    "R={} OS={} ARCH={}",
                    target.target.r_version, target.target.os, target.target.arch
                )))
                .chain(
                    target
                        .libraries
                        .iter()
                        .map(|path| path.to_string_lossy().into_owned()),
                ),
        )
        .0;
        let locator = PackageLocator::new(target);
        let cache = Cache::new(ANALYSIS_SCHEMA)?;
        let native_summaries = NativeSummaryManifest::load()?;
        Ok(Self {
            locator,
            locations: HashMap::new(),
            indexes: HashMap::new(),
            bindings: HashMap::new(),
            cache,
            r_home,
            target_fingerprint,
            worker: None,
            locate_pool: None,
            native_summaries,
        })
    }

    fn package_index(
        &self,
        worker: WorkerPackageIndex,
        package: InstalledPackage,
    ) -> Result<Arc<PackageIndex>> {
        if worker.name != package.id.name
            || worker.version != package.id.version.to_string()
            || worker.image_fingerprint != package.id.image_fingerprint.0
        {
            return Err(Error::Analysis(format!(
                "installed index identity changed while inspecting {}",
                package.id.name
            )));
        }
        let mut index = PackageIndex {
            package: package.clone(),
            description: package.description.clone(),
            exports: worker.exports,
            imports: worker.imports,
            s3: worker.s3,
            dynlibs: worker.dynlibs,
            lifecycle: LifecycleMetadata {
                on_load: worker.on_load,
            },
            binding_names: worker.binding_names,
            datasets: worker.datasets,
            files: Vec::new(),
            has_sysdata: worker.has_sysdata,
        };
        self.native_summaries.apply(&mut index);
        Ok(Arc::new(index))
    }

    fn package_image(
        &self,
        package: InstalledPackage,
        index: Arc<PackageIndex>,
        worker: WorkerBinding,
    ) -> Result<Arc<PackageImage>> {
        if worker.package_name != package.id.name
            || worker.package_version != package.id.version.to_string()
            || worker.image_fingerprint != package.id.image_fingerprint.0
            || !index
                .binding_names
                .iter()
                .any(|name| name == &worker.binding.name)
        {
            return Err(Error::Analysis(format!(
                "worker returned unindexed binding {}::{}",
                package.id.name, worker.binding.name
            )));
        }
        Ok(Arc::new(PackageImage {
            index: (*index).clone(),
            bindings: HashMap::from([(worker.binding.name.clone(), worker.binding)]),
            private_environments: worker.private_environments,
        }))
    }

    pub fn target(&self) -> &TargetEnvironment {
        self.locator.target()
    }

    fn cache_key(&self, package: &InstalledPackage) -> String {
        let key = fingerprint_strings([
            self.target_fingerprint.as_str(),
            AIR_VERSION,
            ANALYSIS_SCHEMA,
            package.id.name.as_str(),
            package.id.version.as_ref(),
            package.id.image_fingerprint.0.as_str(),
        ]);
        key.0
    }

    fn index_cache_path(&self, package: &InstalledPackage) -> PathBuf {
        self.cache.path(format!(
            "{}-{}.index.slinker",
            package.id.name,
            self.cache_key(package)
        ))
    }

    fn binding_cache_path(&self, package: &InstalledPackage, binding: &str) -> PathBuf {
        let binding = fingerprint_strings([binding]).0;
        self.cache.path(format!(
            "{}-{}-{binding}.binding.slinker",
            package.id.name,
            self.cache_key(package)
        ))
    }

    fn worker(&mut self) -> Result<&mut WorkerClient> {
        if self.worker.is_none() {
            let target = self.locator.target().clone();
            self.worker = Some(WorkerClient::spawn(self.r_home.clone(), &target)?);
        }
        Ok(self.worker.as_mut().expect("Harp worker initialized"))
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
        let cached = self.cache.read::<CachedIndex>(&cache).filter(|entry| {
            entry.schema == ANALYSIS_SCHEMA
                && entry.target == self.target_fingerprint
                && entry.package_fingerprint == package.id.image_fingerprint.0
        });
        match cached.map(|entry| self.package_index(entry.index, package.clone())) {
            Some(Ok(index)) => {
                self.indexes.insert(package.id.clone(), Arc::clone(&index));
                Ok(Some(index))
            }
            _ => Ok(None),
        }
    }

    fn load_cached_binding(
        &mut self,
        package: &InstalledPackage,
        binding: &str,
    ) -> Result<Option<Arc<PackageImage>>> {
        let key = (package.id.clone(), binding.to_owned());
        if let Some(image) = self.bindings.get(&key) {
            return Ok(Some(Arc::clone(image)));
        }
        let cache = self.binding_cache_path(package, binding);
        if !cache.is_file() {
            return Ok(None);
        }
        let cached = self.cache.read::<CachedBinding>(&cache).filter(|entry| {
            entry.schema == ANALYSIS_SCHEMA
                && entry.target == self.target_fingerprint
                && entry.package_fingerprint == package.id.image_fingerprint.0
                && entry.binding_name == binding
        });
        match cached {
            Some(cached) => {
                let index = self.index(package)?;
                let image = self.package_image(package.clone(), index, cached.binding)?;
                self.bindings.insert(key, Arc::clone(&image));
                Ok(Some(image))
            }
            None => Ok(None),
        }
    }
}

impl PackageProvider for PackageStore {
    fn target_environment(&self) -> Option<&TargetEnvironment> {
        Some(self.locator.target())
    }
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
        let worker = self.worker()?.package_index(package)?;
        let index = self.package_index(worker.clone(), package.clone())?;
        let cache = self.index_cache_path(package);
        let cached = CachedIndex {
            schema: ANALYSIS_SCHEMA.into(),
            target: self.target_fingerprint.clone(),
            package_fingerprint: package.id.image_fingerprint.0.clone(),
            index: worker,
        };
        self.cache.publish(&cache, &cached);
        self.indexes.insert(package.id.clone(), Arc::clone(&index));
        Ok(index)
    }

    fn binding_image(
        &mut self,
        package: &InstalledPackage,
        name: &str,
    ) -> Result<Arc<PackageImage>> {
        if let Some(image) = self.load_cached_binding(package, name)? {
            return Ok(image);
        }
        let index = self.index(package)?;
        let binding = self.worker()?.binding(package, name)?;
        let cache = self.binding_cache_path(package, name);
        let cached = CachedBinding {
            schema: ANALYSIS_SCHEMA.into(),
            target: self.target_fingerprint.clone(),
            package_fingerprint: package.id.image_fingerprint.0.clone(),
            binding_name: name.into(),
            binding: binding.clone(),
        };
        self.cache.publish(&cache, &cached);
        let image = self.package_image(package.clone(), index, binding)?;
        self.bindings
            .insert((package.id.clone(), name.to_owned()), Arc::clone(&image));
        Ok(image)
    }

    fn prefetch_indexes(&mut self, packages: &[InstalledPackage], _jobs: usize) -> Result<()> {
        for package in packages {
            self.index(package)?;
        }
        Ok(())
    }

    fn resource_exists(&mut self, package: &InstalledPackage, path: &str) -> Result<bool> {
        use std::path::Component;
        if path.is_empty() {
            return Ok(package.location.root.is_dir());
        }
        let relative = Path::new(path);
        if relative.is_absolute()
            || relative
                .components()
                .any(|component| !matches!(component, Component::Normal(_) | Component::CurDir))
        {
            return Ok(false);
        }
        Ok(package.location.root.join(relative).exists())
    }

    fn validate_syntax(
        &mut self,
        _id: &PackageId,
        _binding: &str,
        source: &str,
    ) -> Result<SyntaxValidation> {
        self.worker()?.validate_syntax(source)
    }

    fn normalize_syntax(&mut self, source: &str) -> Result<String> {
        self.worker()?.normalize_syntax(source)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Description;
    use crate::package::{Digest, LifecycleMetadata, NativeComponent, PackageLocation};

    fn package_index() -> PackageIndex {
        let description = Description::parse("Package: fixture\nVersion: 1.0.0\n");
        PackageIndex {
            package: InstalledPackage {
                id: PackageId {
                    name: "fixture".into(),
                    version: "1.0.0".parse().expect("version"),
                    image_fingerprint: Digest("exact-image".into()),
                },
                location: PackageLocation {
                    library: PathBuf::from("/library"),
                    root: PathBuf::from("/library/fixture"),
                },
                description: description.clone(),
            },
            description,
            exports: Default::default(),
            imports: Vec::new(),
            s3: Vec::new(),
            dynlibs: vec![NativeComponent {
                name: "fixture".into(),
                registration: None,
                symbols: Vec::new(),
                safety: NativeSafety::Unanalyzed,
            }],
            lifecycle: LifecycleMetadata::default(),
            binding_names: Vec::new(),
            datasets: Vec::new(),
            files: Vec::new(),
            has_sysdata: false,
        }
    }

    #[test]
    fn exact_image_native_manifest_attaches_routine_callbacks() {
        let manifest: NativeSummaryManifest = serde_json::from_str(
            r#"{
                "schema": 1,
                "packages": [{
                    "package": "fixture",
                    "version": "1.0.0",
                    "image_fingerprint": "exact-image",
                    "components": [{
                        "component": "fixture",
                        "safety": "summarized",
                        "routines": [{"selector": "fixture_call", "callback_arguments": [2]}]
                    }]
                }]
            }"#,
        )
        .expect("manifest");
        manifest.validate().expect("valid manifest");
        let mut index = package_index();
        manifest.apply(&mut index);

        assert!(matches!(
            &index.dynlibs[0].safety,
            NativeSafety::Summarized(routines)
                if routines == &[NativeRoutineSummary {
                    selector: "fixture_call".into(),
                    callback_arguments: vec![2],
                }]
        ));
    }

    #[test]
    fn native_manifest_rejects_zero_callback_position() {
        let manifest: NativeSummaryManifest = serde_json::from_str(
            r#"{
                "schema": 1,
                "packages": [{
                    "package": "fixture",
                    "version": "1.0.0",
                    "image_fingerprint": "exact-image",
                    "components": [{
                        "component": "fixture",
                        "safety": "summarized",
                        "routines": [{"selector": "fixture_call", "callback_arguments": [0]}]
                    }]
                }]
            }"#,
        )
        .expect("manifest");
        assert!(manifest.validate().is_err());
    }
}
