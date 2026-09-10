use std::collections::BTreeSet;
use std::env;
use std::error::Error;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use hrm::analysis::{LinkPlan, Linker, Need, NodeKind};
use hrm::build::Rewrite;
use hrm::package::PackageStore;
use hrm::{RToolchain, TargetEnvironment, TargetEnvironmentRequest};

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("hrm: {error}");
            let mut source = error.source();
            while let Some(cause) = source {
                eprintln!("  caused by: {cause}");
                source = cause.source();
            }
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), Box<dyn Error>> {
    match parse_args(env::args_os().skip(1))? {
        Command::Help => {
            print_help();
            Ok(())
        }
        Command::Version => {
            println!("hrm {VERSION}");
            Ok(())
        }
        Command::Analyze(args) => analyze(&args),
    }
}

#[derive(Debug, Eq, PartialEq)]
enum Command {
    Analyze(AnalyzeArgs),
    Help,
    Version,
}

#[derive(Debug, Eq, PartialEq)]
struct AnalyzeArgs {
    root: String,
    libraries: Vec<PathBuf>,
    target_provided: BTreeSet<String>,
    jobs: usize,
}

fn parse_args(args: impl IntoIterator<Item = std::ffi::OsString>) -> Result<Command, CliError> {
    let mut args = args.into_iter();
    let Some(command) = args.next() else {
        return Err(CliError::Usage("missing command"));
    };
    match command.to_str() {
        Some("--help" | "-h") => {
            reject_extra(args)?;
            Ok(Command::Help)
        }
        Some("--version" | "-V") => {
            reject_extra(args)?;
            Ok(Command::Version)
        }
        Some("analyze") => parse_analyze_args(args).map(Command::Analyze),
        _ => Err(CliError::Usage("expected `analyze`")),
    }
}

fn parse_analyze_args(
    args: impl Iterator<Item = std::ffi::OsString>,
) -> Result<AnalyzeArgs, CliError> {
    let mut args = args.peekable();
    let mut root = None;
    let mut libraries = Vec::new();
    let mut target_provided = BTreeSet::new();
    let mut jobs = default_jobs();
    while let Some(argument) = args.next() {
        let text = argument.to_string_lossy();
        if text == "--lib" {
            let value = args
                .next()
                .ok_or(CliError::Usage("`--lib` requires a library path"))?;
            libraries.push(PathBuf::from(value));
            continue;
        }
        if let Some(value) = text.strip_prefix("--lib=") {
            if value.is_empty() {
                return Err(CliError::Usage("`--lib` requires a library path"));
            }
            libraries.push(PathBuf::from(value));
            continue;
        }
        if text == "--target-provided" {
            let value = args.next().ok_or(CliError::Usage(
                "`--target-provided` requires a package list",
            ))?;
            insert_package_list(&mut target_provided, &value)?;
            continue;
        }
        if let Some(value) = text.strip_prefix("--target-provided=") {
            insert_package_list_str(&mut target_provided, value)?;
            continue;
        }
        if text == "--jobs" {
            let value = args
                .next()
                .ok_or(CliError::Usage("`--jobs` requires a positive integer"))?;
            jobs = parse_jobs(&value)?;
            continue;
        }
        if let Some(value) = text.strip_prefix("--jobs=") {
            jobs = parse_jobs_str(value)?;
            continue;
        }
        if text.starts_with('-') {
            return Err(CliError::Usage("unknown analyze option"));
        }
        let package = argument
            .into_string()
            .map_err(|_| CliError::Usage("package name must be valid UTF-8"))?;
        if package.contains('/') || package.contains('\\') {
            return Err(CliError::Usage(
                "analyze expects an installed package name, not a source path",
            ));
        }
        if root.replace(package).is_some() {
            return Err(CliError::Usage(
                "analyze accepts one installed package name",
            ));
        }
    }
    Ok(AnalyzeArgs {
        root: root.ok_or(CliError::Usage(
            "analyze requires an installed package name",
        ))?,
        libraries,
        target_provided,
        jobs,
    })
}

fn analyze(args: &AnalyzeArgs) -> Result<(), Box<dyn Error>> {
    let r = env::var_os("HRM_R")
        .map(PathBuf::from)
        .unwrap_or_else(default_r_executable);
    let toolchain = RToolchain::from_r(r);
    let scratch = ScratchDir::new()?;
    let mut target_request = TargetEnvironmentRequest::new(scratch.path().join("target"));
    target_request.libraries = args
        .libraries
        .iter()
        .map(|library| absolute_path(library))
        .collect::<io::Result<Vec<_>>>()?;
    let target = toolchain.capture_target_environment(&target_request)?;

    let store = PackageStore::new(
        toolchain,
        target.clone(),
        args.target_provided.iter().cloned(),
        scratch.path().join("linker"),
    )?;
    let plan = Linker::new(store, args.jobs).analyze(&args.root)?;
    print_analysis(&target, &args.root, &plan)?;
    Ok(())
}

fn print_analysis(
    target: &TargetEnvironment,
    root_name: &str,
    plan: &LinkPlan,
) -> Result<(), Box<dyn Error>> {
    let root = plan
        .images
        .iter()
        .find(|(id, _)| id.name == root_name)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "root image missing from link plan",
            )
        })?;
    let root_id = root.0;
    let root_image = root.1;

    println!("package: {} {}", root_id.name, root_id.version);
    println!("image: {}", root_id.root.display());
    println!("origin: installed image from {}", root_id.library.display());
    println!(
        "target: R {} | {} | {}",
        target.target.r_version, target.target.os, target.target.arch
    );
    println!();

    println!("library universe");
    for (index, library) in target.libraries.iter().enumerate() {
        println!("  [{index}] {}", library.display());
    }
    println!();

    println!("root image");
    println!("  installed bytes: {}", directory_bytes(&root_id.root)?);
    println!("  bindings: {}", root_image.index.binding_names.len());
    println!("  exports: {}", root_image.index.exports.len());
    println!("  resources: {}", root_image.index.resources.len());
    println!("  S3 registrations: {}", root_image.index.s3.len());
    println!("  dynamic libraries: {}", root_image.index.dynlibs.len());
    println!(
        "  .onLoad present: {}",
        if root_image.index.lifecycle.on_load {
            "yes"
        } else {
            "no"
        }
    );
    println!("  inspection: one structured installed lazy-load image; Air parsing is binding-lazy");
    println!();

    println!("semantic package closure");
    let mut packages = plan
        .images
        .keys()
        .filter(|id| id.name != root_name)
        .collect::<Vec<_>>();
    packages.sort_by(|left, right| left.name.cmp(&right.name));
    if packages.is_empty() {
        println!("  none internalized");
    } else {
        for package in &packages {
            println!(
                "  {} {}: demanded installed image",
                package.name, package.version
            );
            println!("    {}", package.root.display());
        }
    }
    let mut externals = plan
        .graph
        .nodes
        .iter()
        .filter_map(|node| match &node.kind {
            NodeKind::ExternalBinding { .. } => Some(node.package.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    externals.sort();
    externals.dedup();
    for package in externals {
        println!("  {package}: exact target-provided namespace");
    }
    println!();

    println!("graph");
    println!("  nodes: {}", plan.graph.nodes.len());
    println!("  edges: {}", plan.graph.edges.len());
    println!("  roots: {}", plan.roots.len());
    println!("  processed semantic needs: {}", plan.retained.len());
    println!("  installed images inspected: {}", plan.inspected_packages);
    println!("  bindings Air-parsed: {}", plan.parsed_bindings);
    println!();

    println!("retention");
    for package in &packages {
        let image = plan.images.get(*package).expect("retained package image");
        let mut kept = plan
            .retained
            .iter()
            .filter_map(|need| match need {
                Need::Binding {
                    package: owner,
                    binding,
                } if owner == *package => Some(binding.clone()),
                _ => None,
            })
            .collect::<Vec<_>>();
        kept.sort();
        let dropped = image.index.binding_names.len().saturating_sub(kept.len());
        println!(
            "  {}: {}/{} bindings retained; {} discarded",
            package.name,
            kept.len(),
            image.index.binding_names.len(),
            dropped
        );
        if !kept.is_empty() {
            println!("    keep: {}", kept.join(", "));
        }
    }
    if packages.is_empty() {
        println!("  no third-party runtime bindings internalized");
    }
    println!();

    println!("blockers");
    if plan.diagnostics.is_empty() {
        println!("  none");
    } else {
        for diagnostic in &plan.diagnostics {
            let owner = diagnostic
                .binding
                .as_ref()
                .map(|binding| format!("{}::{binding}", diagnostic.package))
                .unwrap_or_else(|| diagnostic.package.clone());
            let location = diagnostic
                .span
                .as_ref()
                .and_then(|span| plan.sources.display(&span.source))
                .unwrap_or(owner);
            println!(
                "  - {:?} [{}]: {}",
                diagnostic.code, location, diagnostic.message
            );
        }
    }
    println!();

    let namespace_rewrites = plan
        .rewrites
        .iter()
        .filter(|rewrite| matches!(rewrite, Rewrite::NamespaceAccess { .. }))
        .count();
    let resource_rewrites = plan
        .rewrites
        .iter()
        .filter(|rewrite| matches!(rewrite, Rewrite::ResourceAccess { .. }))
        .count();
    let discovery_rewrites = plan
        .rewrites
        .iter()
        .filter(|rewrite| matches!(rewrite, Rewrite::SpecializedDiscovery { .. }))
        .count();
    println!("hermetification plan");
    println!("  root package: {} {}", root_id.name, root_id.version);
    println!("  synthetic namespaces: {}", packages.len());
    println!("  package-qualified rewrites: {namespace_rewrites}");
    println!("  resource rewrites: {resource_rewrites}");
    println!("  specialized discovery rewrites: {discovery_rewrites}");
    println!("  analysis model: binding-level demand-driven installed-image linker");
    println!("  package discovery rounds: none");
    println!("  per-binding temporary R files: none");
    println!(
        "  analysis status: {}",
        if plan.diagnostics.is_empty() {
            "link plan complete"
        } else {
            "blocked"
        }
    );
    println!("  build status: graph-driven materialization and rewriting not wired yet");
    Ok(())
}

fn parse_jobs(value: &std::ffi::OsString) -> Result<usize, CliError> {
    let value = value
        .to_str()
        .ok_or(CliError::Usage("`--jobs` must be valid UTF-8"))?;
    parse_jobs_str(value)
}

fn parse_jobs_str(value: &str) -> Result<usize, CliError> {
    value
        .parse::<usize>()
        .ok()
        .filter(|jobs| *jobs > 0)
        .ok_or(CliError::Usage("`--jobs` requires a positive integer"))
}

fn default_jobs() -> usize {
    std::thread::available_parallelism()
        .map(|value| value.get())
        .unwrap_or(1)
        .min(8)
}

fn insert_package_list(
    packages: &mut BTreeSet<String>,
    value: &std::ffi::OsString,
) -> Result<(), CliError> {
    let value = value.to_str().ok_or(CliError::Usage(
        "target-provided package names must be valid UTF-8",
    ))?;
    insert_package_list_str(packages, value)
}

fn insert_package_list_str(packages: &mut BTreeSet<String>, value: &str) -> Result<(), CliError> {
    for package in value.split(',').map(str::trim) {
        if package.is_empty() {
            return Err(CliError::Usage(
                "target-provided package list contains an empty name",
            ));
        }
        packages.insert(package.to_owned());
    }
    Ok(())
}

fn reject_extra(mut args: impl Iterator<Item = std::ffi::OsString>) -> Result<(), CliError> {
    if args.next().is_some() {
        Err(CliError::Usage("too many arguments"))
    } else {
        Ok(())
    }
}

fn print_help() {
    println!(
        "hrm {VERSION}\n\n\
         Usage:\n  hrm analyze PACKAGE [--lib PATH]... [--target-provided PKG[,PKG...]] [--jobs N]\n\n\
         Link an installed R package image by following reachable semantic bindings.\n\
         hrm never installs, rebuilds, or downloads packages, and never recursively resolves DESCRIPTION dependencies.\n\n\
         Options:\n\
           --lib PATH                     select an installed R library (repeatable, ordered)\n\
           --target-provided PKG[,PKG...] leave these exact third-party namespaces external\n\
           --jobs N                       analysis workers (default: min(CPUs, 8))\n\n\
         Environment:\n\
           HRM_R          target R executable (defaults to R/R.exe from PATH)\n\
           HRM_CACHE_DIR  persistent installed-image analysis cache"
    );
}

fn directory_bytes(root: &Path) -> io::Result<u64> {
    let mut total = 0u64;
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory)? {
            let entry = entry?;
            let file_type = entry.file_type()?;
            if file_type.is_dir() {
                pending.push(entry.path());
            } else if file_type.is_file() {
                total = total.saturating_add(entry.metadata()?.len());
            }
        }
    }
    Ok(total)
}

fn absolute_path(path: &Path) -> io::Result<PathBuf> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(env::current_dir()?.join(path))
    }
}

fn default_r_executable() -> PathBuf {
    if cfg!(windows) {
        PathBuf::from("R.exe")
    } else {
        PathBuf::from("R")
    }
}

struct ScratchDir {
    path: PathBuf,
}
impl ScratchDir {
    fn new() -> io::Result<Self> {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let path = env::temp_dir().join(format!("hrm-{}-{nonce}", std::process::id()));
        fs::create_dir_all(&path)?;
        Ok(Self { path })
    }
    fn path(&self) -> &Path {
        &self.path
    }
}
impl Drop for ScratchDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

#[derive(Debug)]
enum CliError {
    Usage(&'static str),
}
impl std::fmt::Display for CliError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Usage(message) => write!(f, "{message}; run `hrm --help`"),
        }
    }
}
impl Error for CliError {}

#[cfg(test)]
mod tests {
    use super::{AnalyzeArgs, Command, default_jobs, parse_args};
    use std::collections::BTreeSet;
    use std::ffi::OsString;
    use std::path::PathBuf;
    fn os(values: &[&str]) -> Vec<OsString> {
        values.iter().map(OsString::from).collect()
    }

    #[test]
    fn analyze_requires_installed_package_name() {
        assert!(parse_args(os(&["analyze"])).is_err());
        assert!(parse_args(os(&["analyze", "./voucher"])).is_err());
    }

    #[test]
    fn accepts_installed_package_and_ordered_libraries() {
        assert_eq!(
            parse_args(os(&["analyze", "voucher", "--lib", "one", "--lib=two"])).unwrap(),
            Command::Analyze(AnalyzeArgs {
                root: "voucher".into(),
                libraries: vec![PathBuf::from("one"), PathBuf::from("two")],
                target_provided: BTreeSet::new(),
                jobs: default_jobs(),
            })
        );
    }

    #[test]
    fn accepts_explicit_jobs() {
        assert_eq!(
            parse_args(os(&["analyze", "voucher", "--jobs", "6"])).unwrap(),
            Command::Analyze(AnalyzeArgs {
                root: "voucher".into(),
                libraries: Vec::new(),
                target_provided: BTreeSet::new(),
                jobs: 6,
            })
        );
        assert!(parse_args(os(&["analyze", "voucher", "--jobs=0"])).is_err());
    }
}
