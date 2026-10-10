use crate::cache::{Cache, CacheLocation};
use crate::package::cache_names::EntryKind;
use crate::package::inspection::{Batcher, Lanes, Slot};
use crate::package::locator::fingerprint_strings;
use crate::package::{
    BindingName, ComponentName, Digest, EnvironmentKind, EnvironmentLabel, FrozenPackages,
    GenericName, InstalledPackage, LifecycleMetadata, NativeFacts, NativeRoutineSummary,
    NativeSafety, ObjectImage, PackageData, PackageIdentity, PackageImage, PackageIndex,
    PackageName, PrivateEnvironmentImage, ResourcePath,
};
use crate::worker::client::WorkerClient;
use crate::worker::protocol::{
    NormalizeOutcome, NormalizedSource, WorkerBinding, WorkerNormalization, WorkerPackageIndex,
};
use crate::worker::service::WorkerService;
use crate::{Error, Result, TargetEnvironment};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

const AIR_VERSION: &str = "0.11.0";
fn identifiers(source: &str) -> impl Iterator<Item = &str> {
    source
        .split(|character: char| !(character.is_alphanumeric() || matches!(character, '.' | '_')))
        .filter(|word| !word.is_empty())
}

pub(super) const ANALYSIS_SCHEMA: &str = "slinker-analysis-v1";

#[must_use]
pub fn analysis_schema() -> &'static str {
    ANALYSIS_SCHEMA
}
const SOURCES_PER_WORKER: usize = 48;

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct CachedPackage<T> {
    target: Digest,
    package_fingerprint: Digest,
    value: T,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct CachedDispatch {
    target: Digest,
    generics: BTreeSet<String>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct CachedNormalization {
    target: Digest,
    source: String,
    canonical: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub(crate) struct NativeSummaryManifest {
    schema: u32,
    #[serde(default)]
    packages: Vec<NativePackageSummary>,
    #[serde(skip)]
    root: Option<RootNativeAudit>,
}

#[derive(Clone, Debug)]
struct RootNativeAudit {
    identity: PackageIdentity,
    source: Digest,
    target: crate::Target,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Hash)]
#[serde(tag = "origin", rename_all = "snake_case")]
enum NativeAuditScope {
    Installed {
        image_fingerprint: Digest,
    },
    RootSource {
        source_fingerprint: Digest,
        target: crate::Target,
    },
}

#[derive(Clone, Debug, Deserialize)]
struct NativePackageSummary {
    package: PackageName,
    version: String,
    #[serde(flatten)]
    scope: NativeAuditScope,
    components: Vec<NativeComponentSummary>,
}

impl NativePackageSummary {
    fn applies_to(&self, identity: &PackageIdentity, root: Option<&RootNativeAudit>) -> bool {
        self.package == identity.name
            && self.version == identity.version.as_ref()
            && match &self.scope {
                NativeAuditScope::Installed { image_fingerprint } => {
                    image_fingerprint == &identity.image_fingerprint
                }
                NativeAuditScope::RootSource {
                    source_fingerprint,
                    target,
                } => root.is_some_and(|root| {
                    &root.identity == identity
                        && &root.source == source_fingerprint
                        && &root.target == target
                }),
            }
    }
}

#[derive(Clone, Debug, Deserialize)]
struct NativeComponentSummary {
    component: ComponentName,
    #[serde(flatten)]
    safety: NativeSummarySafety,
}

#[derive(Clone, Debug, Deserialize)]
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
    pub(crate) fn load(path: Option<&std::path::Path>) -> Result<Self> {
        let Some(path) = path else {
            return Ok(Self::default());
        };
        let text = fs::read_to_string(path).map_err(|source| Error::Io {
            path: path.to_path_buf(),
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
                &package.scope,
            );
            if !packages.insert(key) {
                return Err(Error::Analysis(format!(
                    "duplicate native summary package identity {} {} {:?}",
                    package.package, package.version, package.scope
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
        let Some(package) = self
            .packages
            .iter()
            .find(|summary| summary.applies_to(&index.identity, self.root.as_ref()))
        else {
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

    pub(crate) fn bind_root(
        &self,
        identity: PackageIdentity,
        source: Digest,
        target: crate::Target,
    ) -> Result<Self> {
        let root = RootNativeAudit {
            identity,
            source,
            target,
        };
        let matches = self
            .packages
            .iter()
            .filter(|summary| summary.applies_to(&root.identity, Some(&root)))
            .count();
        if matches > 1 {
            return Err(Error::Analysis(
                "multiple native audits apply to the staged Root".into(),
            ));
        }
        let mut bound = self.clone();
        bound.root = Some(root);
        Ok(bound)
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SyntaxRejection(String);

impl std::fmt::Display for SyntaxRejection {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

pub type Normalization = std::result::Result<CanonicalSyntax, SyntaxRejection>;

impl From<NormalizeOutcome> for Normalization {
    fn from(outcome: NormalizeOutcome) -> Self {
        match outcome {
            NormalizeOutcome::Normalized(normalized) => Ok(normalized.into()),
            NormalizeOutcome::Rejected(message) => Err(SyntaxRejection(message)),
        }
    }
}

impl From<NormalizedSource> for CanonicalSyntax {
    fn from(normalized: NormalizedSource) -> Self {
        if normalized.stable {
            Self::Stable(normalized.source)
        } else {
            Self::Unstable
        }
    }
}

impl CanonicalSyntax {
    fn stable_form(&self) -> Option<&str> {
        match self {
            Self::Stable(canonical) => Some(canonical),
            Self::Unstable => None,
        }
    }
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

pub trait PackageResolver: Send + Sync {
    fn target_environment(&self) -> &TargetEnvironment;
    fn locate(&self, name: &str) -> Result<Option<InstalledPackage>>;
}

pub trait PackageProvider: PackageResolver {
    fn native_source_fingerprint(&self, _package: &InstalledPackage) -> Option<Digest> {
        None
    }
    fn index(&self, package: &InstalledPackage) -> Result<Arc<PackageIndex>>;
    fn binding_image(&self, package: &InstalledPackage, name: &str) -> Result<Arc<PackageImage>>;
    fn resource_exists(&self, package: &InstalledPackage, path: &ResourcePath) -> Result<bool> {
        Ok(self
            .index(package)?
            .files
            .iter()
            .any(|candidate| candidate == path.as_str()))
    }
    fn dispatch_generics(&self, subject: DispatchSubject<'_>) -> Result<BTreeSet<GenericName>>;
    fn validate_syntax(&self, source: &str) -> Result<SyntaxValidation>;
    fn canonical_syntax(&self, source: &str) -> Result<CanonicalSyntax>;
    fn prefetch_canonical_syntax(&self, _sources: &[&str]) -> Result<()> {
        Ok(())
    }
    fn prefetch_binding_images(&self, _package: &InstalledPackage, _names: &[&str]) -> Result<()> {
        Ok(())
    }
}

struct Memo<K, V> {
    cells: Mutex<HashMap<K, Arc<Mutex<Option<V>>>>>,
}

impl<K, V> Default for Memo<K, V> {
    fn default() -> Self {
        Self {
            cells: Mutex::new(HashMap::new()),
        }
    }
}

impl<K: Eq + Hash + Clone, V: Clone> Memo<K, V> {
    fn get_or_compute(&self, key: &K, compute: impl FnOnce() -> Result<V>) -> Result<V> {
        let cell = Arc::clone(
            self.cells
                .lock()
                .expect("memo cells")
                .entry(key.clone())
                .or_default(),
        );
        let mut slot = cell.lock().expect("memo cell");
        if let Some(value) = slot.as_ref() {
            return Ok(value.clone());
        }
        let value = compute()?;
        *slot = Some(value.clone());
        Ok(value)
    }
}

type BindingBatcher = Batcher<String, Arc<PackageImage>>;

pub struct PackageStore {
    locator: Arc<FrozenPackages>,
    indexes: Memo<PackageIdentity, Arc<PackageIndex>>,
    bindings: Mutex<HashMap<PackageIdentity, Arc<BindingBatcher>>>,
    delivered:
        Mutex<HashMap<PackageIdentity, HashMap<EnvironmentLabel, Arc<PrivateEnvironmentImage>>>>,
    published_environments: Mutex<HashSet<(PackageIdentity, EnvironmentLabel)>>,
    demanded: Mutex<HashSet<(PackageIdentity, String)>>,
    dispatch: Memo<(Option<PackageIdentity>, String), BTreeSet<GenericName>>,
    normalizer: Batcher<String, Normalization>,
    cache: Cache,
    target_fingerprint: Digest,
    lanes: Arc<Lanes>,
    native_summaries: Arc<NativeSummaryManifest>,
}

impl PackageStore {
    pub fn new(target: TargetEnvironment, cache: CacheLocation) -> Result<Self> {
        let manifest = std::env::var_os("SLINKER_NATIVE_SUMMARIES").map(PathBuf::from);
        Self::for_session(
            &WorkerService::new(target, crate::WorkerExecutable::default(), 1),
            cache,
            Arc::new(NativeSummaryManifest::load(manifest.as_deref())?),
        )
    }

    pub(crate) fn for_session(
        workers: &WorkerService,
        cache: CacheLocation,
        native_summaries: Arc<NativeSummaryManifest>,
    ) -> Result<Self> {
        let target = workers.target().clone();
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
            lanes: workers.inspection(),
            locator: workers.packages(),
            indexes: Memo::default(),
            bindings: Mutex::new(HashMap::new()),
            delivered: Mutex::new(HashMap::new()),
            published_environments: Mutex::new(HashSet::new()),
            demanded: Mutex::new(HashSet::new()),
            dispatch: Memo::default(),
            normalizer: Batcher::default(),
            cache: Cache::new(cache, ANALYSIS_SCHEMA)?,
            target_fingerprint,
            native_summaries,
        })
    }

    fn affine_lane(&self, identity: &PackageIdentity) -> usize {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        identity.name.hash(&mut hasher);
        usize::try_from(hasher.finish() % self.lanes.count() as u64).unwrap_or(0)
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
        private_environments: HashMap<EnvironmentLabel, Arc<PrivateEnvironmentImage>>,
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
            bindings: HashMap::from([(worker.binding.name.clone(), Arc::new(worker.binding))]),
            private_environments,
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
        EntryKind::index_name(identity.name.as_str(), &self.cache_key(identity))
    }

    fn binding_cache_name(&self, identity: &PackageIdentity, binding: &str) -> String {
        let binding = fingerprint_strings([binding]).to_string();
        EntryKind::Binding.member_name(identity.name.as_str(), &self.cache_key(identity), &binding)
    }

    fn load_cached_index(&self, package: &InstalledPackage) -> Option<Arc<PackageIndex>> {
        let worker =
            self.load_cached_package(&self.index_cache_name(&package.identity), &package.identity)?;
        self.package_index(worker, package).ok()
    }

    fn load_cached_package<T: DeserializeOwned>(
        &self,
        name: &str,
        identity: &PackageIdentity,
    ) -> Option<T> {
        self.cache
            .read::<CachedPackage<T>>(name)
            .filter(|entry| {
                entry.target == self.target_fingerprint
                    && entry.package_fingerprint == identity.image_fingerprint
            })
            .map(|entry| entry.value)
    }

    fn publish_cached_package<T: Serialize>(
        &self,
        name: &str,
        identity: &PackageIdentity,
        value: T,
    ) {
        self.cache.publish(
            name,
            &CachedPackage {
                target: self.target_fingerprint.clone(),
                package_fingerprint: identity.image_fingerprint.clone(),
                value,
            },
        );
    }

    fn environment_cache_name(
        &self,
        identity: &PackageIdentity,
        label: &EnvironmentLabel,
    ) -> String {
        let label = fingerprint_strings([label.as_str()]).to_string();
        EntryKind::Environment.member_name(
            identity.name.as_str(),
            &self.cache_key(identity),
            &label,
        )
    }

    fn publish_binding(
        &self,
        package: &InstalledPackage,
        name: &str,
        binding: &WorkerBinding,
        environments: &HashMap<EnvironmentLabel, Arc<PrivateEnvironmentImage>>,
    ) {
        for (label, environment) in environments {
            let fresh = self
                .published_environments
                .lock()
                .expect("published environments")
                .insert((package.identity.clone(), label.clone()));
            if fresh {
                self.publish_cached_package(
                    &self.environment_cache_name(&package.identity, label),
                    &package.identity,
                    environment.as_ref(),
                );
            }
        }
        let mut fragment = binding.clone();
        fragment.private_environments.clear();
        self.publish_cached_package(
            &self.binding_cache_name(&package.identity, name),
            &package.identity,
            fragment,
        );
    }

    fn load_cached_environments(
        &self,
        package: &InstalledPackage,
        roots: Vec<EnvironmentLabel>,
    ) -> Option<HashMap<EnvironmentLabel, Arc<PrivateEnvironmentImage>>> {
        let mut environments = HashMap::new();
        let mut pending = roots;
        while let Some(label) = pending.pop() {
            if label.kind() != EnvironmentKind::Private || environments.contains_key(&label) {
                continue;
            }
            let environment: PrivateEnvironmentImage = self.load_cached_package(
                &self.environment_cache_name(&package.identity, &label),
                &package.identity,
            )?;
            if environment.id != label {
                return None;
            }
            pending.push(environment.parent.clone());
            pending.extend(
                environment
                    .bindings
                    .values()
                    .flat_map(|private| private.object.environment_labels().cloned()),
            );
            environments.insert(label, Arc::new(environment));
        }
        Some(environments)
    }

    fn load_cached_binding(
        &self,
        package: &InstalledPackage,
        index: &Arc<PackageIndex>,
        binding: &str,
    ) -> Option<Arc<PackageImage>> {
        let cached: WorkerBinding = self.load_cached_package(
            &self.binding_cache_name(&package.identity, binding),
            &package.identity,
        )?;
        if cached.binding.name != binding {
            return None;
        }
        let environments = self.load_cached_environments(
            package,
            cached
                .binding
                .object
                .environment_labels()
                .cloned()
                .collect(),
        )?;
        Self::package_image(&package.identity, Arc::clone(index), cached, environments).ok()
    }

    fn dispatch_cache_name(&self, identity: Option<&PackageIdentity>, binding: &str) -> String {
        let owner = identity.map_or_else(
            || self.target_fingerprint.to_string(),
            |identity| self.cache_key(identity),
        );
        EntryKind::dispatch_name(&fingerprint_strings([owner.as_str(), binding]).to_string())
    }

    fn normalization_cache_name(&self, source: &str) -> String {
        let key = fingerprint_strings([self.target_fingerprint.as_str(), ANALYSIS_SCHEMA, source]);
        EntryKind::normalization_name(&key.to_string())
    }

    fn load_cached_normalization(&self, source: &str) -> Option<Normalization> {
        let cached = self
            .cache
            .read::<CachedNormalization>(&self.normalization_cache_name(source))
            .filter(|entry| entry.target == self.target_fingerprint && entry.source == source)?;
        Some(Ok(cached
            .canonical
            .map_or(CanonicalSyntax::Unstable, CanonicalSyntax::Stable)))
    }

    fn execute_normalizations(
        &self,
        client: &mut WorkerClient,
        sources: &[String],
    ) -> Result<Vec<Normalization>> {
        let refs = sources.iter().map(String::as_str).collect::<Vec<_>>();
        let results = client.canonical_syntax_batch(&refs)?;
        for (source, result) in sources.iter().zip(&results) {
            if let Ok(canonical) = result {
                self.persist_normalization(source, canonical);
            }
        }
        Ok(results)
    }

    fn persist_normalization(&self, source: &str, result: &CanonicalSyntax) {
        self.cache.publish(
            &self.normalization_cache_name(source),
            &CachedNormalization {
                target: self.target_fingerprint.clone(),
                source: source.to_owned(),
                canonical: result.stable_form().map(str::to_owned),
            },
        );
    }

    fn adopt_worker_normalizations(&self, normalizations: Vec<WorkerNormalization>) {
        for WorkerNormalization {
            original,
            canonical,
        } in normalizations
        {
            let canonical = CanonicalSyntax::from(canonical);
            self.persist_normalization(&original, &canonical);
            self.normalizer.seed(&original, Ok(canonical));
        }
    }

    fn seed_cached_normalization(&self, source: &str) -> bool {
        let Some(cached) = self.load_cached_normalization(source) else {
            return false;
        };
        self.normalizer.seed(&source.to_owned(), cached);
        true
    }

    fn load_binding(&self, package: &InstalledPackage, name: &str) -> Result<Arc<PackageImage>> {
        let batcher = self.binding_batcher(&package.identity);
        let query = name.to_owned();
        if let Some(known) = batcher.known(&query) {
            return known;
        }
        let index = self.index(package)?;
        let Some(slot) = self.submit_binding(&batcher, package, &index, name) else {
            return batcher.known(&query).expect("a seeded binding is known");
        };
        batcher.drive(&self.lanes, &slot, &|client, names| {
            self.execute_bindings(package, &index, client, names)
        })
    }

    fn look_ahead(
        &self,
        package: &InstalledPackage,
        name: &str,
        image: &PackageImage,
    ) -> Result<()> {
        let first_demand = self
            .demanded
            .lock()
            .expect("demanded bindings")
            .insert((package.identity.clone(), name.to_owned()));
        if !first_demand {
            return Ok(());
        }
        let index = self.index(package)?;
        let batcher = self.binding_batcher(&package.identity);
        let referenced = image
            .bindings
            .values()
            .map(|binding| &binding.object)
            .chain(
                image
                    .private_environments
                    .values()
                    .flat_map(|environment| environment.bindings.values())
                    .map(|private| &private.object),
            )
            .flat_map(ObjectImage::closure_sources)
            .flat_map(identifiers)
            .filter(|word| index.binding_names.contains(word))
            .collect::<BTreeSet<_>>();
        for word in referenced {
            self.submit_binding(&batcher, package, &index, word);
        }
        Ok(())
    }

    fn binding_batcher(&self, identity: &PackageIdentity) -> Arc<BindingBatcher> {
        Arc::clone(
            self.bindings
                .lock()
                .expect("binding batchers")
                .entry(identity.clone())
                .or_default(),
        )
    }

    fn arced(
        environments: HashMap<EnvironmentLabel, PrivateEnvironmentImage>,
    ) -> HashMap<EnvironmentLabel, Arc<PrivateEnvironmentImage>> {
        environments
            .into_iter()
            .map(|(label, environment)| (label, Arc::new(environment)))
            .collect()
    }

    fn deliver(
        &self,
        identity: &PackageIdentity,
        binding: &mut WorkerBinding,
    ) -> HashMap<EnvironmentLabel, Arc<PrivateEnvironmentImage>> {
        let mut delivered = self.delivered.lock().expect("delivered environments");
        let known = delivered.entry(identity.clone()).or_default();
        for (label, environment) in Self::arced(std::mem::take(&mut binding.private_environments)) {
            known.entry(label).or_insert(environment);
        }
        let mut reachable = HashMap::new();
        let mut pending = binding
            .binding
            .object
            .environment_labels()
            .cloned()
            .collect::<Vec<_>>();
        while let Some(label) = pending.pop() {
            if label.kind() != EnvironmentKind::Private || reachable.contains_key(&label) {
                continue;
            }
            let Some(environment) = known.get(&label) else {
                continue;
            };
            pending.push(environment.parent.clone());
            pending.extend(
                environment
                    .bindings
                    .values()
                    .flat_map(|private| private.object.environment_labels().cloned()),
            );
            reachable.insert(label, Arc::clone(environment));
        }
        reachable
    }

    fn execute_bindings(
        &self,
        package: &InstalledPackage,
        index: &Arc<PackageIndex>,
        client: &mut WorkerClient,
        names: &[String],
    ) -> Result<Vec<Arc<PackageImage>>> {
        let refs = names.iter().map(String::as_str).collect::<Vec<_>>();
        client
            .bindings(package, &refs)?
            .into_iter()
            .zip(names)
            .map(|(mut binding, name)| {
                let privates = self.deliver(&package.identity, &mut binding);
                self.adopt_worker_normalizations(std::mem::take(&mut binding.normalizations));
                self.publish_binding(package, name, &binding, &privates);
                Self::package_image(&package.identity, Arc::clone(index), binding, privates)
            })
            .collect()
    }

    fn submit_binding(
        &self,
        batcher: &BindingBatcher,
        package: &InstalledPackage,
        index: &Arc<PackageIndex>,
        name: &str,
    ) -> Option<Arc<Slot<Arc<PackageImage>>>> {
        let query = name.to_owned();
        if batcher.known(&query).is_some() {
            return None;
        }
        if let Some(image) = self.load_cached_binding(package, index, name) {
            batcher.seed(&query, image);
            return None;
        }
        Some(batcher.submit(&query))
    }
}

impl PackageResolver for PackageStore {
    fn target_environment(&self) -> &TargetEnvironment {
        self.locator.target()
    }

    fn locate(&self, name: &str) -> Result<Option<InstalledPackage>> {
        self.locator.locate(name)
    }
}

impl PackageProvider for PackageStore {
    fn native_source_fingerprint(&self, package: &InstalledPackage) -> Option<Digest> {
        self.native_summaries
            .root
            .as_ref()
            .filter(|root| root.identity == package.identity)
            .map(|root| root.source.clone())
    }
    fn index(&self, package: &InstalledPackage) -> Result<Arc<PackageIndex>> {
        self.indexes.get_or_compute(&package.identity, || {
            match self.load_cached_index(package) {
                Some(index) => Ok(index),
                None => {
                    let lane = self.affine_lane(&package.identity);
                    let worker = self.lanes.lane(lane).client()?.package_index(package)?;
                    let index = self.package_index(worker.clone(), package)?;
                    self.publish_cached_package(
                        &self.index_cache_name(&package.identity),
                        &package.identity,
                        worker,
                    );
                    Ok(index)
                }
            }
        })
    }

    fn binding_image(&self, package: &InstalledPackage, name: &str) -> Result<Arc<PackageImage>> {
        let image = self.load_binding(package, name)?;
        self.look_ahead(package, name, &image)?;
        Ok(image)
    }

    fn prefetch_binding_images(&self, package: &InstalledPackage, names: &[&str]) -> Result<()> {
        let batcher = self.binding_batcher(&package.identity);
        let index = self.index(package)?;
        let mut submitted = false;
        for name in names {
            submitted |= self
                .submit_binding(&batcher, package, &index, name)
                .is_some();
        }
        if submitted {
            batcher.lead(&self.lanes, &|client, names| {
                self.execute_bindings(package, &index, client, names)
            });
        }
        Ok(())
    }

    fn resource_exists(&self, package: &InstalledPackage, path: &ResourcePath) -> Result<bool> {
        if path.is_empty() {
            return Ok(package.location.root.is_dir());
        }
        Ok(package.location.root.join(path.as_str()).exists())
    }

    fn dispatch_generics(&self, subject: DispatchSubject<'_>) -> Result<BTreeSet<GenericName>> {
        let (package, binding) = match subject {
            DispatchSubject::Base { binding } => (None, binding),
            DispatchSubject::Installed { package, binding } => (Some(package), binding),
        };
        let key = (
            package.map(|package| package.identity.clone()),
            binding.to_owned(),
        );
        self.dispatch.get_or_compute(&key, || {
            let name = self.dispatch_cache_name(package.map(|package| &package.identity), binding);
            if let Some(cached) = self
                .cache
                .read::<CachedDispatch>(&name)
                .filter(|entry| entry.target == self.target_fingerprint)
            {
                return Ok(cached.generics.into_iter().map(GenericName::from).collect());
            }
            let lane = package.map_or(0, |package| self.affine_lane(&package.identity));
            let generics = self
                .lanes
                .lane(lane)
                .client()?
                .dispatch_generics(package, binding)?
                .into_iter()
                .collect::<BTreeSet<String>>();
            self.cache.publish(
                &name,
                &CachedDispatch {
                    target: self.target_fingerprint.clone(),
                    generics: generics.clone(),
                },
            );
            Ok(generics.into_iter().map(GenericName::from).collect())
        })
    }

    fn validate_syntax(&self, source: &str) -> Result<SyntaxValidation> {
        self.lanes.lane(0).client()?.validate_syntax(source)
    }

    fn canonical_syntax(&self, source: &str) -> Result<CanonicalSyntax> {
        let query = source.to_owned();
        let known = match self.normalizer.known(&query) {
            Some(known) => known,
            None if self.seed_cached_normalization(source) => self
                .normalizer
                .known(&query)
                .expect("a seeded normalization is known"),
            None => {
                let slot = self.normalizer.submit(&query);
                self.normalizer
                    .drive(&self.lanes, &slot, &|client, sources| {
                        self.execute_normalizations(client, sources)
                    })
            }
        };
        known?.map_err(|rejection| {
            Error::Analysis(format!(
                "Harp worker syntax normalization failed (TargetSyntaxRejection) for target: {rejection}"
            ))
        })
    }

    fn prefetch_canonical_syntax(&self, sources: &[&str]) -> Result<()> {
        let mut submitted = 0usize;
        for source in sources {
            let query = (*source).to_owned();
            if self.normalizer.known(&query).is_none() && !self.seed_cached_normalization(source) {
                self.normalizer.submit(&query);
                submitted += 1;
            }
        }
        let leaders = self
            .lanes
            .count()
            .min(submitted.div_ceil(SOURCES_PER_WORKER))
            .max(1);
        if submitted > 0 {
            std::thread::scope(|scope| {
                for _ in 0..leaders {
                    scope.spawn(|| {
                        self.normalizer.lead(&self.lanes, &|client, sources| {
                            self.execute_normalizations(client, sources)
                        });
                    });
                }
            });
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "../../tests/unit/package/store.rs"]
mod tests;
