use std::env;
use std::error::Error;
use std::io;
use std::io::Write;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::process::{Command as ProcessCommand, ExitCode};

use clap::{Args, Parser, Subcommand};
use slinker::analysis::{
    ANALYSIS_STACK_BYTES, Diagnostic, Edge, ExplanationDag, LinkIr, LinkPolicy, Linker, NodeId,
    NodeKind,
};
use slinker::build::{BuildContext, PureRStatic, materialize};
use slinker::cache::CacheLocation;
use slinker::package::PackageStore;
use slinker::source::{SourcePackageSnapshot, stage_root};
use slinker::{TargetEnvironment, TargetEnvironmentRequest};

#[derive(Debug, Parser)]
#[command(
    version,
    about = "Link R package dependencies into a generated source package",
    after_help = "Environment:\n  R_HOME             fallback R installation when `R RHOME` is unavailable\n  SLINKER_CACHE_DIR  persistent installed-image analysis cache"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    #[command(flatten)]
    User(UserCommand),
    #[command(name = "__r-worker", hide = true)]
    RWorker { protocol: PathBuf },
}

#[derive(Debug, Subcommand)]
enum UserCommand {
    #[command(about = "Build a generated linked R source package")]
    Build(BuildArgs),
    #[command(about = "Analyze an installed package image", alias = "analyse")]
    Analyze(AnalyzeArgs),
    #[command(about = "Explain why TARGET is retained by ROOT")]
    Why(QueryArgs),
    #[command(about = "Show semantic paths from ROOT into TARGET")]
    Path(QueryArgs),
}

#[derive(Debug, Args)]
struct UniverseArgs {
    #[arg(
        long = "lib",
        value_name = "PATH",
        help = "Installed R library, searched in the given order (repeatable)"
    )]
    libraries: Vec<PathBuf>,
    #[arg(
        long,
        value_name = "PKG",
        value_delimiter = ',',
        value_parser = package_name,
        help = "Keep declared packages as runtime dependencies"
    )]
    external: Vec<String>,
    #[arg(long, value_name = "N", default_value_t = default_jobs(), help = "Analysis workers")]
    jobs: NonZeroUsize,
    #[arg(
        long,
        value_name = "BOOL",
        default_value_t = true,
        action = clap::ArgAction::Set,
        help = "Block on every unproven assumption; false records recorded heuristics instead"
    )]
    strict: bool,
}

#[derive(Debug, Args)]
struct BuildArgs {
    #[arg(default_value = ".", help = "Source package root")]
    path: PathBuf,
    #[arg(
        long,
        value_name = "PATH",
        help = "Generated source package directory [default: PATH/target/slinker/<Package>]"
    )]
    output: Option<PathBuf>,
    #[command(flatten)]
    universe: UniverseArgs,
}

#[derive(Debug, Args)]
struct AnalysisArgs {
    #[arg(value_name = "ROOT", value_parser = package_name, help = "Installed root package")]
    root: String,
    #[command(flatten)]
    universe: UniverseArgs,
    #[arg(
        long = "extra-pkgs",
        value_name = "PKG",
        value_delimiter = ',',
        value_parser = package_name,
        help = "Enable optional packages when reachable"
    )]
    extra_pkgs: Vec<String>,
}

#[derive(Debug, Args)]
struct AnalyzeArgs {
    #[command(flatten)]
    analysis: AnalysisArgs,
    #[arg(long, help = "Emit deterministic explanation-DAG JSON")]
    graph: bool,
}

#[derive(Debug, Args)]
struct QueryArgs {
    #[command(flatten)]
    analysis: AnalysisArgs,
    #[arg(value_name = "TARGET", help = "PKG, PKG::name, or PKG:::name")]
    target: String,
}

fn main() -> ExitCode {
    match Cli::parse().command {
        Command::RWorker { protocol } => {
            report(slinker::r_worker::run(&protocol).map_err(Into::into))
        }
        Command::User(command) => std::thread::Builder::new()
            .name("slinker".into())
            .stack_size(ANALYSIS_STACK_BYTES)
            .spawn(move || report(run(command)))
            .expect("spawn the slinker command thread")
            .join()
            .unwrap_or(ExitCode::FAILURE),
    }
}

fn report(result: Result<(), Box<dyn Error>>) -> ExitCode {
    match result {
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

fn run(command: UserCommand) -> Result<(), Box<dyn Error>> {
    match command {
        UserCommand::Build(args) => build(&args),
        UserCommand::Analyze(args) => analyze(&args),
        UserCommand::Why(args) => explain_why(&args),
        UserCommand::Path(args) => explain_paths(&args),
    }
}

fn package_name(value: &str) -> Result<String, &'static str> {
    if value.is_empty() || value.contains(['/', '\\']) {
        return Err("expected an R package name");
    }
    Ok(value.to_owned())
}

fn default_jobs() -> NonZeroUsize {
    std::thread::available_parallelism()
        .unwrap_or(NonZeroUsize::MIN)
        .min(NonZeroUsize::new(8).expect("8 is nonzero"))
}

impl UniverseArgs {
    fn policy(&self) -> LinkPolicy {
        LinkPolicy {
            strict: self.strict,
            ..LinkPolicy::default()
        }
    }
}

fn cache_location() -> CacheLocation {
    env::var_os("SLINKER_CACHE_DIR").map_or(CacheLocation::Default, |root| {
        CacheLocation::Directory(PathBuf::from(root))
    })
}

fn absolute_libraries(universe: &UniverseArgs) -> io::Result<Vec<PathBuf>> {
    universe
        .libraries
        .iter()
        .map(|library| absolute_path(library))
        .collect()
}

fn link(args: &AnalysisArgs) -> Result<(TargetEnvironment, LinkIr), Box<dyn Error>> {
    let r_home = discover_r_home()?;
    let mut target_request = TargetEnvironmentRequest::new(r_home.clone());
    target_request.libraries = absolute_libraries(&args.universe)?;
    let target = target_request.capture()?;

    let store = PackageStore::new(r_home, target.clone(), cache_location())?;
    let plan = Linker::new(store, args.universe.jobs.get())
        .with_external_packages(args.universe.external.iter().cloned())
        .with_policy(args.universe.policy())
        .with_extra_packages(args.extra_pkgs.iter().cloned())
        .analyze(&args.root)?;
    Ok((target, plan))
}

fn build(args: &BuildArgs) -> Result<(), Box<dyn Error>> {
    let r_home = discover_r_home()?;
    let source = SourcePackageSnapshot::capture(&args.path)?;
    let libraries = match absolute_libraries(&args.universe)? {
        explicit if explicit.is_empty() => {
            TargetEnvironmentRequest::new(r_home.clone())
                .capture()?
                .libraries
        }
        explicit => explicit,
    };
    let staged = stage_root(&source, &r_home, &libraries)?;
    let mut target_request = TargetEnvironmentRequest::new(r_home.clone());
    target_request.libraries = std::iter::once(staged.library().to_path_buf())
        .chain(libraries)
        .collect();
    let target = target_request.capture()?;
    let store = PackageStore::new(r_home.clone(), target.clone(), cache_location())?;
    let ir = Linker::new(store, args.universe.jobs.get())
        .without_provenance()
        .with_external_packages(args.universe.external.iter().cloned())
        .with_policy(args.universe.policy())
        .with_root_source(source.description_source())
        .analyze(source.package())?;
    let mut context = BuildContext::new(source, staged, r_home, target);
    let output = args.output.clone().unwrap_or_else(|| {
        context
            .source()
            .original_root()
            .join("target")
            .join("slinker")
            .join(context.source().package())
    });
    let buildable = PureRStatic::check(&ir, &mut context)?;
    let generated = materialize(buildable, &output)?;
    for assumption in ir.assumptions() {
        let owner = assumption.binding.as_ref().map_or_else(
            || assumption.package.clone(),
            |binding| format!("{}::{binding}", assumption.package),
        );
        eprintln!(
            "slinker: assumed {:?} in {owner}: {}",
            assumption.code, assumption.message
        );
    }
    println!("{}", generated.path().display());
    Ok(())
}

fn analyze(args: &AnalyzeArgs) -> Result<(), Box<dyn Error>> {
    let (target, plan) = link(&args.analysis)?;
    if args.graph {
        let graph = ExplanationDag::from_plan(&plan, &target, &args.analysis.root)?;
        let mut stdout = io::stdout().lock();
        serde_json::to_writer_pretty(&mut stdout, &graph)?;
        writeln!(stdout)?;
    } else {
        print_analysis(&target, &plan);
    }
    Ok(())
}

fn explain_why(args: &QueryArgs) -> Result<(), Box<dyn Error>> {
    let (_, plan) = link(&args.analysis)?;
    let targets = matching_nodes(&plan, &args.target);
    if targets.is_empty() {
        println!(
            "{} is not in the semantic closure of {}.",
            args.target, args.analysis.root
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
            args.target, args.analysis.root
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
    let (_, plan) = link(&args.analysis)?;
    let entries = package_entry_edges(&plan, &args.target);
    if entries.is_empty() {
        let targets = matching_nodes(&plan, &args.target);
        if targets.is_empty() {
            println!(
                "{} is not in the semantic closure of {}.",
                args.target, args.analysis.root
            );
        } else {
            println!(
                "{} has no cross-package entry edge from {}.",
                args.target, args.analysis.root
            );
        }
        return Ok(());
    }

    println!(
        "semantic paths from {} to {}:",
        args.analysis.root, args.target
    );
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

fn print_analysis(target: &TargetEnvironment, plan: &LinkIr) {
    let program = plan.program();
    let root = program.package(program.root_package()).identity();
    println!("package: {} {}", root.name, root.version);
    println!(
        "target: R {} | {} | {}",
        target.target.r_version, target.target.os, target.target.arch
    );
    println!();
    println!("package roles");
    for (_, package) in program.packages() {
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
    println!();
    println!("provenance");
    println!("  entities: {}", plan.provenance().nodes().len());
    println!("  derivations: {}", plan.provenance().edges().len());
    println!("  roots: {}", plan.provenance().roots().len());
    println!();
    print_diagnostics("blockers", plan.blockers());
    if !plan.assumptions().is_empty() {
        println!();
        print_diagnostics("assumptions", plan.assumptions());
    }
}

fn print_diagnostics(title: &str, diagnostics: &[Diagnostic]) {
    println!("{title}");
    if diagnostics.is_empty() {
        println!("  none");
    }
    for diagnostic in diagnostics {
        println!("  - {:?}: {}", diagnostic.code, diagnostic.message);
    }
}

fn absolute_path(path: &Path) -> io::Result<PathBuf> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(env::current_dir()?.join(path))
    }
}

fn discover_r_home() -> io::Result<PathBuf> {
    if let Ok(r) = which::which("R")
        && let Ok(output) = ProcessCommand::new(r).arg("RHOME").output()
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

#[cfg(test)]
mod tests {
    use super::{Cli, Command, UserCommand, parse_r_home};
    use clap::{CommandFactory, Parser};
    use std::path::Path;

    #[test]
    fn cli_definition_is_consistent() {
        Cli::command().debug_assert();
    }

    #[test]
    fn build_defaults_to_current_directory() {
        let Command::User(UserCommand::Build(args)) = Cli::parse_from(["slinker", "build"]).command
        else {
            panic!("expected build command");
        };
        assert_eq!(args.path, Path::new("."));
        assert_eq!(args.output, None);
    }

    #[test]
    fn analyze_accepts_ordered_libraries_and_package_lists() {
        let Command::User(UserCommand::Analyze(args)) = Cli::parse_from([
            "slinker",
            "analyze",
            "voucher",
            "--lib",
            "one",
            "--lib=two",
            "--external",
            "cli,glue",
            "--extra-pkgs=foo,bar",
            "--jobs",
            "3",
            "--graph",
        ])
        .command
        else {
            panic!("expected analyze command");
        };
        assert_eq!(args.analysis.root, "voucher");
        assert_eq!(
            args.analysis.universe.libraries,
            [Path::new("one"), Path::new("two")]
        );
        assert_eq!(args.analysis.universe.external, ["cli", "glue"]);
        assert_eq!(args.analysis.extra_pkgs, ["foo", "bar"]);
        assert_eq!(args.analysis.universe.jobs.get(), 3);
        assert!(args.graph);
    }

    #[test]
    fn query_takes_root_then_target() {
        let Command::User(UserCommand::Why(args)) =
            Cli::parse_from(["slinker", "why", "voucher", "cli::cli_abort"]).command
        else {
            panic!("expected why command");
        };
        assert_eq!(args.analysis.root, "voucher");
        assert_eq!(args.target, "cli::cli_abort");
    }

    #[test]
    fn rejects_invalid_arguments() {
        for args in [
            &["slinker", "analyze"][..],
            &["slinker", "analyze", "./voucher"],
            &["slinker", "analyze", "voucher", "--jobs=0"],
            &["slinker", "analyze", "voucher", "--external", "cli,,glue"],
            &["slinker", "analyze", "voucher", "--graph-format", "json"],
            &["slinker", "why", "voucher"],
        ] {
            assert!(Cli::try_parse_from(args).is_err(), "{args:?}");
        }
    }

    #[test]
    fn r_home_uses_last_non_warning_line() {
        let stdout =
            "WARNING: ignoring environment value of R_HOME\nC:/Program Files/R/R-4.6.1\n\n";
        assert_eq!(parse_r_home(stdout), Some("C:/Program Files/R/R-4.6.1"));
    }
}
