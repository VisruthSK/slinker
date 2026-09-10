use std::collections::BTreeSet;
use std::env;
use std::error::Error;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use hrm::analysis::{Edge, LinkPlan, Linker, Need, NodeId, NodeKind};
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
        Command::Why(args) => explain_why(&args),
        Command::Path(args) => explain_paths(&args),
    }
}

#[derive(Debug, Eq, PartialEq)]
enum Command {
    Analyze(AnalyzeArgs),
    Why(QueryArgs),
    Path(QueryArgs),
    Help,
    Version,
}

#[derive(Debug, Eq, PartialEq)]
struct AnalyzeArgs {
    root: String,
    libraries: Vec<PathBuf>,
    target_provided: BTreeSet<String>,
    extra_pkgs: BTreeSet<String>,
    jobs: usize,
}

#[derive(Debug, Eq, PartialEq)]
struct QueryArgs {
    root: String,
    target: String,
    libraries: Vec<PathBuf>,
    target_provided: BTreeSet<String>,
    extra_pkgs: BTreeSet<String>,
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
        Some("why") => parse_query_args(args).map(Command::Why),
        Some("path") => parse_query_args(args).map(Command::Path),
        _ => Err(CliError::Usage("expected `analyze`, `why`, or `path`")),
    }
}

fn parse_analyze_args(args: impl Iterator<Item = std::ffi::OsString>) -> Result<AnalyzeArgs, CliError> {
    let mut args = args.peekable();
    let mut root = None;
    let mut libraries = Vec::new();
    let mut target_provided = BTreeSet::new();
    let mut extra_pkgs = BTreeSet::new();
    let mut jobs = default_jobs();
    while let Some(argument) = args.next() {
        let text = argument.to_string_lossy();
        if text == "--lib" {
            let value = args.next().ok_or(CliError::Usage("`--lib` requires a library path"))?;
            libraries.push(PathBuf::from(value));
            continue;
        }
        if let Some(value) = text.strip_prefix("--lib=") {
            if value.is_empty() { return Err(CliError::Usage("`--lib` requires a library path")); }
            libraries.push(PathBuf::from(value));
            continue;
        }
        if text == "--target-provided" {
            let value = args.next().ok_or(CliError::Usage("`--target-provided` requires a package list"))?;
            insert_package_list(&mut target_provided, &value)?;
            continue;
        }
        if let Some(value) = text.strip_prefix("--target-provided=") {
            insert_package_list_str(&mut target_provided, value)?;
            continue;
        }
        if text == "--extra-pkgs" {
            collect_package_args(&mut extra_pkgs, &mut args, "`--extra-pkgs` requires at least one package")?;
            continue;
        }
        if text == "--jobs" {
            let value = args.next().ok_or(CliError::Usage("`--jobs` requires a positive integer"))?;
            jobs = parse_jobs(&value)?;
            continue;
        }
        if let Some(value) = text.strip_prefix("--jobs=") {
            jobs = parse_jobs_str(value)?;
            continue;
        }
        if text.starts_with('-') { return Err(CliError::Usage("unknown analyze option")); }
        let package = argument.into_string().map_err(|_| CliError::Usage("package name must be valid UTF-8"))?;
        if package.contains('/') || package.contains('\\') {
            return Err(CliError::Usage("analyze expects an installed package name, not a source path"));
        }
        if root.replace(package).is_some() {
            return Err(CliError::Usage("analyze accepts one installed package name"));
        }
    }
    Ok(AnalyzeArgs {
        root: root.ok_or(CliError::Usage("analyze requires an installed package name"))?,
        libraries,
        target_provided,
        extra_pkgs,
        jobs,
    })
}

fn parse_query_args(args: impl Iterator<Item = std::ffi::OsString>) -> Result<QueryArgs, CliError> {
    let mut args = args.peekable();
    let mut positional = Vec::new();
    let mut libraries = Vec::new();
    let mut target_provided = BTreeSet::new();
    let mut extra_pkgs = BTreeSet::new();
    let mut jobs = default_jobs();
    while let Some(argument) = args.next() {
        let text = argument.to_string_lossy();
        if text == "--lib" {
            let value = args.next().ok_or(CliError::Usage("`--lib` requires a library path"))?;
            libraries.push(PathBuf::from(value));
            continue;
        }
        if let Some(value) = text.strip_prefix("--lib=") {
            if value.is_empty() { return Err(CliError::Usage("`--lib` requires a library path")); }
            libraries.push(PathBuf::from(value));
            continue;
        }
        if text == "--target-provided" {
            let value = args.next().ok_or(CliError::Usage("`--target-provided` requires a package list"))?;
            insert_package_list(&mut target_provided, &value)?;
            continue;
        }
        if let Some(value) = text.strip_prefix("--target-provided=") {
            insert_package_list_str(&mut target_provided, value)?;
            continue;
        }
        if text == "--extra-pkgs" {
            collect_package_args(&mut extra_pkgs, &mut args, "`--extra-pkgs` requires at least one package")?;
            continue;
        }
        if text == "--jobs" {
            let value = args.next().ok_or(CliError::Usage("`--jobs` requires a positive integer"))?;
            jobs = parse_jobs(&value)?;
            continue;
        }
        if let Some(value) = text.strip_prefix("--jobs=") {
            jobs = parse_jobs_str(value)?;
            continue;
        }
        if text.starts_with('-') { return Err(CliError::Usage("unknown query option")); }
        positional.push(argument.into_string().map_err(|_| CliError::Usage("package/binding names must be valid UTF-8"))?);
    }
    if positional.len() != 2 {
        return Err(CliError::Usage("why/path require ROOT and TARGET"));
    }
    let root = positional.remove(0);
    if root.contains('/') || root.contains('\\') {
        return Err(CliError::Usage("ROOT must be an installed package name"));
    }
    Ok(QueryArgs { root, target: positional.remove(0), libraries, target_provided, extra_pkgs, jobs })
}

fn link(args: &AnalyzeArgs) -> Result<(TargetEnvironment, LinkPlan), Box<dyn Error>> {
    let r = env::var_os("HRM_R").map(PathBuf::from).unwrap_or_else(default_r_executable);
    let toolchain = RToolchain::from_r(r);
    let scratch = ScratchDir::new()?;
    let mut target_request = TargetEnvironmentRequest::new(scratch.path().join("target"));
    target_request.libraries = args.libraries.iter().map(|library| absolute_path(library)).collect::<io::Result<Vec<_>>>()?;
    let target = toolchain.capture_target_environment(&target_request)?;

    let store = PackageStore::new(
        toolchain,
        target.clone(),
        args.target_provided.iter().cloned(),
        scratch.path().join("linker"),
    )?;
    let plan = Linker::new(store, args.jobs)
        .with_extra_packages(args.extra_pkgs.iter().cloned())
        .analyze(&args.root)?;
    Ok((target, plan))
}

fn analyze(args: &AnalyzeArgs) -> Result<(), Box<dyn Error>> {
    let (target, plan) = link(args)?;
    print_analysis(&target, &args.root, &plan)?;
    Ok(())
}

fn query_link_args(args: &QueryArgs) -> AnalyzeArgs {
    AnalyzeArgs {
        root: args.root.clone(),
        libraries: args.libraries.clone(),
        target_provided: args.target_provided.clone(),
        extra_pkgs: args.extra_pkgs.clone(),
        jobs: args.jobs,
    }
}

fn explain_why(args: &QueryArgs) -> Result<(), Box<dyn Error>> {
    let (_, plan) = link(&query_link_args(args))?;
    let targets = matching_nodes(&plan, &args.target);
    if targets.is_empty() {
        println!("{} is not in the semantic closure of {}.", args.target, args.root);
        return Ok(());
    }

    let best = targets
        .iter()
        .filter_map(|target| plan.graph.shortest_path(&plan.roots, *target).map(|path| (*target, path)))
        .min_by_key(|(_, path)| path.len());
    let Some((target, path)) = best else {
        println!("{} exists in the graph but is not reachable from {}.", args.target, args.root);
        return Ok(());
    };

    println!("{} retained because:", node_label(&plan, target));
    print_edge_path(&plan, &path);

    let uses = package_entry_edges(&plan, &args.target);
    if !uses.is_empty() {
        println!();
        println!("direct semantic uses:");
        for edge in uses {
            println!(
                "  {} -- {:?}{}: {} --> {}",
                node_label(&plan, edge.from),
                edge.kind,
                edge_location(&plan, edge),
                edge.reason,
                node_label(&plan, edge.to)
            );
        }
    }
    Ok(())
}

fn explain_paths(args: &QueryArgs) -> Result<(), Box<dyn Error>> {
    let (_, plan) = link(&query_link_args(args))?;
    let entries = package_entry_edges(&plan, &args.target);
    if entries.is_empty() {
        let targets = matching_nodes(&plan, &args.target);
        if targets.is_empty() {
            println!("{} is not in the semantic closure of {}.", args.target, args.root);
        } else {
            println!("{} has no cross-package entry edge from {}.", args.target, args.root);
        }
        return Ok(());
    }

    println!("semantic paths from {} to {}:", args.root, args.target);
    for (ordinal, entry) in entries.iter().enumerate() {
        println!();
        println!("path {}:", ordinal + 1);
        if let Some(mut prefix) = plan.graph.shortest_path(&plan.roots, entry.from) {
            prefix.push(entry);
            print_edge_path(&plan, &prefix);
        } else {
            println!(
                "  {} -- {:?}{}: {} --> {}",
                node_label(&plan, entry.from),
                entry.kind,
                edge_location(&plan, entry),
                entry.reason,
                node_label(&plan, entry.to)
            );
        }
    }
    Ok(())
}

fn matching_nodes(plan: &LinkPlan, target: &str) -> Vec<NodeId> {
    let (package, binding) = if let Some((package, binding)) = target.split_once(":::") {
        (package, Some(binding))
    } else if let Some((package, binding)) = target.split_once("::") {
        (package, Some(binding))
    } else {
        (target, None)
    };
    plan.graph
        .nodes_for_package(package)
        .filter(|id| match (binding, &plan.graph.nodes[id.0].kind) {
            (None, _) => true,
            (Some(name), NodeKind::Binding { name: binding } | NodeKind::ExternalBinding { name: binding }) => binding == name,
            _ => false,
        })
        .collect()
}

fn package_entry_edges<'a>(plan: &'a LinkPlan, target: &str) -> Vec<&'a Edge> {
    let package = target.split_once(":::").map(|x| x.0)
        .or_else(|| target.split_once("::").map(|x| x.0))
        .unwrap_or(target);
    let binding = target.split_once(":::").map(|x| x.1)
        .or_else(|| target.split_once("::").map(|x| x.1));
    let mut edges = plan.graph.edges.iter().filter(|edge| {
        let to = &plan.graph.nodes[edge.to.0];
        let from = &plan.graph.nodes[edge.from.0];
        if to.package != package || from.package == package { return false; }
        match (binding, &to.kind) {
            (None, _) => true,
            (Some(name), NodeKind::Binding { name: actual } | NodeKind::ExternalBinding { name: actual }) => actual == name,
            _ => false,
        }
    }).collect::<Vec<_>>();
    edges.sort_by_key(|edge| (edge.from.0, edge.to.0));
    edges
}

fn print_edge_path(plan: &LinkPlan, path: &[&Edge]) {
    if path.is_empty() {
        println!("  explicit root");
        return;
    }
    println!("  {}", node_label(plan, path[0].from));
    for edge in path {
        println!("    -- {:?}{}: {}", edge.kind, edge_location(plan, edge), edge.reason);
        println!("    -> {}", node_label(plan, edge.to));
    }
}

fn edge_location(plan: &LinkPlan, edge: &Edge) -> String {
    let Some(span) = &edge.span else { return String::new() };
    let source = plan.sources.display(&span.source).unwrap_or_else(|| format!("source#{}", span.source.0));
    format!(" @ {source}:{}..{}", span.start, span.end)
}

fn node_label(plan: &LinkPlan, id: NodeId) -> String {
    let node = &plan.graph.nodes[id.0];
    match &node.kind {
        NodeKind::Binding { name } | NodeKind::ExternalBinding { name } => format!("{}::{name}", node.package),
        NodeKind::Activation => format!("{} [activation]", node.package),
        NodeKind::Dataset { name } => format!("{} [dataset {name}]", node.package),
        NodeKind::Lifecycle { hook } => format!("{}::{hook} [lifecycle]", node.package),
        NodeKind::S3Registration { generic, class } => format!("{} [S3 {generic}/{class}]", node.package),
        NodeKind::Resource { path } => format!("{} [resource {path}]", node.package),
        NodeKind::NativeComponent { name } => format!("{} [native {name}]", node.package),
        NodeKind::PackageMetadata { name } => format!("{} [metadata {name}]", node.package),
        NodeKind::MissingPackage => format!("{} [missing package]", node.package),
        NodeKind::Rejection { code } => format!("{} [rejection {code}]", node.package),
    }
}

fn print_analysis(target: &TargetEnvironment, root_name: &str, plan: &LinkPlan) -> Result<(), Box<dyn Error>> {
    let root = plan.images.iter().find(|(id, _)| id.name == root_name).ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidData, "root image missing from link plan")
    })?;
    let root_id = root.0;
    let root_image = root.1;

    println!("package: {} {}", root_id.name, root_id.version);
    println!("image: {}", root_id.root.display());
    println!("origin: installed image from {}", root_id.library.display());
    println!("target: R {} | {} | {}", target.target.r_version, target.target.os, target.target.arch);
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
    println!("  installed files/directories indexed: {}", root_image.index.files.len());
    println!("  S3 registrations: {}", root_image.index.s3.len());
    println!("  dynamic libraries: {}", root_image.index.dynlibs.len());
    println!("  .onLoad present: {}", if root_image.index.lifecycle.on_load { "yes" } else { "no" });
    println!("  inspection: one structured installed lazy-load image; Air parsing is binding-lazy");
    println!();

    println!("semantic package closure");
    let mut packages = plan.packages.iter()
        .filter(|id| id.name != root_name && !plan.target_provided.contains(*id))
        .collect::<Vec<_>>();
    packages.sort_by(|left, right| left.name.cmp(&right.name));
    if packages.is_empty() {
        println!("  none internalized");
    } else {
        for package in &packages {
            let state = if plan.images.contains_key(*package) { "demanded installed image" } else { "activation/index only" };
            println!("  {} {}: {state}", package.name, package.version);
            println!("    {}", package.root.display());
        }
    }
    let mut externals = plan.target_provided.iter().collect::<Vec<_>>();
    externals.sort_by(|left, right| left.name.cmp(&right.name));
    for package in externals {
        println!("  {} {}: exact target-provided namespace", package.name, package.version);
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
        let mut kept = plan.retained.iter().filter_map(|need| match need {
            Need::Binding { package: owner, binding } if owner == *package => Some(binding.clone()),
            _ => None,
        }).collect::<Vec<_>>();
        kept.sort();
        if let Some(image) = plan.images.get(*package) {
            let dropped = image.index.binding_names.len().saturating_sub(kept.len());
            println!("  {}: {}/{} bindings retained; {} discarded", package.name, kept.len(), image.index.binding_names.len(), dropped);
        } else {
            println!("  {}: activation/index only; no R binding image forced", package.name);
        }
        if !kept.is_empty() { println!("    keep: {}", kept.join(", ")); }
    }
    if packages.is_empty() { println!("  no third-party runtime bindings internalized"); }
    println!();

    println!("missing dependencies");
    let mut missing = plan.graph.missing_packages().collect::<Vec<_>>();
    missing.sort_by(|left, right| plan.graph.nodes[left.0].package.cmp(&plan.graph.nodes[right.0].package));
    if missing.is_empty() {
        println!("  none");
    } else {
        for node in missing {
            let package = &plan.graph.nodes[node.0].package;
            println!("  {package}");
            let mut incoming = plan.graph.incoming(node).collect::<Vec<_>>();
            incoming.sort_by_key(|edge| edge.from.0);
            for edge in incoming {
                println!("    {} -- {:?}{}: {}", node_label(plan, edge.from), edge.kind, edge_location(plan, edge), edge.reason);
            }
        }
    }
    println!();

    println!("blockers");
    let blockers = plan.diagnostics.iter().filter(|diagnostic| !matches!(diagnostic.code, hrm::analysis::RejectCode::MissingDependency)).collect::<Vec<_>>();
    if blockers.is_empty() {
        println!("  none");
    } else {
        for diagnostic in blockers {
            let owner = diagnostic.binding.as_ref().map(|binding| format!("{}::{binding}", diagnostic.package)).unwrap_or_else(|| diagnostic.package.clone());
            let location = diagnostic.span.as_ref().and_then(|span| plan.sources.display(&span.source)).unwrap_or(owner);
            println!("  - {:?} [{}]: {}", diagnostic.code, location, diagnostic.message);
        }
    }
    println!();

    let namespace_rewrites = plan.rewrites.iter().filter(|rewrite| matches!(rewrite, Rewrite::NamespaceAccess { .. })).count();
    let resource_rewrites = plan.rewrites.iter().filter(|rewrite| matches!(rewrite, Rewrite::ResourceAccess { .. })).count();
    let discovery_rewrites = plan.rewrites.iter().filter(|rewrite| matches!(rewrite, Rewrite::PackageOperation { .. })).count();
    println!("hermetification plan");
    println!("  root package: {} {}", root_id.name, root_id.version);
    println!("  synthetic namespaces: {}", packages.len());
    println!("  package-qualified rewrites: {namespace_rewrites}");
    println!("  resource rewrites: {resource_rewrites}");
    println!("  specialized discovery rewrites: {discovery_rewrites}");
    println!("  analysis model: binding-level demand-driven installed-image linker");
    println!("  package discovery rounds: none");
    println!("  per-binding temporary R files: none");
    println!("  analysis status: {}", if plan.diagnostics.is_empty() { "link plan complete" } else { "blocked" });
    println!("  build status: graph-driven materialization and rewriting not wired yet");
    Ok(())
}

fn parse_jobs(value: &std::ffi::OsString) -> Result<usize, CliError> {
    let value = value.to_str().ok_or(CliError::Usage("`--jobs` must be valid UTF-8"))?;
    parse_jobs_str(value)
}

fn parse_jobs_str(value: &str) -> Result<usize, CliError> {
    value.parse::<usize>().ok().filter(|jobs| *jobs > 0).ok_or(CliError::Usage("`--jobs` requires a positive integer"))
}

fn default_jobs() -> usize {
    std::thread::available_parallelism().map(|value| value.get()).unwrap_or(1).min(8)
}

fn insert_package_list(packages: &mut BTreeSet<String>, value: &std::ffi::OsString) -> Result<(), CliError> {
    let value = value.to_str().ok_or(CliError::Usage("target-provided package names must be valid UTF-8"))?;
    insert_package_list_str(packages, value)
}

fn insert_package_list_str(packages: &mut BTreeSet<String>, value: &str) -> Result<(), CliError> {
    for package in value.split(',').map(str::trim) {
        if package.is_empty() { return Err(CliError::Usage("target-provided package list contains an empty name")); }
        packages.insert(package.to_owned());
    }
    Ok(())
}

fn collect_package_args(
    packages: &mut BTreeSet<String>,
    args: &mut std::iter::Peekable<impl Iterator<Item = std::ffi::OsString>>,
    missing: &'static str,
) -> Result<(), CliError> {
    let mut count = 0usize;
    while let Some(next) = args.peek() {
        if next.to_string_lossy().starts_with('-') {
            break;
        }
        let value = args.next().expect("peeked argument exists");
        let package = value.into_string().map_err(|_| CliError::Usage("package name must be valid UTF-8"))?;
        if package.is_empty() || package.contains(',') || package.contains('/') || package.contains('\\') {
            return Err(CliError::Usage("extra packages must be space-separated package names"));
        }
        packages.insert(package);
        count += 1;
    }
    if count == 0 {
        return Err(CliError::Usage(missing));
    }
    Ok(())
}

fn reject_extra(mut args: impl Iterator<Item = std::ffi::OsString>) -> Result<(), CliError> {
    if args.next().is_some() { Err(CliError::Usage("too many arguments")) } else { Ok(()) }
}

fn print_help() {
    println!(
        "hrm {VERSION}\n\n\
         Usage:\n  hrm analyze PACKAGE [--lib PATH]... [--target-provided PKG[,PKG...]] [--extra-pkgs PKG...] [--jobs N]\n  hrm why ROOT TARGET [same options]\n  hrm path ROOT DOWNSTREAM [same options]\n\n\
         Link an installed R package image by following reachable semantic bindings.\n  `why` prints a shortest provenance chain; `path` prints every cross-package use site.\n\
         hrm never installs, rebuilds, or downloads packages, and never recursively resolves DESCRIPTION dependencies.\n\n\
         Options:\n\
           --lib PATH                     select an installed R library (repeatable, ordered)\n\
           --target-provided PKG[,PKG...] leave these exact third-party namespaces external\n\
           --extra-pkgs PKG...             enable named optional packages when reachable\n\
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
            if file_type.is_dir() { pending.push(entry.path()); }
            else if file_type.is_file() { total = total.saturating_add(entry.metadata()?.len()); }
        }
    }
    Ok(total)
}

fn absolute_path(path: &Path) -> io::Result<PathBuf> {
    if path.is_absolute() { Ok(path.to_path_buf()) } else { Ok(env::current_dir()?.join(path)) }
}

fn default_r_executable() -> PathBuf {
    if cfg!(windows) { PathBuf::from("R.exe") } else { PathBuf::from("R") }
}

struct ScratchDir { path: PathBuf }
impl ScratchDir {
    fn new() -> io::Result<Self> {
        let nonce = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos();
        let path = env::temp_dir().join(format!("hrm-{}-{nonce}", std::process::id()));
        fs::create_dir_all(&path)?;
        Ok(Self { path })
    }
    fn path(&self) -> &Path { &self.path }
}
impl Drop for ScratchDir {
    fn drop(&mut self) { let _ = fs::remove_dir_all(&self.path); }
}

#[derive(Debug)]
enum CliError { Usage(&'static str) }
impl std::fmt::Display for CliError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self { Self::Usage(message) => write!(f, "{message}; run `hrm --help`") }
    }
}
impl Error for CliError {}

#[cfg(test)]
mod tests {
    use super::{AnalyzeArgs, Command, default_jobs, parse_args};
    use std::collections::BTreeSet;
    use std::ffi::OsString;
    use std::path::PathBuf;
    fn os(values: &[&str]) -> Vec<OsString> { values.iter().map(OsString::from).collect() }

    #[test]
    fn analyze_requires_installed_package_name() {
        assert!(parse_args(os(&["analyze"])).is_err());
        assert!(parse_args(os(&["analyze", "./voucher"])).is_err());
    }

    #[test]
    fn accepts_installed_package_and_ordered_libraries() {
        assert_eq!(parse_args(os(&["analyze", "voucher", "--lib", "one", "--lib=two"])).unwrap(), Command::Analyze(AnalyzeArgs {
            root: "voucher".into(), libraries: vec![PathBuf::from("one"), PathBuf::from("two")], target_provided: BTreeSet::new(), extra_pkgs: BTreeSet::new(), jobs: default_jobs(),
        }));
    }

    #[test]
    fn accepts_extra_packages_as_space_separated_values() {
        let mut extra_pkgs = BTreeSet::new();
        extra_pkgs.extend(["foo".to_owned(), "bar".to_owned(), "baz".to_owned()]);
        assert_eq!(parse_args(os(&["analyze", "voucher", "--extra-pkgs", "foo", "bar", "baz", "--jobs", "3"])).unwrap(), Command::Analyze(AnalyzeArgs {
            root: "voucher".into(), libraries: Vec::new(), target_provided: BTreeSet::new(), extra_pkgs, jobs: 3,
        }));
        assert!(parse_args(os(&["analyze", "voucher", "--extra-pkgs", "--jobs", "3"])).is_err());
        assert!(parse_args(os(&["analyze", "voucher", "--extra-pkgs", "foo,bar"])).is_err());
    }

    #[test]
    fn accepts_explicit_jobs() {
        assert_eq!(parse_args(os(&["analyze", "voucher", "--jobs", "6"])).unwrap(), Command::Analyze(AnalyzeArgs {
            root: "voucher".into(), libraries: Vec::new(), target_provided: BTreeSet::new(), extra_pkgs: BTreeSet::new(), jobs: 6,
        }));
        assert!(parse_args(os(&["analyze", "voucher", "--jobs=0"])).is_err());
    }
}
