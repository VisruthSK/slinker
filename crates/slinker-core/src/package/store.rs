use crate::cache::{Cache, CacheLocation};
use crate::package::locator::fingerprint_strings;
use crate::package::{
    BindingName, ComponentName, Digest, GenericName, InstalledPackage, LifecycleMetadata,
    NativeFacts, NativeRoutineSummary, NativeSafety, PackageData, PackageIdentity, PackageImage,
    PackageIndex, PackageLocator, PackageName,
};
use crate::worker::client::WorkerClient;
use crate::worker::protocol::{WorkerBinding, WorkerPackageIndex};
use crate::{Error, Result, TargetEnvironment};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

const AIR_VERSION: &str = "0.11.0";
const ANALYSIS_SCHEMA: &str = "slinker-analysis-v10";
const MAX_R_WORKERS: usize = 4;
const SOURCES_PER_WORKER: usize = 48;
const BINDING_BATCH: usize = 64;

#[derive(Deserialize, Serialize)]
struct CachedIndex {
    schema: String,
    target: Digest,
    package_fingerprint: Digest,
    index: WorkerPackageIndex,
}

#[derive(Deserialize, Serialize)]
struct CachedBinding {
    schema: String,
    target: Digest,
    package_fingerprint: Digest,
    binding_name: BindingName,
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
    package: PackageName,
    version: String,
    image_fingerprint: Digest,
    components: Vec<NativeComponentSummary>,
}

#[derive(Debug, Deserialize)]
struct NativeComponentSummary {
    component: ComponentName,
    #[serde(flatten)]
    safety: NativeSummarySafety,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "safety", rename_all = "snake_case")]
enum NativeSummarySafety {
    Safe {
        #[serde(default)]
        callbacks: Vec<BindingName>,
    },
    Summarized {
        routines: Vec<NativeRoutineSummary>,
    },
    Unsupported {
        effects: Vec<String>,
    },
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
            index.identity.name == summary.package
                && summary.version == index.identity.version.as_ref()
                && summary.image_fingerprint == index.identity.image_fingerprint
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
                NativeSummarySafety::Summarized { routines } => {
                    NativeSafety::Summarized(routines.clone())
                }
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CanonicalSyntax {
    Stable(String),
    Unstable,
}

#[derive(Clone, Copy, Debug)]
pub enum DispatchSubject<'a> {
    Base {
        binding: &'a str,
    },
    Installed {
        package: &'a InstalledPackage,
        binding: &'a str,
    },
}

pub trait PackageResolver {
    fn target_environment(&self) -> &TargetEnvironment;
    fn locate(&mut self, name: &str) -> Result<Option<InstalledPackage>>;
}

pub trait PackageProvider: PackageResolver {
    fn index(&mut self, package: &InstalledPackage) -> Result<Arc<PackageIndex>>;
    fn binding_image(
        &mut self,
        package: &InstalledPackage,
        name: &str,
    ) -> Result<Arc<PackageImage>>;
    fn resource_exists(&mut self, package: &InstalledPackage, path: &str) -> Result<bool> {
        Ok(self
            .index(package)?
            .files
            .iter()
            .any(|candidate| candidate == path))
    }
    fn dispatch_generics(&mut self, subject: DispatchSubject<'_>) -> Result<BTreeSet<GenericName>>;
    fn validate_syntax(&mut self, source: &str) -> Result<SyntaxValidation>;
    fn canonical_syntax(&mut self, source: &str) -> Result<CanonicalSyntax>;
    fn prefetch_canonical_syntax(&mut self, _sources: &[&str]) -> Result<()> {
        Ok(())
    }
    fn prefetch_binding_images(
        &mut self,
        _package: &InstalledPackage,
        _names: &[&str],
    ) -> Result<()> {
        Ok(())
    }
}

pub struct PackageStore {
    locator: PackageLocator,
    indexes: HashMap<PackageIdentity, Arc<PackageIndex>>,
    bindings: HashMap<(PackageIdentity, String), Arc<PackageImage>>,
    dispatch: HashMap<(Option<PackageIdentity>, String), BTreeSet<GenericName>>,
    cache: Cache,
    r_home: PathBuf,
    target_fingerprint: Digest,
    worker: Option<WorkerClient>,
    extra_workers: Vec<WorkerClient>,
    worker_limit: usize,
    syntax: HashMap<String, CanonicalSyntax>,
    native_summaries: NativeSummaryManifest,
}

impl PackageStore {
    pub fn new(r_home: PathBuf, target: TargetEnvironment, cache: CacheLocation) -> Result<Self> {
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
        );
        Ok(Self {
            locator: PackageLocator::new(target),
            indexes: HashMap::new(),
            bindings: HashMap::new(),
            dispatch: HashMap::new(),
            cache: Cache::new(cache, ANALYSIS_SCHEMA)?,
            r_home,
            target_fingerprint,
            worker: None,
            extra_workers: Vec::new(),
            worker_limit: 1,
            syntax: HashMap::new(),
            native_summaries: NativeSummaryManifest::load()?,
        })
    }

    #[must_use]
    pub fn with_worker_limit(mut self, limit: usize) -> Self {
        self.worker_limit = limit.clamp(1, MAX_R_WORKERS);
        self
    }

    fn package_index(
        &self,
        worker: WorkerPackageIndex,
        package: &InstalledPackage,
    ) -> Result<Arc<PackageIndex>> {
        let identity = &package.identity;
        if identity.name != worker.name
            || worker.version != identity.version.to_string()
            || worker.image_fingerprint != identity.image_fingerprint
        {
            return Err(Error::Analysis(format!(
                "installed index identity changed while inspecting {}",
                identity.name
            )));
        }
        let mut index = PackageIndex {
            identity: identity.clone(),
            description: package.description.clone(),
            exports: worker.exports,
            imports: worker.imports,
            s3: worker.s3,
            dynlibs: worker.dynlibs,
            lifecycle: LifecycleMetadata {
                on_load: worker.on_load,
            },
            binding_names: worker.binding_names.into(),
            data: PackageData::new(worker.data_sets, worker.data_storage),
            files: Vec::new(),
            has_sysdata: worker.has_sysdata,
        };
        self.native_summaries.apply(&mut index);
        Ok(Arc::new(index))
    }

    fn package_image(
        identity: &PackageIdentity,
        index: Arc<PackageIndex>,
        worker: WorkerBinding,
    ) -> Result<Arc<PackageImage>> {
        if identity.name != worker.package_name
            || worker.package_version != identity.version.to_string()
            || worker.image_fingerprint != identity.image_fingerprint
            || !index.binding_names.contains(&worker.binding.name)
        {
            return Err(Error::Analysis(format!(
                "worker returned unindexed binding {}::{}",
                identity.name, worker.binding.name
            )));
        }
        Ok(Arc::new(PackageImage {
            index,
            bindings: HashMap::from([(worker.binding.name.clone(), worker.binding)]),
            private_environments: worker.private_environments,
        }))
    }

    pub fn target(&self) -> &TargetEnvironment {
        self.locator.target()
    }

    fn cache_key(&self, identity: &PackageIdentity) -> String {
        fingerprint_strings([
            self.target_fingerprint.as_str(),
            AIR_VERSION,
            ANALYSIS_SCHEMA,
            identity.name.as_str(),
            identity.version.as_ref(),
            identity.image_fingerprint.as_str(),
        ])
        .to_string()
    }

    fn index_cache_name(&self, identity: &PackageIdentity) -> String {
        format!(
            "{}-{}.index.slinker",
            identity.name,
            self.cache_key(identity)
        )
    }

    fn binding_cache_name(&self, identity: &PackageIdentity, binding: &str) -> String {
        let binding = fingerprint_strings([binding]).to_string();
        format!(
            "{}-{}-{binding}.binding.slinker",
            identity.name,
            self.cache_key(identity)
        )
    }

    fn worker(&mut self) -> Result<&mut WorkerClient> {
        match &mut self.worker {
            Some(worker) => Ok(worker),
            empty => {
                let target = self.locator.target();
                Ok(empty.insert(WorkerClient::spawn(self.r_home.clone(), target, 1)?))
            }
        }
    }

    fn load_cached_index(&self, package: &InstalledPackage) -> Option<Arc<PackageIndex>> {
        let cached = self
            .cache
            .read::<CachedIndex>(&self.index_cache_name(&package.identity))
            .filter(|entry| {
                entry.schema == ANALYSIS_SCHEMA
                    && entry.target == self.target_fingerprint
                    && entry.package_fingerprint == package.identity.image_fingerprint
            })?;
        self.package_index(cached.index, package).ok()
    }

    fn publish_binding(&self, package: &InstalledPackage, name: &str, binding: &WorkerBinding) {
        if is_epoch_independent(binding) {
            let cached = CachedBinding {
                schema: ANALYSIS_SCHEMA.into(),
                target: self.target_fingerprint.clone(),
                package_fingerprint: package.identity.image_fingerprint.clone(),
                binding_name: name.into(),
                binding: binding.clone(),
            };
            self.cache
                .publish(&self.binding_cache_name(&package.identity, name), &cached);
        }
    }

    fn load_cached_binding(
        &mut self,
        package: &InstalledPackage,
        binding: &str,
    ) -> Result<Option<Arc<PackageImage>>> {
        let Some(cached) = self
            .cache
            .read::<CachedBinding>(&self.binding_cache_name(&package.identity, binding))
            .filter(|entry| {
                entry.schema == ANALYSIS_SCHEMA
                    && entry.target == self.target_fingerprint
                    && entry.package_fingerprint == package.identity.image_fingerprint
                    && entry.binding_name == binding
                    && is_epoch_independent(&entry.binding)
            })
        else {
            return Ok(None);
        };
        let index = self.index(package)?;
        Self::package_image(&package.identity, index, cached.binding).map(Some)
    }
}

fn is_epoch_independent(binding: &WorkerBinding) -> bool {
    let image = &binding.binding;
    binding.private_environments.is_empty()
        && image
            .object
            .closure
            .iter()
            .map(|closure| closure.environment.as_str())
            .chain(image.object.environment.as_deref())
            .chain(
                image
                    .object
                    .embedded_closures
                    .iter()
                    .map(|closure| closure.environment.as_str()),
            )
            .chain(
                image
                    .object
                    .embedded_environments
                    .iter()
                    .map(|environment| environment.environment.as_str()),
            )
            .all(|label| !label.starts_with("private:"))
}

impl PackageResolver for PackageStore {
    fn target_environment(&self) -> &TargetEnvironment {
        self.locator.target()
    }

    fn locate(&mut self, name: &str) -> Result<Option<InstalledPackage>> {
        self.locator.locate(name)
    }
}

impl PackageProvider for PackageStore {
    fn index(&mut self, package: &InstalledPackage) -> Result<Arc<PackageIndex>> {
        if let Some(index) = self.indexes.get(&package.identity) {
            return Ok(Arc::clone(index));
        }
        let index = match self.load_cached_index(package) {
            Some(index) => index,
            None => {
                let worker = self.worker()?.package_index(package)?;
                let index = self.package_index(worker.clone(), package)?;
                let cached = CachedIndex {
                    schema: ANALYSIS_SCHEMA.into(),
                    target: self.target_fingerprint.clone(),
                    package_fingerprint: package.identity.image_fingerprint.clone(),
                    index: worker,
                };
                self.cache
                    .publish(&self.index_cache_name(&package.identity), &cached);
                index
            }
        };
        self.indexes
            .insert(package.identity.clone(), Arc::clone(&index));
        Ok(index)
    }

    fn binding_image(
        &mut self,
        package: &InstalledPackage,
        name: &str,
    ) -> Result<Arc<PackageImage>> {
        let key = (package.identity.clone(), name.to_owned());
        if let Some(image) = self.bindings.get(&key) {
            return Ok(Arc::clone(image));
        }
        let image = match self.load_cached_binding(package, name)? {
            Some(image) => image,
            None => {
                let index = self.index(package)?;
                let binding = self.worker()?.binding(package, name)?;
                self.publish_binding(package, name, &binding);
                Self::package_image(&package.identity, index, binding)?
            }
        };
        self.bindings.insert(key, Arc::clone(&image));
        Ok(image)
    }

    fn prefetch_binding_images(
        &mut self,
        package: &InstalledPackage,
        names: &[&str],
    ) -> Result<()> {
        let mut pending = Vec::new();
        let mut seen = HashSet::new();
        for name in names {
            let key = (package.identity.clone(), (*name).to_owned());
            if self.bindings.contains_key(&key) || !seen.insert(*name) {
                continue;
            }
            match self.load_cached_binding(package, name)? {
                Some(image) => {
                    self.bindings.insert(key, image);
                }
                None => pending.push(*name),
            }
        }
        for chunk in pending.chunks(BINDING_BATCH) {
            let index = self.index(package)?;
            let inspected = self.worker()?.bindings(package, chunk)?;
            for (name, binding) in chunk.iter().zip(inspected) {
                self.publish_binding(package, name, &binding);
                let image = Self::package_image(&package.identity, Arc::clone(&index), binding)?;
                self.bindings
                    .insert((package.identity.clone(), (*name).to_owned()), image);
            }
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

    fn dispatch_generics(&mut self, subject: DispatchSubject<'_>) -> Result<BTreeSet<GenericName>> {
        let (package, binding) = match subject {
            DispatchSubject::Base { binding } => (None, binding),
            DispatchSubject::Installed { package, binding } => (Some(package), binding),
        };
        let key = (
            package.map(|package| package.identity.clone()),
            binding.to_owned(),
        );
        if let Some(generics) = self.dispatch.get(&key) {
            return Ok(generics.clone());
        }
        let generics = self
            .worker()?
            .dispatch_generics(package, binding)?
            .into_iter()
            .map(GenericName::from)
            .collect::<BTreeSet<_>>();
        self.dispatch.insert(key, generics.clone());
        Ok(generics)
    }

    fn validate_syntax(&mut self, source: &str) -> Result<SyntaxValidation> {
        self.worker()?.validate_syntax(source)
    }

    fn canonical_syntax(&mut self, source: &str) -> Result<CanonicalSyntax> {
        if let Some(known) = self.syntax.get(source) {
            return Ok(known.clone());
        }
        let canonical = self.worker()?.canonical_syntax(source)?;
        self.syntax.insert(source.to_owned(), canonical.clone());
        Ok(canonical)
    }

    fn prefetch_canonical_syntax(&mut self, sources: &[&str]) -> Result<()> {
        let mut seen = HashSet::new();
        let mut unknown = sources
            .iter()
            .copied()
            .filter(|source| !self.syntax.contains_key(*source) && seen.insert(*source))
            .collect::<Vec<_>>();
        if unknown.is_empty() {
            return Ok(());
        }
        unknown.sort_by_key(|source| std::cmp::Reverse(source.len()));
        let lanes = self
            .worker_limit
            .min(unknown.len().div_ceil(SOURCES_PER_WORKER))
            .max(1);
        let mut chunks = vec![Vec::new(); lanes];
        for (position, source) in unknown.into_iter().enumerate() {
            chunks[position % lanes].push(source);
        }
        let mut clients = Vec::with_capacity(lanes);
        clients.push(self.worker.take());
        clients.extend(self.extra_workers.drain(..).map(Some));
        clients.resize_with(clients.len().max(lanes), || None);
        chunks.resize(clients.len(), Vec::new());
        let r_home = &self.r_home;
        let target = self.locator.target();
        let outcomes = std::thread::scope(|scope| {
            clients
                .into_iter()
                .zip(chunks)
                .enumerate()
                .map(|(position, (client, chunk))| {
                    scope.spawn(move || {
                        let client = match client {
                            Some(client) => Ok(client),
                            None if chunk.is_empty() => return (None, Ok(Vec::new())),
                            None => {
                                WorkerClient::spawn(r_home.clone(), target, position as u64 + 1)
                            }
                        };
                        match client {
                            Ok(mut client) => {
                                let canonical = client.canonical_syntax_batch(&chunk);
                                (
                                    Some(client),
                                    canonical
                                        .map(|results| (chunk, results))
                                        .map(|pair| vec![pair]),
                                )
                            }
                            Err(error) => (None, Err(error)),
                        }
                    })
                })
                .collect::<Vec<_>>()
                .into_iter()
                .map(|handle| {
                    handle
                        .join()
                        .map_err(|_| Error::Analysis("R worker thread panicked".into()))
                })
                .collect::<Result<Vec<_>>>()
        })?;
        let mut first = None;
        let mut failure = None;
        for (position, (client, result)) in outcomes.into_iter().enumerate() {
            match (position, client) {
                (0, client) => first = client,
                (_, Some(client)) => self.extra_workers.push(client),
                (_, None) => {}
            }
            match result {
                Ok(batches) => {
                    for (chunk, results) in batches {
                        for (source, canonical) in chunk.into_iter().zip(results) {
                            self.syntax.insert(source.to_owned(), canonical);
                        }
                    }
                }
                Err(error) => failure = failure.or(Some(error)),
            }
        }
        self.worker = first;
        failure.map_or(Ok(()), Err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Description;
    use crate::package::{
        BindingImage, BindingNames, BindingOrigin, BindingRepresentation, ClosureSource, Digest,
        LifecycleMetadata, NativeComponent, NativeLibrary, ObjectImage, ObjectKind,
    };

    fn package_index() -> PackageIndex {
        PackageIndex {
            identity: PackageIdentity {
                name: "fixture".into(),
                version: "1.0.0".parse().expect("version"),
                image_fingerprint: Digest::from("exact-image"),
            },
            description: Description::parse("Package: fixture\nVersion: 1.0.0\n"),
            exports: Default::default(),
            imports: Vec::new(),
            s3: Vec::new(),
            dynlibs: vec![NativeComponent {
                name: "fixture".into(),
                alias: String::new(),
                registration: None,
                symbols: Vec::new(),
                library: NativeLibrary::Missing,
                safety: NativeSafety::Unanalyzed,
            }],
            lifecycle: LifecycleMetadata::default(),
            binding_names: BindingNames::default(),
            data: PackageData::default(),
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
    fn only_epoch_independent_binding_fragments_are_cacheable() {
        let fragment = |environment: &str| WorkerBinding {
            package_name: "fixture".into(),
            package_version: "1.0.0".into(),
            image_fingerprint: "exact-image".into(),
            binding: BindingImage {
                name: "f".into(),
                origin: BindingOrigin::Code,
                object: ObjectImage {
                    representation: BindingRepresentation::Value,
                    classes: Vec::new(),
                    object_kind: ObjectKind::Closure,
                    closure: Some(ClosureSource {
                        source: "function() 1".into(),
                        environment: environment.into(),
                    }),
                    environment: None,
                    embedded_closures: Vec::new(),
                    embedded_environments: Vec::new(),
                    issues: Vec::new(),
                },
            },
            private_environments: HashMap::new(),
        };

        assert!(is_epoch_independent(&fragment("namespace:fixture")));
        assert!(!is_epoch_independent(&fragment(
            "private:00000000000000ab:1"
        )));
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
