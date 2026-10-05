#[path = "../tests/common/mod.rs"]
mod common;
mod support;

use slinker_core::TargetEnvironment;
use slinker_core::analysis::{LinkIr, Linker};
use slinker_core::cache::CacheLocation;
use slinker_core::package::PackageStore;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

#[cfg(feature = "profile")]
#[global_allocator]
static ALLOCATOR: slinker_core::profile::heap::CountingAllocator =
    slinker_core::profile::heap::CountingAllocator;

const ANALYZED: [&str; 6] = ["R6", "jsonlite", "rlang", "cli", "callr", "testthat"];
const BUILT: [&str; 2] = ["here", "rebus.numbers"];
const DEFAULT_THREADS: usize = 4;

fn threads() -> usize {
    std::thread::available_parallelism()
        .map_or(1, usize::from)
        .min(DEFAULT_THREADS)
}

fn analyze(
    r_home: &Path,
    target: &TargetEnvironment,
    root: &str,
    cache: CacheLocation,
) -> (Duration, LinkIr, String) {
    #[cfg(feature = "profile")]
    slinker_core::profile::heap::begin_measurement();
    let start = Instant::now();
    let plan = PackageStore::new(r_home.to_path_buf(), target.clone(), cache)
        .and_then(|store| Linker::new(store, threads()).analyze(root))
        .unwrap_or_else(|error| panic!("analyze {root}: {error}"));
    let elapsed = start.elapsed();
    (elapsed, plan, heap_usage())
}

#[cfg(feature = "profile")]
fn heap_usage() -> String {
    let usage = slinker_core::profile::heap::usage();
    format!(
        "  heap peak {:>7.1} MiB  allocations {:>10}",
        usage.peak_mib, usage.allocations
    )
}

#[cfg(not(feature = "profile"))]
fn heap_usage() -> String {
    String::new()
}

fn report_analysis(name: &str, elapsed: Duration, plan: &LinkIr, heap: &str) {
    println!(
        "{name:<32} {:>9.3} s  bindings {:>6}  blockers {:>4}{heap}",
        elapsed.as_secs_f64(),
        plan.program().bindings().len(),
        plan.blockers().len()
    );
}

fn analyze_installed(r_home: &Path) {
    let target = support::target_request(r_home)
        .capture()
        .expect("capture the target R library universe");
    for root in ANALYZED {
        let (elapsed, plan, heap) = analyze(r_home, &target, root, CacheLocation::Disabled);
        report_analysis(&format!("analyze {root} cold"), elapsed, &plan, &heap);

        let warm = tempfile::tempdir().expect("warm cache directory");
        let directory = || CacheLocation::Directory(warm.path().to_path_buf());
        analyze(r_home, &target, root, directory());
        let (elapsed, plan, heap) = analyze(r_home, &target, root, directory());
        report_analysis(&format!("analyze {root} warm"), elapsed, &plan, &heap);
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
            if (!file.exists(file.path({source}, "DESCRIPTION"))) {{
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

fn analyze_edit(r_home: &Path, package: &str) {
    let (source, library) = provision(r_home, package);
    let work = tempfile::tempdir().expect("edit work directory");
    let edited = work.path().join(package);
    copy_directory(&source, &edited);
    let cache = work.path().join("cache");
    let timed = |label: &str| {
        let start = Instant::now();
        let result = Command::new(env!("CARGO_BIN_EXE_slinker"))
            .arg("analyze")
            .arg(&edited)
            .arg("--lib")
            .arg(&library)
            .env("SLINKER_CACHE_DIR", &cache)
            .output()
            .expect("run slinker analyze");
        let elapsed = start.elapsed();
        common::assert_success(&result, &format!("slinker analyze {package}"));
        println!(
            "{:<32} {:>9.3} s",
            format!("analyze {package} {label}"),
            elapsed.as_secs_f64()
        );
    };
    timed("source cold");
    timed("source warm");
    let entry = std::fs::read_dir(edited.join("R"))
        .expect("package R directory")
        .filter_map(std::result::Result::ok)
        .map(|entry| entry.path())
        .min()
        .expect("a source file to edit");
    let mut text = std::fs::read_to_string(&entry).expect("read source file");
    text.push_str(
        "
.slinker_bench_edit <- function() NULL
",
    );
    std::fs::write(&entry, text).expect("edit source file");
    timed("one-edit");
}

fn copy_directory(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).expect("create directory");
    for entry in std::fs::read_dir(from)
        .expect("list directory")
        .filter_map(std::result::Result::ok)
    {
        let target = to.join(entry.file_name());
        if entry.path().is_dir() {
            copy_directory(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).expect("copy file");
        }
    }
}

fn r_string(value: impl AsRef<OsStr>) -> String {
    let value = value.as_ref().to_string_lossy().replace('\\', "/");
    format!("\"{}\"", value.replace('"', "\\\""))
}

fn run() {
    let r_home = common::discover_r_home();
    let selected = |section: &str| support::filter().is_none_or(|filter| section.contains(&filter));
    if selected("analyze") {
        analyze_installed(&r_home);
    }
    if selected("build") {
        for package in BUILT {
            build(&r_home, package);
            analyze_edit(&r_home, package);
        }
    }
}

fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    support::main(run)
}
