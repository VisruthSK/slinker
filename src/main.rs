use std::collections::BTreeSet;
use std::env;
use std::error::Error;
use std::io;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command as ProcessCommand, ExitCode};

use slinker::analysis::{Edge, ExplanationDag, LinkIr, Linker, NodeId, NodeKind};
use slinker::build::{BuildContext, PureRStatic, materialize};
use slinker::package::PackageStore;
use slinker::source::{SourcePackageSnapshot, stage_root};
use slinker::{TargetEnvironment, TargetEnvironmentRequest};

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("slinker: {error}");
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
    if env::args_os().nth(1).as_deref() == Some(std::ffi::OsStr::new("__r-worker")) {
        let protocol = env::args_os()
            .nth(2)
            .ok_or("missing R worker protocol path")?;
        return slinker::r_worker::run(std::path::Path::new(&protocol))
            .map_err(|error| Box::new(error) as Box<dyn Error>);
    }
    match parse_args(env::args_os().skip(1))? {
        Command::Help => {
            print_help();
            Ok(())
        }
        Command::Version => {
            println!("slinker {VERSION}");
            Ok(())
        }
        Command::Analyze(args) => analyze(&args),
        Command::Build(args) => build(&args),
        Command::Why(args) => explain_why(&args),
        Command::Path(args) => explain_paths(&args),
    }
}

#[derive(Debug, Eq, PartialEq)]
enum Command {
    Build(BuildArgs),
    Analyze(AnalyzeArgs),
    Why(QueryArgs),
    Path(QueryArgs),
    Help,
    Version,
}

#[derive(Debug, Eq, PartialEq)]
struct BuildArgs {
    input: PathBuf,
    output: Option<PathBuf>,
    libraries: Vec<PathBuf>,
    external: BTreeSet<String>,
    jobs: usize,
}

#[derive(Debug, Eq, PartialEq)]
struct AnalyzeArgs {
    root: String,
    libraries: Vec<PathBuf>,
    external: BTreeSet<String>,
    extra_pkgs: BTreeSet<String>,
    jobs: usize,
    graph: bool,
}

#[derive(Debug, Eq, PartialEq)]
struct QueryArgs {
    root: String,
    target: String,
    libraries: Vec<PathBuf>,
    external: BTreeSet<String>,
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
        Some("build") => parse_build_args(args).map(Command::Build),
        Some("why") => parse_query_args(args).map(Command::Why),
        Some("path") => parse_query_args(args).map(Command::Path),
        _ => Err(CliError::Usage(
            "expected `build`, `analyze`, `why`, or `path`",
        )),
    }
}

fn parse_build_args(args: impl Iterator<Item = std::ffi::OsString>) -> Result<BuildArgs, CliError> {
    let mut args = args.peekable();
    let mut input = None;
    let mut output = None;
    let mut libraries = Vec::new();
    let mut external = BTreeSet::new();
    let mut jobs = default_jobs();
    while let Some(argument) = args.next() {
        let text = argument.to_string_lossy();
        if text == "--lib" {
            libraries.push(PathBuf::from(
                args.next()
                    .ok_or(CliError::Usage("`--lib` requires a library path"))?,
            ));
            continue;
        }
        if let Some(value) = text.strip_prefix("--lib=") {
            if value.is_empty() {
                return Err(CliError::Usage("`--lib` requires a library path"));
            }
            libraries.push(value.into());
            continue;
        }
        if text == "--external" {
            let value = args
                .next()
                .ok_or(CliError::Usage("`--external` requires a package list"))?;
            insert_package_list(&mut external, &value)?;
            continue;
        }
        if let Some(value) = text.strip_prefix("--external=") {
            insert_package_list_str(&mut external, value)?;
            continue;
        }
        if text == "--output" {
            output = Some(PathBuf::from(
                args.next()
                    .ok_or(CliError::Usage("`--output` requires a path"))?,
            ));
            continue;
        }
        if let Some(value) = text.strip_prefix("--output=") {
            if value.is_empty() {
                return Err(CliError::Usage("`--output` requires a path"));
            }
            output = Some(value.into());
            continue;
        }
        if text == "--jobs" {
            jobs = parse_jobs(
                &args
                    .next()
                    .ok_or(CliError::Usage("`--jobs` requires a positive integer"))?,
            )?;
            continue;
        }
        if let Some(value) = text.strip_prefix("--jobs=") {
            jobs = parse_jobs_str(value)?;
            continue;
        }
        if text.starts_with('-') {
            return Err(CliError::Usage("unknown build option"));
        }
        if input.replace(PathBuf::from(argument)).is_some() {
            return Err(CliError::Usage(
                "build accepts at most one source package path",
            ));
        }
    }
    Ok(BuildArgs {
        input: input.unwrap_or_else(|| PathBuf::from(".")),
        output,
        libraries,
        external,
        jobs,
    })
}

fn parse_analyze_args(
    args: impl Iterator<Item = std::ffi::OsString>,
) -> Result<AnalyzeArgs, CliError> {
    let mut args = args.peekable();
    let mut root = None;
    let mut libraries = Vec::new();
    let mut external = BTreeSet::new();
    let mut extra_pkgs = BTreeSet::new();
    let mut jobs = default_jobs();
    let mut graph = false;
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
        if text == "--external" {
            let value = args
                .next()
                .ok_or(CliError::Usage("`--external` requires a package list"))?;
            insert_package_list(&mut external, &value)?;
            continue;
        }
        if let Some(value) = text.strip_prefix("--external=") {
            insert_package_list_str(&mut external, value)?;
            continue;
        }
        if text == "--extra-pkgs" {
            collect_package_args(
                &mut extra_pkgs,
                &mut args,
                "`--extra-pkgs` requires at least one package",
            )?;
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
        if text == "--graph" {
            graph = true;
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
        external,
        extra_pkgs,
        jobs,
        graph,
    })
}

fn parse_query_args(args: impl Iterator<Item = std::ffi::OsString>) -> Result<QueryArgs, CliError> {
    let mut args = args.peekable();
    let mut positional = Vec::new();
    let mut libraries = Vec::new();
    let mut external = BTreeSet::new();
    let mut extra_pkgs = BTreeSet::new();
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
        if text == "--external" {
            let value = args
                .next()
                .ok_or(CliError::Usage("`--external` requires a package list"))?;
            insert_package_list(&mut external, &value)?;
            continue;
        }
        if let Some(value) = text.strip_prefix("--external=") {
            insert_package_list_str(&mut external, value)?;
            continue;
        }
        if text == "--extra-pkgs" {
            collect_package_args(
                &mut extra_pkgs,
                &mut args,
                "`--extra-pkgs` requires at least one package",
            )?;
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
            return Err(CliError::Usage("unknown query option"));
        }
        positional.push(
            argument
                .into_string()
                .map_err(|_| CliError::Usage("package/binding names must be valid UTF-8"))?,
        );
    }
    if positional.len() != 2 {
        return Err(CliError::Usage("why/path require ROOT and TARGET"));
    }
    let root = positional.remove(0);
    if root.contains('/') || root.contains('\\') {
        return Err(CliError::Usage("ROOT must be an installed package name"));
    }
    Ok(QueryArgs {
        root,
        target: positional.remove(0),
        libraries,
        external,
        extra_pkgs,
        jobs,
    })
}

fn link(args: &AnalyzeArgs) -> Result<(TargetEnvironment, LinkIr), Box<dyn Error>> {
    let r_home = discover_r_home()?;
    let mut target_request = TargetEnvironmentRequest::new(r_home.clone());
    target_request.libraries = args
        .libraries
        .iter()
        .map(|library| absolute_path(library))
        .collect::<io::Result<Vec<_>>>()?;
    let target = target_request.capture()?;

    let store = PackageStore::new(r_home, target.clone())?;
    let plan = Linker::new(store, args.jobs)
        .with_external_packages(args.external.iter().cloned())
        .with_extra_packages(args.extra_pkgs.iter().cloned())
        .analyze(&args.root)?;
    Ok((target, plan))
}

fn build(args: &BuildArgs) -> Result<(), Box<dyn Error>> {
    let r_home = discover_r_home()?;
    let source = SourcePackageSnapshot::capture(&args.input)?;
    let libraries = args
        .libraries
        .iter()
        .map(|library| absolute_path(library))
        .collect::<io::Result<Vec<_>>>()?;
    let staged = stage_root(&source, &r_home, &libraries)?;
    let mut target_request = TargetEnvironmentRequest::new(r_home.clone());
    target_request.libraries = std::iter::once(staged.library().to_path_buf())
        .chain(libraries)
        .collect();
    let target = target_request.capture()?;
    let store = PackageStore::new(r_home.clone(), target.clone())?;
    let ir = Linker::new(store, args.jobs)
        .with_external_packages(args.external.iter().cloned())
        .with_root_source(source.description_source())
        .analyze(source.package())?;
    let context = BuildContext::new(source, staged, r_home, target, ir.program())?;
    let output = args.output.clone().unwrap_or_else(|| {
        context
            .source()
            .original_root()
            .join("target/slinker")
            .join(context.source().package())
    });
    let buildable = PureRStatic::check(&ir, &context)?;
    let generated = materialize(buildable, &output)?;
    println!("{}", generated.path().display());
    Ok(())
}

fn analyze(args: &AnalyzeArgs) -> Result<(), Box<dyn Error>> {
    let (target, plan) = link(args)?;
    if args.graph {
        let graph = ExplanationDag::from_plan(&plan, &target, &args.root)?;
        let stdout = io::stdout();
        let mut stdout = stdout.lock();
        serde_json::to_writer_pretty(&mut stdout, &graph)?;
        writeln!(stdout)?;
    } else {
        print_analysis(&target, &args.root, &plan)?;
    }
    Ok(())
}

fn query_link_args(args: &QueryArgs) -> AnalyzeArgs {
    AnalyzeArgs {
        root: args.root.clone(),
        libraries: args.libraries.clone(),
        external: args.external.clone(),
        extra_pkgs: args.extra_pkgs.clone(),
        jobs: args.jobs,
        graph: false,
    }
}

fn explain_why(args: &QueryArgs) -> Result<(), Box<dyn Error>> {
    let (_, plan) = link(&query_link_args(args))?;
    let targets = matching_nodes(&plan, &args.target);
    if targets.is_empty() {
        println!(
            "{} is not in the semantic closure of {}.",
            args.target, args.root
        );
        return Ok(());
    }

    let best = targets
        .iter()
        .filter_map(|target| {
            plan.provenance()
                .shortest_path(plan.provenance().roots(), *target)
                .map(|path| (*target, path))
        })
        .min_by_key(|(_, path)| path.len());
    let Some((target, path)) = best else {
        println!(
            "{} exists in the graph but is not reachable from {}.",
            args.target, args.root
        );
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
            println!(
                "{} is not in the semantic closure of {}.",
                args.target, args.root
            );
        } else {
            println!(
                "{} has no cross-package entry edge from {}.",
                args.target, args.root
            );
        }
        return Ok(());
    }

    println!("semantic paths from {} to {}:", args.root, args.target);
    for (ordinal, entry) in entries.iter().enumerate() {
        println!();
        println!("path {}:", ordinal + 1);
        if let Some(mut prefix) = plan
            .provenance()
            .shortest_path(plan.provenance().roots(), entry.from)
        {
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

fn matching_nodes(plan: &LinkIr, target: &str) -> Vec<NodeId> {
    let (package, binding) = if let Some((package, binding)) = target.split_once(":::") {
        (package, Some(binding))
    } else if let Some((package, binding)) = target.split_once("::") {
        (package, Some(binding))
    } else {
        (target, None)
    };
    plan.provenance()
        .nodes()
        .iter()
        .filter(|node| node.package == package)
        .map(|node| node.id)
        .filter(
            |id| match (binding, &plan.provenance().nodes()[id.0].kind) {
                (None, _) => true,
                (
                    Some(name),
                    NodeKind::Binding { name: binding }
                    | NodeKind::ExternalBinding { name: binding },
                ) => binding == name,
                _ => false,
            },
        )
        .collect()
}

fn package_entry_edges<'a>(plan: &'a LinkIr, target: &str) -> Vec<&'a Edge> {
    let package = target
        .split_once(":::")
        .map(|x| x.0)
        .or_else(|| target.split_once("::").map(|x| x.0))
        .unwrap_or(target);
    let binding = target
        .split_once(":::")
        .map(|x| x.1)
        .or_else(|| target.split_once("::").map(|x| x.1));
    let mut edges = plan
        .provenance()
        .edges()
        .iter()
        .filter(|edge| {
            let to = &plan.provenance().nodes()[edge.to.0];
            let from = &plan.provenance().nodes()[edge.from.0];
            if to.package != package || from.package == package {
                return false;
            }
            match (binding, &to.kind) {
                (None, _) => true,
                (
                    Some(name),
                    NodeKind::Binding { name: actual } | NodeKind::ExternalBinding { name: actual },
                ) => actual == name,
                _ => false,
            }
        })
        .collect::<Vec<_>>();
    edges.sort_by_key(|edge| (edge.from.0, edge.to.0));
    edges
}

fn print_edge_path(plan: &LinkIr, path: &[&Edge]) {
    if path.is_empty() {
        println!("  explicit root");
        return;
    }
    println!("  {}", node_label(plan, path[0].from));
    for edge in path {
        println!(
            "    -- {:?}{}: {}",
            edge.kind,
            edge_location(plan, edge),
            edge.reason
        );
        println!("    -> {}", node_label(plan, edge.to));
    }
}

fn edge_location(plan: &LinkIr, edge: &Edge) -> String {
    let Some(span) = &edge.span else {
        return String::new();
    };
    let source = plan
        .sources()
        .display(&span.source)
        .unwrap_or_else(|| format!("source#{}", span.source.0));
    format!(" @ {source}:{}..{}", span.start, span.end)
}

fn node_label(plan: &LinkIr, id: NodeId) -> String {
    let node = &plan.provenance().nodes()[id.0];
    match &node.kind {
        NodeKind::Binding { name } | NodeKind::ExternalBinding { name } => {
            format!("{}::{name}", node.package)
        }
        NodeKind::PrivateBinding { environment, name } => {
            format!("{} [private {environment}::{name}]", node.package)
        }
        NodeKind::ClosureObject {
            owner,
            path,
            enclosure,
            derived,
        } => {
            let origin = if *derived { "derived" } else { "installed" };
            format!(
                "{} [{owner}{path} {origin} closure in {enclosure}]",
                node.package
            )
        }
        NodeKind::Activation => format!("{} [activation]", node.package),
        NodeKind::Dataset { name } => format!("{} [dataset {name}]", node.package),
        NodeKind::Lifecycle { hook } => format!("{}::{hook} [lifecycle]", node.package),
        NodeKind::S3Registration { generic, class } => {
            format!("{} [S3 {generic}/{class}]", node.package)
        }
        NodeKind::Resource { path } => format!("{} [resource {path}]", node.package),
        NodeKind::NativeComponent { name } => format!("{} [native {name}]", node.package),
        NodeKind::PackageMetadata { name } => format!("{} [metadata {name}]", node.package),
        NodeKind::MissingPackage => format!("{} [missing package]", node.package),
        NodeKind::Rejection { code } => format!("{} [rejection {code}]", node.package),
    }
}

fn print_analysis(
    target: &TargetEnvironment,
    _root_name: &str,
    plan: &LinkIr,
) -> Result<(), Box<dyn Error>> {
    let program = plan.program();
    let root = program.package(program.root_package()).identity();
    println!("package: {} {}", root.name, root.version);
    println!(
        "target: R {} | {} | {}",
        target.target.r_version, target.target.os, target.target.arch
    );
    println!();
    println!("package roles");
    for package in program.packages() {
        println!(
            "  {} {}: {:?}",
            package.identity().name,
            package.identity().version,
            package.role()
        );
    }
    println!();
    println!("linked program");
    println!("  namespaces: {}", program.namespaces().len());
    println!("  bindings: {}", program.bindings().len());
    println!("  closures: {}", program.closures().len());
    println!("  environments: {}", program.environments().len());
    println!("  relocations: {}", program.relocations().len());
    println!("  residual capabilities: {}", program.residuals().len());
    println!();
    println!("provenance");
    println!("  entities: {}", plan.provenance().nodes().len());
    println!("  derivations: {}", plan.provenance().edges().len());
    println!("  roots: {}", plan.provenance().roots().len());
    println!();
    println!("blockers");
    if plan.provenance().diagnostics().is_empty() {
        println!("  none");
    } else {
        for diagnostic in plan.provenance().diagnostics() {
            println!("  - {:?}: {}", diagnostic.code, diagnostic.message);
        }
    }
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
        "External package names must be valid UTF-8",
    ))?;
    insert_package_list_str(packages, value)
}

fn insert_package_list_str(packages: &mut BTreeSet<String>, value: &str) -> Result<(), CliError> {
    for package in value.split(',').map(str::trim) {
        if package.is_empty() {
            return Err(CliError::Usage(
                "External package list contains an empty name",
            ));
        }
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
        let package = value
            .into_string()
            .map_err(|_| CliError::Usage("package name must be valid UTF-8"))?;
        if package.is_empty()
            || package.contains(',')
            || package.contains('/')
            || package.contains('\\')
        {
            return Err(CliError::Usage(
                "extra packages must be space-separated package names",
            ));
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
    if args.next().is_some() {
        Err(CliError::Usage("too many arguments"))
    } else {
        Ok(())
    }
}

fn print_help() {
    println!(
        "slinker {VERSION}\n\n\
         Usage:\n  slinker build [PATH] [--lib PATH]... [--external PKG[,PKG...]] [--output PATH] [--jobs N]\n  slinker analyze PACKAGE [--lib PATH]... [--external PKG[,PKG...]] [--extra-pkgs PKG...] [--jobs N] [--graph]\n  slinker why ROOT TARGET [same options]\n  slinker path ROOT DOWNSTREAM [same options]\n\n\
         Build a generated linked R source package. PATH defaults to the current directory.\n  `analyze` inspects an installed image; `why` and `path` query provenance.\n\n\
         Options:\n\
           --lib PATH                     select an installed R library (repeatable, ordered)\n\
           --external PKG[,PKG...] preserve declared packages as runtime dependencies\n\
           --output PATH                   generated source-package directory\n\
           --extra-pkgs PKG...             enable named optional packages when reachable\n\
           --jobs N                       analysis workers (default: min(CPUs, 8))\n\
           --graph                        emit deterministic explanation-DAG JSON\n\
\n\
         Environment:\n\
            R_HOME             fallback R installation when `R RHOME` is unavailable\n\
           SLINKER_CACHE_DIR  persistent installed-image analysis cache"
    );
}

fn absolute_path(path: &Path) -> io::Result<PathBuf> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(env::current_dir()?.join(path))
    }
}

fn discover_r_home() -> io::Result<PathBuf> {
    if let Ok(output) = ProcessCommand::new("R").arg("RHOME").output()
        && output.status.success()
        && let Some(home) = parse_r_home(&String::from_utf8_lossy(&output.stdout))
    {
        return dunce::canonicalize(home);
    }
    if let Some(home) = env::var_os("R_HOME")
        && !home.is_empty()
    {
        return dunce::canonicalize(home);
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        "could not select R: `R RHOME` failed and R_HOME is unset",
    ))
}

fn parse_r_home(stdout: &str) -> Option<&str> {
    stdout
        .lines()
        .rev()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with("WARNING:"))
}

#[derive(Debug)]
enum CliError {
    Usage(&'static str),
}
impl std::fmt::Display for CliError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Usage(message) => write!(f, "{message}; run `slinker --help`"),
        }
    }
}
impl Error for CliError {}

#[cfg(test)]
mod tests {
    use super::{AnalyzeArgs, BuildArgs, Command, default_jobs, parse_args, parse_r_home};
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
                external: BTreeSet::new(),
                extra_pkgs: BTreeSet::new(),
                jobs: default_jobs(),
                graph: false,
            })
        );
    }

    #[test]
    fn accepts_extra_packages_as_space_separated_values() {
        let mut extra_pkgs = BTreeSet::new();
        extra_pkgs.extend(["foo".to_owned(), "bar".to_owned(), "baz".to_owned()]);
        assert_eq!(
            parse_args(os(&[
                "analyze",
                "voucher",
                "--extra-pkgs",
                "foo",
                "bar",
                "baz",
                "--jobs",
                "3"
            ]))
            .unwrap(),
            Command::Analyze(AnalyzeArgs {
                root: "voucher".into(),
                libraries: Vec::new(),
                external: BTreeSet::new(),
                extra_pkgs,
                jobs: 3,
                graph: false,
            })
        );
        assert!(parse_args(os(&["analyze", "voucher", "--extra-pkgs", "--jobs", "3"])).is_err());
        assert!(parse_args(os(&["analyze", "voucher", "--extra-pkgs", "foo,bar"])).is_err());
    }

    #[test]
    fn accepts_explicit_jobs() {
        assert_eq!(
            parse_args(os(&["analyze", "voucher", "--jobs", "6"])).unwrap(),
            Command::Analyze(AnalyzeArgs {
                root: "voucher".into(),
                libraries: Vec::new(),
                external: BTreeSet::new(),
                extra_pkgs: BTreeSet::new(),
                jobs: 6,
                graph: false,
            })
        );
        assert!(parse_args(os(&["analyze", "voucher", "--jobs=0"])).is_err());
    }

    #[test]
    fn removed_debug_dump_options_are_rejected() {
        assert!(parse_args(os(&["analyze", "voucher", "--dump-objects=objects.txt"])).is_err());
        assert!(parse_args(os(&["analyze", "voucher", "--dump-graph", "graph.txt"])).is_err());
    }

    #[test]
    fn build_defaults_to_current_directory() {
        assert_eq!(
            parse_args(os(&["build"])).unwrap(),
            Command::Build(BuildArgs {
                input: PathBuf::from("."),
                output: None,
                libraries: Vec::new(),
                external: BTreeSet::new(),
                jobs: default_jobs(),
            })
        );
    }

    #[test]
    fn accepts_graph_output_options() {
        let Command::Analyze(args) = parse_args(os(&["analyze", "voucher", "--graph"])).unwrap()
        else {
            panic!("expected analyze command");
        };
        assert!(args.graph);
    }

    #[test]
    fn rejects_removed_graph_format() {
        assert!(parse_args(os(&["analyze", "voucher", "--graph-format", "json"])).is_err());
    }

    #[test]
    fn r_home_uses_last_non_warning_line() {
        let stdout =
            "WARNING: ignoring environment value of R_HOME\nC:/Program Files/R/R-4.6.1\n\n";
        assert_eq!(parse_r_home(stdout), Some("C:/Program Files/R/R-4.6.1"));
    }
}
