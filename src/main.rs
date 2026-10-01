use std::env;
use std::error::Error;
use std::io;
use std::io::Write;
use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::process::{Command as ProcessCommand, ExitCode};

use clap::{Args, Parser, Subcommand};
use serde_json::json;
use slinker::TargetEnvironment;
use slinker::analysis::{ANALYSIS_STACK_BYTES, Edge, ExplanationDag, LinkIr, NodeId, NodeKind};
use slinker::build::{BuildReport, PreflightError, PureRStatic, materialize};

mod session;

use session::{RootSpec, Session, SourceSession};

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
    #[command(about = "Run the build pipeline through preflight and write nothing")]
    Check(CheckArgs),
    #[command(about = "Analyze a package", alias = "analyse")]
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
    #[arg(
        long = "extra-pkgs",
        value_name = "PKG",
        value_delimiter = ',',
        value_parser = package_name,
        help = "Enable optional packages when reachable"
    )]
    extra_pkgs: Vec<String>,
    #[arg(long, value_name = "N", default_value_t = default_jobs(), help = "Analysis workers")]
    jobs: NonZeroUsize,
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
    #[arg(long, help = "Report the outcome as JSON on stdout")]
    json: bool,
    #[command(flatten)]
    universe: UniverseArgs,
}

#[derive(Debug, Args)]
struct CheckArgs {
    #[arg(default_value = ".", help = "Source package root")]
    path: PathBuf,
    #[arg(long, help = "Report the outcome as JSON on stdout")]
    json: bool,
    #[command(flatten)]
    universe: UniverseArgs,
}

#[derive(Debug, Args)]
struct AnalysisArgs {
    #[arg(
        value_name = "ROOT",
        value_parser = RootSpec::parse,
        help = "Source package path, installed package name, or installed package directory"
    )]
    root: RootSpec,
    #[command(flatten)]
    universe: UniverseArgs,
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
        UserCommand::Check(args) => check(&args),
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

#[derive(Debug, thiserror::Error)]
#[error("build blocked by {0} blocker(s); the JSON report is on stdout")]
struct BlockedJson(usize);

fn link(args: &AnalysisArgs) -> Result<(Session, LinkIr), Box<dyn Error>> {
    let session = Session::open(&args.root, &args.universe, discover_r_home()?)?;
    let plan = session.analyze(&args.universe, true)?;
    Ok((session, plan))
}

fn preflight_failure(error: PreflightError, json: bool) -> Box<dyn Error> {
    match error {
        PreflightError::Blocked(report) if json => {
            let blockers = report.blockers().count();
            let rendered = json!({ "status": "blocked", "groups": report.groups() });
            println!("{rendered:#}");
            Box::new(BlockedJson(blockers))
        }
        other => Box::new(other),
    }
}

fn build(args: &BuildArgs) -> Result<(), Box<dyn Error>> {
    let session = SourceSession::open(&args.path, &args.universe, discover_r_home()?)?;
    let ir = session.session().analyze(&args.universe, false)?;
    let package = session.snapshot().package().to_owned();
    let output = args.output.clone().unwrap_or_else(|| {
        session
            .snapshot()
            .original_root()
            .join("target")
            .join("slinker")
            .join(&package)
    });
    let context = session.into_build_context();
    let buildable =
        PureRStatic::check(&ir, &context).map_err(|error| preflight_failure(error, args.json))?;
    let generated = materialize(buildable, &output)?;
    if args.json {
        let rendered = json!({
            "status": "built",
            "package": package,
            "output": generated.path(),
        });
        println!("{rendered:#}");
    } else {
        println!("{}", generated.path().display());
    }
    Ok(())
}

fn check(args: &CheckArgs) -> Result<(), Box<dyn Error>> {
    let session = SourceSession::open(&args.path, &args.universe, discover_r_home()?)?;
    let ir = session.session().analyze(&args.universe, false)?;
    let package = session.snapshot().package().to_owned();
    let version = session.snapshot().version().to_string();
    let context = session.into_build_context();
    PureRStatic::check(&ir, &context).map_err(|error| preflight_failure(error, args.json))?;
    if args.json {
        let rendered = json!({ "status": "ok", "package": package, "version": version });
        println!("{rendered:#}");
    } else {
        println!("{package} {version} passes preflight; nothing was written");
    }
    Ok(())
}

fn analyze(args: &AnalyzeArgs) -> Result<(), Box<dyn Error>> {
    let (session, plan) = link(&args.analysis)?;
    let target = session.target();
    if args.graph {
        let graph = ExplanationDag::from_plan(&plan, target, session.root())?;
        let mut stdout = io::stdout().lock();
        serde_json::to_writer_pretty(&mut stdout, &graph)?;
        writeln!(stdout)?;
    } else {
        print_analysis(target, &plan);
    }
    Ok(())
}

fn explain_why(args: &QueryArgs) -> Result<(), Box<dyn Error>> {
    let (session, plan) = link(&args.analysis)?;
    let targets = matching_nodes(&plan, &args.target);
    if targets.is_empty() {
        println!(
            "{} is not in the semantic closure of {}.",
            args.target,
            session.root()
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
            args.target,
            session.root()
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
    let (session, plan) = link(&args.analysis)?;
    let entries = package_entry_edges(&plan, &args.target);
    if entries.is_empty() {
        let targets = matching_nodes(&plan, &args.target);
        if targets.is_empty() {
            println!(
                "{} is not in the semantic closure of {}.",
                args.target,
                session.root()
            );
        } else {
            println!(
                "{} has no cross-package entry edge from {}.",
                args.target,
                session.root()
            );
        }
        return Ok(());
    }

    println!("semantic paths from {} to {}:", session.root(), args.target);
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
    let source = plan.sources().display(&span.source);
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
    let report = BuildReport::from_analysis(plan);
    println!("blockers");
    if report.is_empty() {
        println!("  none");
    } else {
        println!("{report}");
    }
}

fn discover_r_home() -> io::Result<PathBuf> {
    let failure = match r_home_from_command() {
        Ok(home) => return dunce::canonicalize(home),
        Err(failure) => failure,
    };
    if let Some(home) = env::var_os("R_HOME")
        && !home.is_empty()
    {
        return dunce::canonicalize(home);
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        format!("could not select R: `R RHOME` failed ({failure}) and R_HOME is unset"),
    ))
}

fn r_home_from_command() -> Result<String, String> {
    let r = which::which("R").map_err(|error| format!("`R` not found on PATH: {error}"))?;
    let output = ProcessCommand::new(&r)
        .arg("RHOME")
        .output()
        .map_err(|error| format!("could not start {}: {error}", r.display()))?;
    if !output.status.success() {
        return Err(format!(
            "{} RHOME exited with {}: {}",
            r.display(),
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    parse_r_home(&String::from_utf8_lossy(&output.stdout))
        .map(str::to_owned)
        .ok_or_else(|| format!("{} RHOME printed no path", r.display()))
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
    use super::{Cli, Command, RootSpec, UserCommand, parse_r_home};
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
        assert_eq!(args.analysis.root, RootSpec::Installed("voucher".into()));
        assert_eq!(
            args.analysis.universe.libraries,
            [Path::new("one"), Path::new("two")]
        );
        assert_eq!(args.analysis.universe.external, ["cli", "glue"]);
        assert_eq!(args.analysis.universe.extra_pkgs, ["foo", "bar"]);
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
        assert_eq!(args.analysis.root, RootSpec::Installed("voucher".into()));
        assert_eq!(args.target, "cli::cli_abort");
    }

    #[test]
    fn root_is_a_package_name_or_a_path() {
        assert_eq!(
            RootSpec::parse("voucher"),
            Ok(RootSpec::Installed("voucher".into()))
        );
        for path in ["./voucher", "..", ".", "C:\\src\\voucher"] {
            assert_eq!(
                RootSpec::parse(path),
                Ok(RootSpec::Source(path.into())),
                "{path}"
            );
        }
        assert!(RootSpec::parse("").is_err());
    }

    #[test]
    fn check_takes_a_path_defaulting_to_the_current_directory() {
        let Command::User(UserCommand::Check(args)) =
            Cli::parse_from(["slinker", "check", "--json"]).command
        else {
            panic!("expected check command");
        };
        assert_eq!(args.path, Path::new("."));
        assert!(args.json);
    }

    #[test]
    fn rejects_invalid_arguments() {
        for args in [
            &["slinker", "analyze"][..],
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
