use clap::{Args, Subcommand};
use serde_json::json;
use slinker_core::cache::{CacheLocation, cache_root};
use slinker_core::package::{
    CacheReport, ClearScope, EntryKind, SchemaCacheReport, clear_cache, inspect_cache,
};
use std::error::Error;

#[derive(Debug, Args)]
pub struct CacheArgs {
    #[command(subcommand)]
    command: Option<CacheCommand>,
    #[arg(long, global = true, help = "Print one JSON document on stdout")]
    json: bool,
}

#[derive(Debug, Subcommand)]
enum CacheCommand {
    #[command(about = "Show where the cache is and how large it is (default)")]
    Info,
    #[command(about = "List every cached package with version, fingerprint, key and size")]
    List {
        #[arg(long, help = "Print full fingerprints and cache keys")]
        full: bool,
    },
    #[command(about = "Print the cache directory")]
    Path,
    #[command(about = "Delete cache entries: everything, obsolete schemas, or chosen packages")]
    Clear {
        #[arg(value_name = "PKG", help = "Only delete entries of these packages")]
        packages: Vec<String>,
        #[arg(
            long,
            conflicts_with = "packages",
            help = "Only delete caches written by older analyzer versions"
        )]
        obsolete: bool,
    },
}

impl CacheArgs {
    pub fn wants_json(&self) -> bool {
        self.json
    }
}

pub fn run(args: &CacheArgs, location: &CacheLocation) -> Result<(), Box<dyn Error>> {
    match args.command.as_ref().unwrap_or(&CacheCommand::Info) {
        CacheCommand::Info => info(location, args.json, None),
        CacheCommand::List { full } => info(location, args.json, Some(*full)),
        CacheCommand::Path => {
            let root = cache_root(location).ok_or("the cache is disabled")?;
            if args.json {
                println!("{:#}", json!({ "location": root }));
            } else {
                println!("{}", root.display());
            }
            Ok(())
        }
        CacheCommand::Clear { packages, obsolete } => {
            clear(location, args.json, packages, *obsolete)
        }
    }
}

fn info(location: &CacheLocation, json: bool, listing: Option<bool>) -> Result<(), Box<dyn Error>> {
    let report = inspect_cache(location)?;
    if json {
        println!("{:#}", serde_json::to_value(&report)?);
    } else if let Some(full) = listing {
        print!("{}", render(&report, full));
    } else {
        print!("{}", summarize(&report));
    }
    Ok(())
}

fn summarize(report: &CacheReport) -> String {
    use std::fmt::Write;
    let mut out = String::new();
    match &report.location {
        Some(location) => {
            let _ = writeln!(out, "cache: {}", location.display());
        }
        None => out.push_str(
            "cache: disabled
",
        ),
    }
    let total: u64 = report.schemas.iter().map(|schema| schema.bytes).sum();
    let _ = writeln!(out, "size:  {}", size(total));
    for schema in &report.schemas {
        let state = if schema.current {
            "current"
        } else {
            "obsolete"
        };
        let _ = writeln!(
            out,
            "  {} ({state}): {}, {} entries, {} packages",
            schema.schema,
            size(schema.bytes),
            schema.entries,
            schema.packages.len()
        );
    }
    out.push_str("run `slinker cache list` for packages, `slinker cache --json` for everything\n");
    out
}

fn clear(
    location: &CacheLocation,
    json: bool,
    packages: &[String],
    obsolete: bool,
) -> Result<(), Box<dyn Error>> {
    let scope = if !packages.is_empty() {
        ClearScope::Packages(packages.to_vec())
    } else if obsolete {
        ClearScope::Obsolete
    } else {
        ClearScope::All
    };
    let outcome = clear_cache(location, &scope)?;
    if json {
        println!(
            "{:#}",
            json!({
                "status": "ok",
                "entries_removed": outcome.entries_removed,
                "bytes_removed": outcome.bytes_removed,
            })
        );
    } else {
        println!(
            "removed {} entries ({})",
            outcome.entries_removed,
            size(outcome.bytes_removed)
        );
    }
    Ok(())
}

#[allow(clippy::cast_precision_loss)]
fn size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KiB", "MiB", "GiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

fn shortened(text: &str, full: bool) -> &str {
    if full {
        text
    } else {
        text.get(..12).unwrap_or(text)
    }
}

fn render(report: &CacheReport, full: bool) -> String {
    use std::fmt::Write;
    let mut out = String::new();
    match &report.location {
        Some(location) => {
            let _ = writeln!(out, "cache: {}", location.display());
        }
        None => out.push_str("cache: disabled\n"),
    }
    if report.schemas.is_empty() {
        out.push_str("empty\n");
        return out;
    }
    for schema in &report.schemas {
        out.push('\n');
        render_schema(&mut out, schema, full);
    }
    out
}

fn render_schema(out: &mut String, schema: &SchemaCacheReport, full: bool) {
    use std::fmt::Write;
    let state = if schema.current {
        "current"
    } else {
        "obsolete"
    };
    let _ = writeln!(
        out,
        "{} ({state}): {} in {} database file(s), {} entries",
        schema.schema,
        size(schema.bytes),
        schema.files,
        schema.entries
    );
    let kinds = EntryKind::ALL
        .iter()
        .filter_map(|kind| {
            let totals = schema.by_kind.get(kind)?;
            Some(format!(
                "{} {} ({})",
                kind,
                totals.entries,
                size(totals.bytes as u64)
            ))
        })
        .collect::<Vec<_>>();
    if !kinds.is_empty() {
        let _ = writeln!(out, "  {}", kinds.join(" | "));
    }
    if schema.unrecognized_entries > 0 {
        let _ = writeln!(
            out,
            "  {} unrecognized entries",
            schema.unrecognized_entries
        );
    }
    if schema.packages.is_empty() {
        return;
    }
    let width = schema
        .packages
        .iter()
        .map(|package| package.name.len())
        .max()
        .unwrap_or(7)
        .max(7);
    let _ = writeln!(
        out,
        "  {:<width$}  {:<9} {:<14} {:<14} {:>8} {:>5} {:>10}",
        "package", "version", "image", "key", "bindings", "envs", "size"
    );
    for package in &schema.packages {
        let _ = writeln!(
            out,
            "  {:<width$}  {:<9} {:<14} {:<14} {:>8} {:>5} {:>10}",
            package.name,
            package.version.as_deref().unwrap_or("?"),
            shortened(package.image_fingerprint.as_deref().unwrap_or("?"), full),
            shortened(&package.cache_key, full),
            package.bindings,
            package.environments,
            size(package.bytes as u64),
        );
    }
}
