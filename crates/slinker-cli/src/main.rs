use std::env;
use std::error::Error;
use std::io;
use std::io::Write;
use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::process::{Command as ProcessCommand, ExitCode};

use clap::builder::styling::{AnsiColor, Effects, Styles};
use clap::{Args, Parser, Subcommand};
use serde_json::json;
use slinker_core::TargetEnvironment;
use slinker_core::analysis::{
    ANALYSIS_STACK_BYTES, Edge, ExplanationDag, LinkIr, Node, NodeId, NodeKind,
};
use slinker_core::build::BuildReport;
use slinker_core::package::{BindingName, PackageName};

#[cfg(feature = "profile")]
#[global_allocator]
static ALLOCATOR: slinker_core::profile::heap::CountingAllocator =
    slinker_core::profile::heap::CountingAllocator;

mod cache_command;
mod roles;
use slinker_core::session::{RootSpec, Session, SessionOptions, SourceSession};

const STYLES: Styles = Styles::styled()
    .header(AnsiColor::Yellow.on_default().effects(Effects::BOLD))
    .usage(AnsiColor::Yellow.on_default().effects(Effects::BOLD))
    .literal(AnsiColor::Green.on_default().effects(Effects::BOLD))
    .placeholder(AnsiColor::Cyan.on_default())
    .error(AnsiColor::Red.on_default().effects(Effects::BOLD))
    .valid(AnsiColor::Green.on_default())
    .invalid(AnsiColor::Yellow.on_default());

const DEFAULT_THREADS: NonZeroUsize = NonZeroUsize::new(4).unwrap();

#[derive(Debug, Parser)]
#[command(
    name = "slinker",
    styles = STYLES,
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
    #[command(about = "Inspect and manage the persistent analysis cache")]
    Cache(cache_command::CacheArgs),
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
        help = "Keep packages as runtime dependencies; selects declared optional packages"
    )]
    external: Vec<PackageName>,
    #[arg(
        long = "link",
        value_name = "PKG",
        value_delimiter = ',',
        value_parser = package_name,
        help = "Select declared optional packages and link them in when reachable code uses them"
    )]
    linked: Vec<PackageName>,
    #[arg(long, value_name = "N", default_value_t = DEFAULT_THREADS, help = "Analysis threads")]
    threads: NonZeroUsize,
}

impl UniverseArgs {
    fn options(&self) -> SessionOptions {
        SessionOptions {
            native_summaries: std::env::var_os("SLINKER_NATIVE_SUMMARIES").map(PathBuf::from),
            libraries: self.libraries.clone(),
            external: self.external.clone(),
            linked: self.linked.clone(),
            threads: self.threads,
            cache: cache_location(),
            worker_executable: slinker_core::WorkerExecutable::CurrentProcess,
        }
    }
}

fn cache_location() -> slinker_core::cache::CacheLocation {
    use slinker_core::cache::CacheLocation;
    std::env::var_os("SLINKER_CACHE_DIR").map_or(CacheLocation::Default, |root| {
        CacheLocation::Directory(PathBuf::from(root))
    })
}

#[derive(Debug, Args)]
struct JsonArg {
    #[arg(long, help = "Print one JSON document on stdout, including on failure")]
    json: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OutputFormat {
    Text,
    Json,
}

impl JsonArg {
    fn format(&self) -> OutputFormat {
        if self.json {
            OutputFormat::Json
        } else {
            OutputFormat::Text
        }
    }
}

#[derive(Debug, Args)]
struct BuildArgs {
    #[arg(default_value = ".", help = "Source package root")]
    path: PathBuf,
    #[arg(
        long,
        value_name = "PATH",
        help = "Generated source package directory [default: <Package>-slinked beside the source]"
    )]
    output: Option<PathBuf>,
    #[command(flatten)]
    universe: UniverseArgs,
    #[command(flatten)]
    json: JsonArg,
}

#[derive(Debug, Args)]
struct CheckArgs {
    #[arg(default_value = ".", help = "Source package root")]
    path: PathBuf,
    #[command(flatten)]
    universe: UniverseArgs,
    #[command(flatten)]
    json: JsonArg,
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
    #[command(flatten)]
    json: JsonArg,
}

#[derive(Debug, Args)]
struct QueryArgs {
    #[command(flatten)]
    analysis: AnalysisArgs,
    #[arg(
        value_name = "TARGET",
        value_parser = QueryTarget::parse,
        help = "PKG, PKG::name, or PKG:::name"
    )]
    target: QueryTarget,
}

#[derive(Clone, Debug)]
struct QueryTarget {
    text: String,
    package: PackageName,
    binding: Option<BindingName>,
}

impl QueryTarget {
    fn parse(text: &str) -> Result<Self, &'static str> {
        let (package, binding) = match text.split_once(":::").or_else(|| text.split_once("::")) {
            Some((package, binding)) => (package, Some(BindingName::from(binding))),
            None => (text, None),
        };
        if package.is_empty() {
            return Err("expected PKG, PKG::name, or PKG:::name");
        }
        Ok(Self {
            text: text.to_owned(),
            package: PackageName::from(package),
            binding,
        })
    }

    fn selects(&self, node: &Node) -> bool {
        node.package == self.package
            && self.binding.as_ref().is_none_or(|wanted| match &node.kind {
                NodeKind::Binding { name } | NodeKind::ExternalBinding { name } => name == wanted,
                _ => false,
            })
    }
}

impl std::fmt::Display for QueryTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.text)
    }
}

impl UserCommand {
    fn format(&self) -> OutputFormat {
        match self {
            Self::Build(args) => args.json.format(),
            Self::Check(args) => args.json.format(),
            Self::Analyze(args) => args.json.format(),
            Self::Cache(args) if args.wants_json() => OutputFormat::Json,
            Self::Why(_) | Self::Path(_) | Self::Cache(_) => OutputFormat::Text,
        }
    }
}

fn main() -> ExitCode {
    #[cfg(feature = "profile")]
    if slinker_core::profile::heap::sampling_requested() {
        slinker_core::profile::heap::enable_site_sampling();
    }
    match Cli::parse().command {
        Command::RWorker { protocol } => report(
            slinker_r_worker::run(&protocol).map_err(Into::into),
            OutputFormat::Text,
        ),
        Command::User(command) => std::thread::Builder::new()
            .name("slinker".into())
            .stack_size(ANALYSIS_STACK_BYTES)
            .spawn(move || {
                let format = command.format();
                let code = report(run(command), format);
                if let Some(summary) = slinker_core::profile::report() {
                    eprint!("{summary}");
                }
                code
            })
            .expect("spawn the slinker command thread")
            .join()
            .unwrap_or(ExitCode::FAILURE),
    }
}

fn report(result: Result<(), Box<dyn Error>>, format: OutputFormat) -> ExitCode {
    let Err(error) = result else {
        return ExitCode::SUCCESS;
    };
    if format == OutputFormat::Json {
        println!("{:#}", failure_document(error.as_ref()));
    } else {
        eprintln!("slinker: {error}");
        let mut source = error.source();
        while let Some(cause) = source {
            eprintln!("  caused by: {cause}");
            source = cause.source();
        }
    }
    ExitCode::FAILURE
}

fn failure_document(error: &(dyn Error + 'static)) -> serde_json::Value {
    let mut current = Some(error);
    while let Some(cause) = current {
        if let Some(report) = cause.downcast_ref::<BuildReport>() {
            return json!({ "status": "blocked", "groups": report.groups() });
        }
        current = cause.source();
    }
    let mut causes = Vec::new();
    let mut source = error.source();
    while let Some(cause) = source {
        causes.push(cause.to_string());
        source = cause.source();
    }
    json!({ "status": "error", "message": error.to_string(), "causes": causes })
}

fn run(command: UserCommand) -> Result<(), Box<dyn Error>> {
    match command {
        UserCommand::Build(args) => build(&args),
        UserCommand::Check(args) => check(&args),
        UserCommand::Analyze(args) => analyze(&args),
        UserCommand::Why(args) => explain_why(&args),
        UserCommand::Path(args) => explain_paths(&args),
        UserCommand::Cache(args) => cache_command::run(&args, &cache_location()),
    }
}

fn package_name(value: &str) -> Result<PackageName, &'static str> {
    if value.is_empty() || value.contains(['/', '\\']) {
        return Err("expected an R package name");
    }
    Ok(PackageName::from(value))
}

fn link(args: &AnalysisArgs) -> Result<(Session, LinkIr), Box<dyn Error>> {
    let session = Session::open(&args.root, &args.universe.options(), discover_r_home()?)?;
    let plan = session.analyze(true)?;
    Ok((session, plan))
}

fn build(args: &BuildArgs) -> Result<(), Box<dyn Error>> {
    let prepared =
        SourceSession::prepare(&args.path, &args.universe.options(), discover_r_home()?)?;
    let package = prepared.snapshot().package().to_owned();
    let output = args.output.clone().unwrap_or_else(|| {
        prepared
            .snapshot()
            .original_root()
            .with_file_name(format!("{package}-slinked"))
    });
    let output = std::path::absolute(&output)?;
    prepared.build(&output)?;
    print_built(args.json.format(), &package, &output);
    Ok(())
}

fn print_built(format: OutputFormat, package: &str, output: &std::path::Path) {
    if format == OutputFormat::Json {
        let rendered = json!({
            "status": "built",
            "package": package,
            "output": output,
        });
        println!("{rendered:#}");
    } else {
        println!("{}", output.display());
    }
}

fn check(args: &CheckArgs) -> Result<(), Box<dyn Error>> {
    let session = SourceSession::open(&args.path, &args.universe.options(), discover_r_home()?)?;
    let package = session.snapshot().package().to_owned();
    let version = session.snapshot().version().to_string();
    session.check()?;
    if args.json.format() == OutputFormat::Json {
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
    if args.json.format() == OutputFormat::Json {
        let graph =
            slinker_core::profile::scoped(slinker_core::profile::Probe::ExplanationBuild, || {
                ExplanationDag::from_plan(&plan, target, session.root())
            })?;
        let _serialize =
            slinker_core::profile::span(slinker_core::profile::Probe::ExplanationSerialize);
        let mut stdout = io::BufWriter::with_capacity(1 << 20, io::stdout().lock());
        serde_json::to_writer_pretty(&mut stdout, &graph)?;
        writeln!(stdout)?;
        stdout.flush()?;
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
            print_direct_use(&plan, edge);
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
            print_direct_use(&plan, entry);
        }
    }
    Ok(())
}

fn matching_nodes(plan: &LinkIr, target: &QueryTarget) -> Vec<NodeId> {
    plan.provenance()
        .nodes()
        .iter()
        .filter(|node| target.selects(node))
        .map(|node| node.id)
        .collect()
}

fn package_entry_edges<'a>(plan: &'a LinkIr, target: &QueryTarget) -> Vec<&'a Edge> {
    let nodes = plan.provenance().nodes();
    let mut edges = plan
        .provenance()
        .edges()
        .iter()
        .filter(|edge| {
            target.selects(&nodes[edge.to.0]) && nodes[edge.from.0].package != target.package
        })
        .collect::<Vec<_>>();
    edges.sort_by_key(|edge| (edge.from.0, edge.to.0));
    edges
}

fn print_direct_use(plan: &LinkIr, edge: &Edge) {
    println!(
        "  {} -- {:?}{}: {} --> {}",
        node_label(plan, edge.from),
        edge.kind,
        edge_location(plan, edge),
        edge.reason,
        node_label(plan, edge.to)
    );
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
    let source = plan.sources().origin(&span.source);
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
        } => {
            let origin = "installed";
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
    roles::print_package_roles(plan);
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
#[path = "../tests/unit/cli.rs"]
mod tests;
