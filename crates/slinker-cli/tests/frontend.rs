mod common;

use common::{assert_success, discover_r_home, install_package};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn slinker(args: &[&str], paths: &[&Path]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_slinker"))
        .args(args)
        .args(paths)
        .output()
        .expect("run slinker")
}

fn write_package(root: &Path, name: &str, namespace: &str, code: &str) {
    fs::create_dir_all(root.join("R")).expect("R directory");
    fs::write(
        root.join("DESCRIPTION"),
        format!(
            "Package: {name}\nVersion: 1.0.0\nTitle: Fixture {name}\nDescription: Generated linker test fixture.\nAuthors@R: person('A', 'B', email='a@example.com', role=c('aut','cre'))\nLicense: MIT\nEncoding: UTF-8\n"
        ),
    )
    .expect("DESCRIPTION");
    fs::write(root.join("NAMESPACE"), namespace).expect("NAMESPACE");
    fs::write(root.join("R/code.R"), code).expect("R code");
}

fn rootonly_fixture(parent: &Path) -> PathBuf {
    let source = parent.join("rootonly");
    write_package(
        &source,
        "rootonly",
        "export(hello)\n",
        "hello <- function(name = 'world') paste0('hello ', name)\n",
    );
    source
}

fn blocked_fixture(parent: &Path) -> PathBuf {
    let source = parent.join("blockedroot");
    write_package(
        &source,
        "blockedroot",
        "export(discover, dispatch)\n",
        "discover <- function(package) requireNamespace(package)\ndispatch <- function(x, generic) UseMethod(generic)\n",
    );
    source
}

#[test]
fn check_runs_preflight_and_writes_nothing() {
    let fixture = tempfile::tempdir().expect("fixture tempdir");
    let source = rootonly_fixture(fixture.path());

    let result = slinker(&["check", "--json"], &[&source]);

    assert_success(&result, "slinker check");
    let report: serde_json::Value = serde_json::from_slice(&result.stdout).expect("JSON report");
    assert_eq!(report["status"], "ok");
    assert_eq!(report["package"], "rootonly");
    assert!(!source.join("target").exists());
}

#[test]
fn library_build_uses_a_separate_worker_and_frozen_options() {
    use slinker_core::WorkerExecutable;
    use slinker_core::cache::CacheLocation;
    use slinker_core::session::{SessionOptions, SourceSession};

    let fixture = tempfile::tempdir().expect("fixture tempdir");
    let source = rootonly_fixture(fixture.path());
    let output = fixture.path().join("generated");
    let r_home = discover_r_home();
    let mut options = SessionOptions {
        native_summaries: None,
        libraries: Vec::new(),
        external: Vec::new(),
        linked: Vec::new(),
        threads: std::num::NonZeroUsize::new(2).unwrap(),
        cache: CacheLocation::Directory(fixture.path().join("cache")),
        worker_executable: WorkerExecutable::Standalone(env!("CARGO_BIN_EXE_slinker").into()),
    };
    let prepared =
        SourceSession::prepare(&source, &options, r_home.clone()).expect("library capture");
    let original = options.clone();
    options
        .libraries
        .push(fixture.path().join("missing-library"));
    options.worker_executable = WorkerExecutable::Standalone(fixture.path().join("missing-worker"));
    prepared.build(&output).expect("library build");
    let prepared = SourceSession::prepare(&source, &original, r_home).expect("second capture");
    prepared.build(&output).expect("library rebuild");
    assert!(output.join("R/zzz-slinker-generated.R").is_file());
}

#[test]
fn blocked_check_groups_blockers_and_reports_them_as_json() {
    let fixture = tempfile::tempdir().expect("fixture tempdir");
    let source = blocked_fixture(fixture.path());

    let human = slinker(&["check"], &[&source]);
    let json = slinker(&["check", "--json"], &[&source]);

    assert!(!human.status.success());
    let stderr = String::from_utf8_lossy(&human.stderr);
    assert!(stderr.contains("  DynamicPackageDiscovery ("), "{stderr}");
    assert!(
        stderr.contains("    - DynamicPackageDiscovery in blockedroot::discover:"),
        "{stderr}"
    );
    assert!(!json.status.success());
    let report: serde_json::Value = serde_json::from_slice(&json.stdout).expect("JSON report");
    assert_eq!(report["status"], "blocked");
    let groups = report["groups"].as_array().expect("groups");
    let codes = groups
        .iter()
        .map(|group| group["code"].as_str().expect("group code"))
        .collect::<Vec<_>>();
    assert!(codes.contains(&"dynamic_package_discovery"), "{codes:?}");
    assert!(codes.contains(&"object_system"), "{codes:?}");
    let discovery = groups
        .iter()
        .find(|group| group["code"] == "dynamic_package_discovery")
        .expect("discovery group");
    let blocker = &discovery["blockers"][0];
    assert_eq!(blocker["package"], "blockedroot");
    assert_eq!(blocker["binding"], "discover");
    assert!(blocker["location"]["line"].as_u64().is_some(), "{blocker}");
}

#[test]
fn library_rejects_a_concurrently_owned_output() {
    use slinker_core::WorkerExecutable;
    use slinker_core::cache::CacheLocation;
    use slinker_core::session::{SessionOptions, SourceSession};

    let fixture = tempfile::tempdir().expect("fixture tempdir");
    let source = rootonly_fixture(fixture.path());
    let output = fixture.path().join("generated");
    let cache = fixture.path().join("cache");
    fs::create_dir(&cache).unwrap();
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(fixture.path().join(".#generated.slinker-lock"))
        .unwrap();
    lock.try_lock().expect("hold publication ownership");
    let options = SessionOptions {
        native_summaries: None,
        libraries: Vec::new(),
        external: Vec::new(),
        linked: Vec::new(),
        threads: std::num::NonZeroUsize::new(1).unwrap(),
        cache: CacheLocation::Directory(cache),
        worker_executable: WorkerExecutable::Standalone(env!("CARGO_BIN_EXE_slinker").into()),
    };
    let prepared = SourceSession::prepare(&source, &options, discover_r_home()).unwrap();
    assert!(prepared.build(&output).is_err());
    assert!(
        !output.exists(),
        "failed build published a complete package"
    );
}

#[test]
fn analyze_accepts_source_paths_installed_names_and_installed_directories() {
    let r_home = discover_r_home();
    let fixture = tempfile::tempdir().expect("fixture tempdir");
    let source = rootonly_fixture(fixture.path());
    let library = fixture.path().join("library");
    fs::create_dir(&library).expect("library");
    install_package(&r_home, &source, &library);
    let installed_directory = library.join("rootonly");

    let from_source = slinker(&["analyze"], &[&source]);
    let from_name = Command::new(env!("CARGO_BIN_EXE_slinker"))
        .args(["analyze", "rootonly", "--lib"])
        .arg(&library)
        .output()
        .expect("analyze installed name");
    let from_directory = slinker(&["analyze"], &[&installed_directory]);

    for (result, label) in [
        (&from_source, "source package"),
        (&from_name, "installed name"),
        (&from_directory, "installed directory"),
    ] {
        assert_success(result, label);
        let stdout = String::from_utf8_lossy(&result.stdout);
        assert!(
            stdout.contains("package: rootonly 1.0.0"),
            "{label}: {stdout}"
        );
        assert!(stdout.contains("none"), "{label}: {stdout}");
    }
}

#[test]
fn json_mode_reports_failures_as_one_document_on_stdout() {
    let fixture = tempfile::tempdir().expect("fixture tempdir");
    let missing = fixture.path().join("absent");

    let result = slinker(&["check", "--json"], &[&missing]);

    assert!(!result.status.success());
    assert!(result.stderr.is_empty());
    let report: serde_json::Value = serde_json::from_slice(&result.stdout).expect("JSON report");
    assert_eq!(report["status"], "error");
    assert!(report["message"].is_string(), "{report}");
}
