use crate::build::incremental::BuildState;
use crate::cache::{
    Cache, CacheLocation, SchemaDirectory, cache_root, clear_schemas, schema_directories,
};
use crate::package::cache_names::{EntryKind, EntryName};
use crate::package::store::ANALYSIS_SCHEMA;
use crate::{Error, Result};
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Debug, Default, Serialize)]
pub struct KindTotals {
    pub entries: usize,
    pub bytes: usize,
}

#[derive(Debug, Serialize)]
pub struct PackageCacheReport {
    pub name: String,
    pub version: Option<String>,
    pub image_fingerprint: Option<String>,
    pub cache_key: String,
    pub bindings: usize,
    pub environments: usize,
    pub bytes: usize,
}

#[derive(Debug, Serialize)]
pub struct SchemaCacheReport {
    pub schema: String,
    pub current: bool,
    pub directory: PathBuf,
    pub packs: usize,
    pub pack_bytes: u64,
    pub entries: usize,
    pub by_kind: BTreeMap<EntryKind, KindTotals>,
    pub unrecognized_entries: usize,
    pub packages: Vec<PackageCacheReport>,
}

#[derive(Debug, Serialize)]
pub struct BuildCacheReport {
    pub package: String,
    pub output: PathBuf,
    pub inputs: String,
    pub output_digest: String,
    pub consulted_packages: usize,
}

#[derive(Debug, Serialize)]
pub struct CacheReport {
    pub location: Option<PathBuf>,
    pub current_schema: &'static str,
    pub schemas: Vec<SchemaCacheReport>,
    pub builds: Vec<BuildCacheReport>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ClearScope {
    All,
    Obsolete,
    Packages(Vec<String>),
}

#[derive(Debug, Default, Serialize)]
pub struct ClearOutcome {
    pub entries_removed: usize,
    pub bytes_removed: u64,
}

fn schema_report(location: &CacheLocation, schema: SchemaDirectory) -> Result<SchemaCacheReport> {
    let root =
        cache_root(location).ok_or_else(|| Error::Analysis("the cache is disabled".into()))?;
    let cache = Cache::new(CacheLocation::Directory(root), &schema.schema)?;
    let mut by_kind = BTreeMap::<EntryKind, KindTotals>::new();
    let mut packages = BTreeMap::<(String, String), PackageCacheReport>::new();
    let mut unrecognized = 0;
    let entries = cache.entries();
    for (name, bytes) in &entries {
        let Some(parsed) = EntryName::parse(name) else {
            unrecognized += 1;
            continue;
        };
        let totals = by_kind.entry(parsed.kind).or_default();
        totals.entries += 1;
        totals.bytes += bytes;
        let Some(package) = parsed.package else {
            continue;
        };
        let report = packages
            .entry((package.clone(), parsed.key.clone()))
            .or_insert_with(|| PackageCacheReport {
                name: package,
                version: None,
                image_fingerprint: None,
                cache_key: parsed.key.clone(),
                bindings: 0,
                environments: 0,
                bytes: 0,
            });
        report.bytes += bytes;
        match parsed.kind {
            EntryKind::Binding => report.bindings += 1,
            EntryKind::Environment => report.environments += 1,
            EntryKind::Index => {
                if let Some(entry) = cache.read::<serde_json::Value>(name) {
                    report.version = entry["index"]["version"].as_str().map(str::to_owned);
                    report.image_fingerprint = entry["index"]["image_fingerprint"]
                        .as_str()
                        .map(str::to_owned);
                }
            }
            EntryKind::Normalization | EntryKind::Dispatch => {}
        }
    }
    Ok(SchemaCacheReport {
        current: schema.schema == ANALYSIS_SCHEMA,
        schema: schema.schema,
        directory: schema.directory,
        packs: schema.packs,
        pack_bytes: schema.bytes,
        entries: entries.len(),
        by_kind,
        unrecognized_entries: unrecognized,
        packages: packages.into_values().collect(),
    })
}

pub fn inspect_cache(location: &CacheLocation) -> Result<CacheReport> {
    let schemas = schema_directories(location)
        .into_iter()
        .map(|schema| schema_report(location, schema))
        .collect::<Result<Vec<_>>>()?;
    let builds = BuildState::new(location)
        .records()
        .into_iter()
        .map(|record| BuildCacheReport {
            consulted_packages: record.consulted.len(),
            package: record.package,
            output: record.output,
            inputs: record.inputs,
            output_digest: record.output_digest,
        })
        .collect();
    Ok(CacheReport {
        location: cache_root(location),
        current_schema: ANALYSIS_SCHEMA,
        schemas,
        builds,
    })
}

pub fn clear_cache(location: &CacheLocation, scope: &ClearScope) -> Result<ClearOutcome> {
    let mut outcome = ClearOutcome::default();
    match scope {
        ClearScope::All | ClearScope::Obsolete => {
            let keep_current = *scope == ClearScope::Obsolete;
            for schema in schema_directories(location) {
                if keep_current && schema.schema == ANALYSIS_SCHEMA {
                    continue;
                }
                let entries = Cache::new(
                    CacheLocation::Directory(
                        cache_root(location)
                            .ok_or_else(|| Error::Analysis("the cache is disabled".into()))?,
                    ),
                    &schema.schema,
                )?
                .entries()
                .len();
                outcome.entries_removed += entries;
            }
            outcome.bytes_removed =
                clear_schemas(location, |schema| keep_current && schema == ANALYSIS_SCHEMA)?;
            if !keep_current {
                outcome.entries_removed += BuildState::new(location).clear()?;
            }
        }
        ClearScope::Packages(names) => {
            outcome.entries_removed += BuildState::new(location).remove_packages(names);
            let root = cache_root(location)
                .ok_or_else(|| Error::Analysis("the cache is disabled".into()))?;
            for schema in schema_directories(location) {
                let before = schema.bytes;
                let cache = Cache::new(CacheLocation::Directory(root.clone()), &schema.schema)?;
                outcome.entries_removed += cache.remove_where(|entry| {
                    EntryName::parse(entry)
                        .and_then(|parsed| parsed.package)
                        .is_some_and(|package| names.contains(&package))
                })?;
                drop(cache);
                let after = schema_directories(location)
                    .into_iter()
                    .find(|candidate| candidate.schema == schema.schema)
                    .map_or(0, |candidate| candidate.bytes);
                outcome.bytes_removed += before.saturating_sub(after);
            }
        }
    }
    Ok(outcome)
}
