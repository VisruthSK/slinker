use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::error::Error;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

mod resolve;

use resolve::{ResolvedPackage, Resolution, resolve_installed_closure};

use hrm::analysis::{Analysis, Analyzer, AnalyzerConfig, NodeKind, TargetPackage};
use hrm::{
    Description, RToolchain, SemanticSnapshot, TargetEnvironment, TargetEnvironmentRequest,
};

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

fn parse_analyze_args(args: impl Iterator<Item = std::ffi::OsString>) -> Result<AnalyzeArgs, CliError> {
    let mut args = args.peekable();
    let mut root = None;
    let mut libraries = Vec::new();
    let mut target_provided = BTreeSet::new();

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
            let value = args
                .next()
                .ok_or(CliError::Usage("`--target-provided` requires a package list"))?;
            insert_package_list(&mut target_provided, &value)?;
            continue;
        }
        if let Some(value) = text.strip_prefix("--target-provided=") {
            insert_package_list_str(&mut target_provided, value)?;
            continue;
        }
        if text.starts_with('-') {
            return Err(CliError::Usage("unknown analyze option"));
        }
        let package = argument
            .into_string()
            .map_err(|_| CliError::Usage("package name must be valid UTF-8"))?;
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
    })
}

fn insert_package_list(packages: &mut BTreeSet<String>, value: &std::ffi::OsString) -> Result<(), CliError> {
    let value = value
        .to_str()
        .ok_or(CliError::Usage("target-provided package names must be valid UTF-8"))?;
    insert_package_list_str(packages, value)
}

fn insert_package_list_str(packages: &mut BTreeSet<String>, value: &str) -> Result<(), CliError> {
    for package in value.split(',').map(str::trim) {
        if package.is_empty() {
            return Err(CliError::Usage("target-provided package list contains an empty name"));
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
         Usage:\n  hrm analyze PACKAGE [--lib PATH]... [--target-provided PKG[,PKG...]]\n\n\
         Link an installed R package image and its installed runtime dependency closure.\n\
         hrm never installs, rebuilds, or downloads packages. Install the package into\n\
         the library universe you want to analyze before invoking hrm.\n\n\
         Options:\n\
           --lib PATH                     select an installed R library (repeatable, ordered)\n\
           --target-provided PKG[,PKG...] leave only these third-party packages external\n\n\
         Environment:\n\
           HRM_R  target R executable (defaults to R/R.exe from PATH)"
    );
}

#[derive(Debug)]
struct InspectedPackage {
    resolved: ResolvedPackage,
    snapshot: SemanticSnapshot,
}

#[derive(Debug)]
struct RootImage {
    name: String,
    version: String,
    root: PathBuf,
    origin: String,
    description: Description,
    snapshot: SemanticSnapshot,
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

    let root = prepare_root(&toolchain, &target, &args.root, scratch.path())?;
    let mut additional = BTreeSet::new();
    let mut round = 0usize;

    let (resolution, dependencies, analysis) = loop {
        let resolution = resolve_installed_closure(
            &target,
            &root.description,
            &additional,
            &args.target_provided,
        )?;
        let dependencies = inspect_dependencies(
            &toolchain,
            &target,
            &resolution,
            &scratch.path().join(format!("round-{round}")),
        )?;
        let target_packages = resolution
            .platform
            .iter()
            .chain(resolution.target_provided.iter())
            .map(|(name, version)| TargetPackage {
                name: name.clone(),
                version: version.clone(),
            })
            .collect();
        let analysis = Analyzer::default().analyze(&AnalyzerConfig {
            root: root.snapshot.analysis_root.clone(),
            dependencies: dependencies
                .iter()
                .map(|dependency| dependency.snapshot.analysis_root.clone())
                .collect(),
            target_packages,
        })?;

        let present = dependencies
            .iter()
            .map(|dependency| dependency.resolved.installed.name.clone())
            .chain(resolution.platform.keys().cloned())
            .chain(resolution.target_provided.keys().cloned())
            .collect::<BTreeSet<_>>();
        let discovered = reachable_named_packages(&analysis)
            .into_iter()
            .filter(|name| {
                name != &root.name
                    && !present.contains(name)
                    && target.package(name).is_some()
            })
            .collect::<BTreeSet<_>>();
        let before = additional.len();
        additional.extend(discovered);
        if additional.len() == before {
            break (resolution, dependencies, analysis);
        }
        round += 1;
        if round > 16 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "installed dependency discovery did not converge after 16 rounds",
            )
            .into());
        }
    };

    print_analysis(&target, &root, &resolution, &dependencies, &analysis)?;
    Ok(())
}

fn prepare_root(
    toolchain: &RToolchain,
    target: &TargetEnvironment,
    name: &str,
    scratch: &Path,
) -> Result<RootImage, Box<dyn Error>> {
    let installed = target.package(name).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            format!("installed root package `{name}` is not present in the selected library universe"),
        )
    })?;
    let root = installed.library.join(name);
    let description = read_description(&root)?;
    let output = scratch.join("root-image.hrm");
    let snapshot = toolchain.inspect_package_snapshot_with_libraries(
        &installed.library,
        name,
        &output,
        &target.libraries,
    )?;
    Ok(RootImage {
        name: name.to_owned(),
        version: installed.version.clone(),
        root,
        origin: format!("installed image from {}", installed.library.display()),
        description,
        snapshot,
    })
}

fn inspect_dependencies(
    toolchain: &RToolchain,
    target: &TargetEnvironment,
    resolution: &Resolution,
    scratch: &Path,
) -> Result<Vec<InspectedPackage>, Box<dyn Error>> {
    let mut out = Vec::with_capacity(resolution.internalized.len());
    for (index, resolved) in resolution.internalized.iter().enumerate() {
        let output = scratch
            .join("images")
            .join(format!("{index:04}-{}.hrm", resolved.installed.name));
        if let Some(parent) = output.parent() {
            fs::create_dir_all(parent)?;
        }
        let snapshot = toolchain.inspect_package_snapshot_with_libraries(
            &resolved.installed.library,
            &resolved.installed.name,
            &output,
            &target.libraries,
        )?;
        out.push(InspectedPackage {
            resolved: resolved.clone(),
            snapshot,
        });
    }
    Ok(out)
}

fn reachable_named_packages(analysis: &Analysis) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for package in &analysis.packages {
        let mut ordinal = 0usize;
        for unit in &package.r_units {
            for expression in &unit.parsed.expressions {
                let reachable = analysis
                    .graph
                    .initialization(&package.id.name, ordinal)
                    .is_some_and(|node| analysis.reachable[node.0]);
                ordinal += 1;
                if !reachable {
                    continue;
                }
                for reference in &expression.package_refs {
                    out.insert(reference.package.clone());
                }
                for reference in &expression.resource_refs {
                    if let Some(name) = &reference.package {
                        out.insert(name.clone());
                    }
                }
                for call in &expression.calls {
                    if let Some(name) = static_package_name(call) {
                        out.insert(name.to_owned());
                    }
                }
            }
        }
    }
    out
}

fn static_package_name(call: &hrm::analysis::parser::CallSite) -> Option<&str> {
    use hrm::analysis::parser::StaticArg;
    let first = call.args.first()?.as_ref()?;
    match call.callee.as_str() {
        "library" | "require" => match first {
            StaticArg::String(name) | StaticArg::Symbol(name) => Some(name.as_str()),
        },
        "requireNamespace" | "loadNamespace" | "getNamespace" | "asNamespace"
        | "packageVersion" | "find.package" => match first {
            StaticArg::String(name) => Some(name.as_str()),
            StaticArg::Symbol(_) => None,
        },
        _ => None,
    }
}

fn print_analysis(
    target: &TargetEnvironment,
    root: &RootImage,
    resolution: &Resolution,
    dependencies: &[InspectedPackage],
    analysis: &Analysis,
) -> Result<(), Box<dyn Error>> {
    println!("package: {} {}", root.name, root.version);
    println!("image: {}", root.root.display());
    println!("origin: {}", root.origin);
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
    print_image_evidence(&root.root, &root.snapshot)?;
    println!();

    println!("runtime dependency closure");
    if dependencies.is_empty()
        && resolution.platform.is_empty()
        && resolution.target_provided.is_empty()
    {
        println!("  none");
    } else {
        for dependency in dependencies {
            println!(
                "  {} {}: internalized installed image",
                dependency.resolved.installed.name, dependency.resolved.installed.version
            );
            println!("    {}", dependency.resolved.root.display());
        }
        for (name, version) in &resolution.platform {
            println!("  {name} {version}: R platform");
        }
        for (name, version) in &resolution.target_provided {
            println!("  {name} {version}: target-provided (explicit)");
        }
    }
    println!();

    let reachable_count = analysis.reachable.iter().filter(|value| **value).count();
    println!("graph");
    println!("  nodes: {}", analysis.graph.nodes.len());
    println!("  edges: {}", analysis.graph.edges.len());
    println!("  roots: {}", analysis.roots.len());
    println!("  reachable: {reachable_count}");
    println!();

    println!("retention");
    if dependencies.is_empty() {
        println!("  no third-party runtime dependencies were internalized");
    } else {
        for dependency in dependencies {
            print_retention(dependency, analysis);
        }
    }
    println!();

    print_blockers(dependencies, analysis);
    println!();
    print_hermetification_plan(root, resolution, dependencies, analysis);
    Ok(())
}

fn print_image_evidence(root: &Path, snapshot: &SemanticSnapshot) -> Result<(), Box<dyn Error>> {
    let bytes = directory_bytes(root)?;
    let supported = snapshot.state.bindings.iter().filter(|binding| binding.supported).count();
    println!("  installed bytes: {bytes}");
    println!("  bindings: {} total, {} materializable", snapshot.state.bindings.len(), supported);
    println!("  exports: {}", snapshot.state.exports.len());
    println!("  resources: {}", snapshot.state.resources.len());
    println!("  S3 registrations: {}", snapshot.state.s3.len());
    println!("  dynamic libraries: {}", snapshot.state.dynlibs.len());
    println!("  .onLoad present: {}", if snapshot.state.has_on_load { "yes" } else { "no" });
    println!("  inspection: direct installed lazy-load image (no loadNamespace() call for this package)");
    Ok(())
}

fn print_retention(package: &InspectedPackage, analysis: &Analysis) {
    let name = &package.resolved.installed.name;
    let all = analysis
        .graph
        .nodes
        .iter()
        .filter_map(|node| match &node.kind {
            NodeKind::Binding { name: binding } if &node.package == name => {
                Some((binding.clone(), analysis.reachable[node.id.0]))
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    let mut keep = all
        .iter()
        .filter(|(_, reachable)| *reachable)
        .map(|(name, _)| name.clone())
        .collect::<Vec<_>>();
    let mut drop = all
        .iter()
        .filter(|(_, reachable)| !*reachable)
        .map(|(name, _)| name.clone())
        .collect::<Vec<_>>();
    keep.sort();
    drop.sort();

    let materializable = keep
        .iter()
        .filter(|binding| {
            package
                .snapshot
                .state
                .bindings
                .iter()
                .find(|state| state.name == binding.as_str())
                .is_some_and(|state| state.supported)
        })
        .count();

    println!(
        "  {name}: {}/{} bindings retained, {}/{} retained bindings materializable",
        keep.len(),
        all.len(),
        materializable,
        keep.len()
    );
    if !keep.is_empty() {
        println!("    keep: {}", keep.join(", "));
    }
    if !drop.is_empty() {
        println!("    drop: {}", drop.join(", "));
    }
}

fn print_blockers(dependencies: &[InspectedPackage], analysis: &Analysis) {
    let mut blockers = Vec::new();
    for diagnostic in &analysis.diagnostics {
        if diagnostic.reachable {
            blockers.push(format!("{:?} [{}]: {}", diagnostic.code, diagnostic.package, diagnostic.message));
        }
    }

    for package in dependencies {
        let name = &package.resolved.installed.name;
        for node in &analysis.graph.nodes {
            let NodeKind::Binding { name: binding } = &node.kind else { continue };
            if &node.package != name || !analysis.reachable[node.id.0] {
                continue;
            }
            let Some(state) = package.snapshot.state.bindings.iter().find(|state| &state.name == binding) else {
                continue;
            };
            if !state.supported {
                let detail = state
                    .issues
                    .iter()
                    .map(|issue| format!("{}: {}", issue.kind, issue.detail))
                    .collect::<Vec<_>>()
                    .join("; ");
                blockers.push(format!("materialization [{name}::{binding}]: {detail}"));
            }
        }
    }

    for package in dependencies {
        let name = &package.resolved.installed.name;
        for resource in reachable_resources(analysis, name) {
            if resource == "*" {
                blockers.push(format!(
                    "resource [{name}]: dynamic system.file() path cannot be selected statically"
                ));
            } else if !package
                .snapshot
                .state
                .resources
                .iter()
                .any(|path| path == &resource)
            {
                blockers.push(format!(
                    "resource [{name}:{resource}]: path does not exist in the selected installed image"
                ));
            }
        }
    }

    println!("blockers");
    if blockers.is_empty() {
        println!("  none");
    } else {
        for blocker in blockers {
            println!("  - {blocker}");
        }
    }
}

fn print_hermetification_plan(
    root: &RootImage,
    resolution: &Resolution,
    dependencies: &[InspectedPackage],
    analysis: &Analysis,
) {
    println!("hermetification plan");
    println!("  root package: {} {}", root.name, root.version);
    println!("  synthetic namespaces: {}", dependencies.len());

    let internalized = dependencies
        .iter()
        .map(|package| package.resolved.installed.name.clone())
        .collect::<BTreeSet<_>>();

    for package in dependencies {
        let name = &package.resolved.installed.name;
        let mut bindings = analysis
            .graph
            .nodes
            .iter()
            .filter_map(|node| match &node.kind {
                NodeKind::Binding { name: binding }
                    if &node.package == name && analysis.reachable[node.id.0] =>
                {
                    Some(binding.clone())
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        bindings.sort();

        let resources = reachable_resources(analysis, name);
        let native = analysis.graph.nodes.iter().any(|node| {
            node.package == name.as_str()
                && analysis.reachable[node.id.0]
                && matches!(&node.kind, NodeKind::NativeComponent { .. })
        });
        let package_refs = reachable_package_refs_to(analysis, name);
        let identity_refs = reachable_identity_refs_to(analysis, name);

        println!("  {name}");
        println!("    namespace env: {name}_ns");
        println!("    imports env: {name}_imports");
        println!("    image: {}", package.resolved.root.display());
        println!("    retain bindings: {}", if bindings.is_empty() { "none".into() } else { bindings.join(", ") });
        println!("    retain resources: {}", if resources.is_empty() { "none".into() } else { resources.join(", ") });
        println!("    retain native shared library whole: {}", if native { "yes" } else { "no" });
        println!("    pkg::/pkg::: rewrites targeting namespace: {package_refs}");
        println!("    package-identity rewrites targeting namespace: {identity_refs}");
    }

    println!("  R platform namespaces: {}", join_map_keys(&resolution.platform));
    println!("  explicit external namespaces: {}", join_map_keys(&resolution.target_provided));
    println!("  internalized namespace references requiring rewrite: {}", count_internalized_package_refs(analysis, &internalized));
    println!("  internalized resource references requiring rewrite: {}", count_internalized_resource_refs(analysis, &internalized));
    println!("  installed images are the canonical linker input; dependency source reconstruction: none");

    let blocked = analysis.diagnostics.iter().any(|diagnostic| diagnostic.reachable)
        || dependencies.iter().any(|package| {
            analysis.graph.nodes.iter().any(|node| {
                let NodeKind::Binding { name: binding } = &node.kind else { return false };
                node.package == package.resolved.installed.name.as_str()
                    && analysis.reachable[node.id.0]
                    && package
                        .snapshot
                        .state
                        .bindings
                        .iter()
                        .find(|state| state.name == binding.as_str())
                        .is_some_and(|state| !state.supported)
            })
        });
    println!("  analysis status: {}", if blocked { "blocked" } else { "link plan complete" });
    println!("  build status: graph-driven materialization and rewriting not wired yet");
}

fn reachable_resources(analysis: &Analysis, package: &str) -> Vec<String> {
    let mut resources = analysis
        .graph
        .nodes
        .iter()
        .filter_map(|node| match &node.kind {
            NodeKind::Resource { path } if node.package == package && analysis.reachable[node.id.0] => Some(path.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    resources.sort();
    resources.dedup();
    resources
}

fn reachable_package_refs_to(analysis: &Analysis, target: &str) -> usize {
    let mut count = 0usize;
    for package in &analysis.packages {
        let mut ordinal = 0usize;
        for unit in &package.r_units {
            for expression in &unit.parsed.expressions {
                let reachable = analysis
                    .graph
                    .initialization(&package.id.name, ordinal)
                    .is_some_and(|node| analysis.reachable[node.0]);
                ordinal += 1;
                if reachable {
                    count += expression.package_refs.iter().filter(|reference| reference.package == target).count();
                }
            }
        }
    }
    count
}

fn reachable_identity_refs_to(analysis: &Analysis, target: &str) -> usize {
    let mut count = 0usize;
    for package in &analysis.packages {
        let mut ordinal = 0usize;
        for unit in &package.r_units {
            for expression in &unit.parsed.expressions {
                let reachable = analysis
                    .graph
                    .initialization(&package.id.name, ordinal)
                    .is_some_and(|node| analysis.reachable[node.0]);
                ordinal += 1;
                if !reachable {
                    continue;
                }
                for call in &expression.calls {
                    if matches!(call.callee.as_str(), "requireNamespace" | "loadNamespace" | "getNamespace" | "asNamespace" | "packageVersion" | "find.package") {
                        if let Some(Some(hrm::analysis::parser::StaticArg::String(name))) = call.args.first() {
                            if name == target {
                                count += 1;
                            }
                        }
                    }
                }
            }
        }
    }
    count
}

fn count_internalized_package_refs(analysis: &Analysis, internalized: &BTreeSet<String>) -> usize {
    internalized
        .iter()
        .map(|package| reachable_package_refs_to(analysis, package))
        .sum()
}

fn count_internalized_resource_refs(analysis: &Analysis, internalized: &BTreeSet<String>) -> usize {
    let mut count = 0usize;
    for package in &analysis.packages {
        let mut ordinal = 0usize;
        for unit in &package.r_units {
            for expression in &unit.parsed.expressions {
                let reachable = analysis
                    .graph
                    .initialization(&package.id.name, ordinal)
                    .is_some_and(|node| analysis.reachable[node.0]);
                ordinal += 1;
                if !reachable {
                    continue;
                }
                count += expression
                    .resource_refs
                    .iter()
                    .filter(|reference| reference.package.as_ref().is_some_and(|name| internalized.contains(name)))
                    .count();
            }
        }
    }
    count
}

fn join_map_keys(map: &BTreeMap<String, String>) -> String {
    if map.is_empty() {
        "none".into()
    } else {
        map.keys().cloned().collect::<Vec<_>>().join(", ")
    }
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

fn read_description(root: &Path) -> Result<Description, Box<dyn Error>> {
    let path = root.join("DESCRIPTION");
    let text = fs::read_to_string(&path)?;
    Ok(Description::parse(&text).map_err(|error| {
        io::Error::new(io::ErrorKind::InvalidData, format!("{}: {error}", path.display()))
    })?)
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
    use super::{AnalyzeArgs, Command, parse_args};
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
            })
        );
    }

    #[test]
    fn target_provided_is_explicit() {
        assert_eq!(
            parse_args(os(&["analyze", "voucher", "--target-provided", "cli,fs"])).unwrap(),
            Command::Analyze(AnalyzeArgs {
                root: "voucher".into(),
                libraries: Vec::new(),
                target_provided: BTreeSet::from(["cli".into(), "fs".into()]),
            })
        );
    }
}
