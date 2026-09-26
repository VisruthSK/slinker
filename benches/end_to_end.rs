#[path = "../tests/common/mod.rs"]
mod common;
mod support;

use slinker::analysis::{LinkIr, Linker};
use slinker::cache::CacheLocation;
use slinker::package::PackageStore;
use slinker::{TargetEnvironment, TargetEnvironmentRequest};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

const ANALYZED: [&str; 3] = ["rlang", "cli", "testthat"];
const BUILT: [&str; 2] = ["here", "rebus.numbers"];

fn jobs() -> usize {
    std::thread::available_parallelism().map_or(1, usize::from)
}

fn analyze(
    r_home: &Path,
    target: &TargetEnvironment,
    root: &str,
    cache: CacheLocation,
) -> (Duration, LinkIr) {
    let start = Instant::now();
    let plan = PackageStore::new(r_home.to_path_buf(), target.clone(), cache)
        .and_then(|store| Linker::new(store, jobs()).analyze(root))
        .unwrap_or_else(|error| panic!("analyze {root}: {error}"));
    (start.elapsed(), plan)
}

fn report_analysis(name: &str, elapsed: Duration, plan: &LinkIr) {
    println!(
        "{name:<32} {:>9.3} s  bindings {:>6}  construction evaluations {:>8}  blockers {:>4}",
        elapsed.as_secs_f64(),
        plan.program().bindings().len(),
        plan.construction_evaluations(),
        plan.blockers().len()
    );
}

fn analyze_installed(r_home: &Path) {
    let target = TargetEnvironmentRequest::new(r_home.to_path_buf())
        .capture()
        .expect("capture the target R library universe");
    for root in ANALYZED {
        let (elapsed, plan) = analyze(r_home, &target, root, CacheLocation::Disabled);
        report_analysis(&format!("analyze {root} cold"), elapsed, &plan);

        let warm = tempfile::tempdir().expect("warm cache directory");
        let directory = || CacheLocation::Directory(warm.path().to_path_buf());
        analyze(r_home, &target, root, directory());
        let (elapsed, plan) = analyze(r_home, &target, root, directory());
        report_analysis(&format!("analyze {root} warm"), elapsed, &plan);
    }
}

fn provision(r_home: &Path, package: &str) -> (PathBuf, PathBuf) {
    let root = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join("bench")
        .join(package);
    let source = root.join("source").join(package);
    let library = root.join("library");
    common::run_r_with_site_profile(
        r_home,
        &format!(
            r#"
            package <- {package}
            if (identical(unname(getOption("repos")["CRAN"]), "@CRAN@")) {{
              options(repos = c(CRAN = "https://cloud.r-project.org"))
            }}
            db <- available.packages()
            base <- c(rownames(installed.packages(priority = "base")), "R")
            fields <- c("Depends", "Imports", "LinkingTo")
            found <- tools::package_dependencies(package, db = db, which = fields, recursive = TRUE)
            required <- setdiff(unlist(found), base)
            dir.create({library}, recursive = TRUE, showWarnings = FALSE)
            missing <- setdiff(required, rownames(installed.packages({library}, noCache = TRUE)))
            if (length(missing)) install.packages(missing, lib = {library}, dependencies = FALSE)
            absent <- setdiff(required, rownames(installed.packages({library}, noCache = TRUE)))
            if (length(absent)) stop("could not install: ", toString(absent))
            if (!dir.exists({source})) {{
              tarball <- download.packages(package, tempdir(), type = "source")[1, 2]
              untar(tarball, exdir = dirname({source}))
            }}
            "#,
            package = r_string(package),
            library = r_string(&library),
            source = r_string(&source),
        ),
    );
    (source, library)
}

fn build(r_home: &Path, package: &str) {
    let (source, library) = provision(r_home, package);
    let work = tempfile::tempdir().expect("build work directory");
    let output = work.path().join("output");
    let start = Instant::now();
    let result = Command::new(env!("CARGO_BIN_EXE_slinker"))
        .arg("build")
        .arg(&source)
        .arg("--lib")
        .arg(&library)
        .arg("--output")
        .arg(&output)
        .env("SLINKER_CACHE_DIR", work.path().join("cache"))
        .output()
        .expect("run slinker build");
    let elapsed = start.elapsed();
    common::assert_success(&result, &format!("slinker build {package}"));
    println!(
        "{:<32} {:>9.3} s",
        format!("build {package} cold"),
        elapsed.as_secs_f64()
    );
}

fn r_string(value: impl AsRef<OsStr>) -> String {
    let value = value.as_ref().to_string_lossy().replace('\\', "/");
    format!("\"{}\"", value.replace('"', "\\\""))
}

fn run() {
    let r_home = common::discover_r_home();
    analyze_installed(&r_home);
    for package in BUILT {
        build(&r_home, package);
    }
}

fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    support::main(run)
}
