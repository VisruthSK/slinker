mod common;

use common::{assert_success, discover_r_home, install_package, run_r};
use std::fs;
use std::path::Path;
use std::process::{Command, Output};

fn package(root: &Path, name: &str, imports: &str, code: &str) {
    fs::create_dir_all(root.join("R")).unwrap();
    fs::write(root.join("DESCRIPTION"), format!(
        "Package: {name}\nVersion: 1.0.0\nTitle: Ownership Fixture\nDescription: Tests source and output ownership.\nAuthors@R: person('A', 'B', email='a@example.com', role=c('aut','cre'))\nLicense: MIT\n{imports}"
    )).unwrap();
    fs::write(root.join("NAMESPACE"), "export(run)\n").unwrap();
    fs::write(root.join("R/code.R"), code).unwrap();
}

fn build(source: &Path, output: &Path, library: &Path, cache: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_slinker"))
        .arg("build")
        .arg(source)
        .arg("--output")
        .arg(output)
        .arg("--lib")
        .arg(library)
        .env("SLINKER_CACHE_DIR", cache)
        .output()
        .unwrap()
}

fn install_built_tarball(r_home: &Path, source: &Path, library: &Path, archive: &str) {
    let directory = tempfile::tempdir().unwrap();
    let result = Command::new(slinker_core::r_executable(r_home).unwrap())
        .args(["CMD", "build", "--no-build-vignettes", "--no-manual"])
        .arg(source)
        .current_dir(directory.path())
        .output()
        .unwrap();
    assert_success(&result, "build generated tarball");
    install_package(r_home, &directory.path().join(archive), library);
}

#[test]
fn source_installation_inputs_are_reobserved_on_every_build() {
    let work = tempfile::tempdir().unwrap();
    let dependency = work.path().join("capturedep");
    let source = work.path().join("captureroot");
    let library = work.path().join("library");
    let generated_library = work.path().join("generated-library");
    fs::create_dir(&library).unwrap();
    fs::create_dir(&generated_library).unwrap();
    let r_home = discover_r_home();
    package(&dependency, "capturedep", "", "run <- function() 1L\n");
    install_package(&r_home, &dependency, &library);
    package(
        &source,
        "captureroot",
        "Imports: capturedep\n",
        "captured <- capturedep::run()\nrun <- function() captured\n",
    );
    let output = work.path().join("output");
    let cache = work.path().join("cache");
    assert_success(&build(&source, &output, &library, &cache), "first build");

    package(&dependency, "capturedep", "", "run <- function() 2L\n");
    install_package(&r_home, &dependency, &library);
    install_package(&r_home, &source, &library);
    run_r(
        &r_home,
        &library,
        "stopifnot(identical(captureroot::run(), 2L))",
    );
    assert_success(&build(&source, &output, &library, &cache), "second build");
    install_package(&r_home, &output, &generated_library);
    run_r(
        &r_home,
        &generated_library,
        "stopifnot(identical(captureroot::run(), 2L))",
    );
}

#[test]
fn source_exclusions_use_target_r_pcre_and_do_not_exclude_generated_code() {
    let work = tempfile::tempdir().unwrap();
    let source = work.path().join("filteredroot");
    let library = work.path().join("library");
    fs::create_dir(&library).unwrap();
    package(&source, "filteredroot", "", "run <- function() 42L\n");
    fs::create_dir(source.join("ignored")).unwrap();
    fs::write(source.join("ignored/file"), "excluded").unwrap();
    fs::write(source.join(".Rbuildignore"), "^ignored(?=/)\n^R/zzz\n").unwrap();
    let output = work.path().join("output");
    assert_success(
        &build(&source, &output, &library, &work.path().join("cache")),
        "PCRE build",
    );
    let r_home = discover_r_home();
    install_built_tarball(&r_home, &output, &library, "filteredroot_1.0.0.tar.gz");
    run_r(
        &r_home,
        &library,
        "stopifnot(identical(filteredroot::run(), 42L))",
    );
    assert!(!output.join("ignored/file").exists());
}

#[test]
fn generated_resource_paths_are_checked_before_publication() {
    let work = tempfile::tempdir().unwrap();
    let source = work.path().join("collisionroot");
    let library = work.path().join("library");
    fs::create_dir(&library).unwrap();
    package(&source, "collisionroot", "", "run <- function() 42L\n");
    fs::create_dir_all(source.join("inst/slinker/payload")).unwrap();
    fs::write(
        source.join("inst/slinker/payload/collisionroot.rds"),
        "original",
    )
    .unwrap();
    let output = work.path().join("output");
    let result = build(&source, &output, &library, &work.path().join("cache"));
    assert!(
        !result.status.success(),
        "reserved resource tree was accepted"
    );
    assert!(String::from_utf8_lossy(&result.stderr).contains("inst/slinker"));
    assert!(!output.exists());
}

fn configure(source: &Path, script: &str) {
    for name in ["configure", "configure.win"] {
        let path = source.join(name);
        fs::write(&path, script).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
        }
    }
}

#[test]
fn staging_preserves_frozen_source_and_retains_configured_resources() {
    use slinker_core::source::{SourcePackageSnapshot, stage_root};

    let work = tempfile::tempdir().unwrap();
    let source = work.path().join("frozenroot");
    let generated = work.path().join("generated");
    fs::create_dir(&generated).unwrap();
    package(&source, "frozenroot", "", "run <- function() 1L\n");
    configure(
        &source,
        "#!/bin/sh\nprintf 'run <- function() 42L\\n' > R/code.R\nmkdir -p inst/configured\nprintf 'configured\\n' > inst/configured/value\n",
    );
    let r_home = discover_r_home();
    let snapshot = SourcePackageSnapshot::capture(&source, &r_home).unwrap();
    let before = fs::read(snapshot.root().join("R/code.R")).unwrap();
    let staged = stage_root(&snapshot, &r_home, &[]).unwrap();
    assert_eq!(
        fs::read(snapshot.root().join("R/code.R")).unwrap(),
        before,
        "staging mutated the frozen source"
    );
    assert_eq!(
        slinker_core::package::tree_digest(snapshot.root()).unwrap(),
        *snapshot.fingerprint()
    );
    assert!(!snapshot.root().join("inst/configured/value").exists());
    let oracle = "stopifnot(identical(frozenroot::run(), 42L), identical(readLines(system.file('configured/value', package='frozenroot')), 'configured'))";
    run_r(&r_home, staged.library(), oracle);
    let output = work.path().join("output");
    assert_success(
        &build(
            &source,
            &output,
            staged.library(),
            &work.path().join("cache"),
        ),
        "configured resource build",
    );
    install_package(&r_home, &output, &generated);
    run_r(&r_home, &generated, oracle);
    install_built_tarball(&r_home, &output, &generated, "frozenroot_1.0.0.tar.gz");
    run_r(&r_home, &generated, oracle);
}

#[test]
fn configure_cannot_reintroduce_linked_imports_during_generated_installation() {
    let work = tempfile::tempdir().unwrap();
    let dependency = work.path().join("configuredep");
    let source = work.path().join("configureroot");
    let library = work.path().join("library");
    let absent = work.path().join("absent");
    fs::create_dir(&library).unwrap();
    fs::create_dir(&absent).unwrap();
    let r_home = discover_r_home();
    package(
        &dependency,
        "configuredep",
        "",
        "configured <- function() 42L\n",
    );
    fs::write(dependency.join("NAMESPACE"), "export(configured)\n").unwrap();
    install_package(&r_home, &dependency, &library);
    package(
        &source,
        "configureroot",
        "Imports: configuredep\n",
        "run <- function() configured()\n",
    );
    configure(
        &source,
        "#!/bin/sh\nprintf 'importFrom(configuredep, configured)\\nexport(run)\\n' > NAMESPACE\n",
    );
    install_package(&r_home, &source, &library);
    run_r(
        &r_home,
        &library,
        "stopifnot(identical(configureroot::run(), 42L))",
    );
    let output = work.path().join("output");
    assert_success(
        &build(&source, &output, &library, &work.path().join("cache")),
        "configured build",
    );
    install_package(&r_home, &output, &absent);
    run_r(
        &r_home,
        &absent,
        "stopifnot(identical(configureroot::run(), 42L))",
    );
    assert!(!output.join("configure").exists());
    assert!(!output.join("configure.win").exists());
}

#[test]
fn foreign_closure_inspection_does_not_execute_namespace_hooks() {
    let work = tempfile::tempdir().unwrap();
    let dependency = work.path().join("hookdep");
    let source = work.path().join("aliasroot");
    let library = work.path().join("library");
    let marker = work.path().join("hook-calls");
    fs::create_dir(&library).unwrap();
    let marker_r = serde_json::to_string(&marker.to_string_lossy().replace('\\', "/")).unwrap();
    package(
        &dependency,
        "hookdep",
        "",
        &format!(
            "hidden <- function() 42L\nrun <- function() hidden()\n.onLoad <- function(...) cat('load\\n', file={marker_r}, append=TRUE)\n"
        ),
    );
    package(
        &source,
        "aliasroot",
        "Imports: hookdep\n",
        "alias <- hookdep::run\nrun <- function() alias()\n",
    );
    let r_home = discover_r_home();
    install_package(&r_home, &dependency, &library);
    install_package(&r_home, &source, &library);
    assert_eq!(fs::read_to_string(&marker).unwrap().lines().count(), 1);
    fs::remove_file(&marker).unwrap();
    let installed = Command::new(env!("CARGO_BIN_EXE_slinker"))
        .args(["analyze", "aliasroot", "--lib"])
        .arg(&library)
        .env("SLINKER_CACHE_DIR", work.path().join("cache-installed"))
        .output()
        .unwrap();
    assert_success(&installed, "installed alias inspection");
    assert!(!marker.exists(), "inspection executed the foreign .onLoad");
    let staged = Command::new(env!("CARGO_BIN_EXE_slinker"))
        .arg("analyze")
        .arg(&source)
        .arg("--lib")
        .arg(&library)
        .env("SLINKER_CACHE_DIR", work.path().join("cache-source"))
        .output()
        .unwrap();
    assert_success(&staged, "source alias inspection");
    assert_eq!(
        fs::read_to_string(&marker).unwrap().lines().count(),
        1,
        "only Root staging may load the dependency"
    );
}

#[test]
fn configured_root_native_audit_is_bound_to_source_and_frozen_once() {
    use slinker_core::cache::CacheLocation;
    use slinker_core::session::{SessionOptions, SourceSession};
    use slinker_core::source::SourcePackageSnapshot;
    let work = tempfile::tempdir().unwrap();
    let source = work.path().join("configurednative");
    let library = work.path().join("library");
    let generated_library = work.path().join("generated-library");
    fs::create_dir(&library).unwrap();
    fs::create_dir(&generated_library).unwrap();
    package(
        &source,
        "configurednative",
        "",
        "run <- function() .Call('configured_tick', PACKAGE='configurednative')\n",
    );
    fs::write(
        source.join("NAMESPACE"),
        "useDynLib(configurednative, .registration=TRUE)\nexport(run)\n",
    )
    .unwrap();
    fs::create_dir(source.join("src")).unwrap();
    fs::write(source.join("src/native.c"), "#include <R.h>\n#include <Rinternals.h>\n#include <R_ext/Rdynload.h>\n#include \"config.h\"\nSEXP configured_tick(void) { return Rf_ScalarInteger(CONFIGURED_VALUE); }\nstatic const R_CallMethodDef calls[] = {{\"configured_tick\", (DL_FUNC)&configured_tick, 0}, {NULL, NULL, 0}};\nvoid R_init_configurednative(DllInfo *dll) { R_registerRoutines(dll, NULL, calls, NULL, NULL); R_useDynamicSymbols(dll, FALSE); }\n").unwrap();
    configure(
        &source,
        "#!/bin/sh\nprintf '#define CONFIGURED_VALUE 42\\n' > src/config.h\n",
    );
    let r_home = discover_r_home();
    install_package(&r_home, &source, &library);
    run_r(
        &r_home,
        &library,
        "stopifnot(identical(configurednative::run(), 42L))",
    );
    let fingerprint = SourcePackageSnapshot::capture(&source, &r_home)
        .unwrap()
        .fingerprint()
        .clone();
    let mut request = slinker_core::TargetEnvironmentRequest::new(&r_home);
    request.worker_executable =
        slinker_core::WorkerExecutable::Standalone(env!("CARGO_BIN_EXE_slinker").into());
    let target = request.capture().unwrap().target;
    let summaries = work.path().join("audits.json");
    fs::write(&summaries, serde_json::to_vec(&serde_json::json!({
        "schema": 1, "packages": [{ "package": "configurednative", "version": "1.0.0", "origin": "root_source", "source_fingerprint": fingerprint, "target": target,
            "components": [{ "component": "configurednative", "safety": "safe" }] }]
    })).unwrap()).unwrap();
    let options = SessionOptions {
        libraries: vec![library],
        external: Vec::new(),
        linked: Vec::new(),
        threads: std::num::NonZeroUsize::new(1).unwrap(),
        cache: CacheLocation::Directory(work.path().join("cache")),
        worker_executable: slinker_core::WorkerExecutable::Standalone(
            env!("CARGO_BIN_EXE_slinker").into(),
        ),
        native_summaries: Some(summaries.clone()),
    };
    let output = work.path().join("output");
    for _ in 0..2 {
        SourceSession::prepare(&source, &options, r_home.clone())
            .unwrap()
            .build(&output)
            .unwrap();
    }
    let frozen = SourceSession::prepare(&source, &options, r_home.clone()).unwrap();
    fs::write(&summaries, "invalid after capture").unwrap();
    frozen
        .build(&output)
        .expect("captured audit remains authoritative");
    assert!(SourceSession::prepare(&source, &options, r_home.clone()).is_err());
    assert!(output.join("R/zzz-slinker-generated.R").is_file());
    assert!(!output.join("src").exists());
    assert!(!output.join("configure").exists());
    install_package(&r_home, &output, &generated_library);
    run_r(
        &r_home,
        &generated_library,
        "stopifnot(identical(configurednative::run(), 42L))",
    );
    install_built_tarball(
        &r_home,
        &output,
        &generated_library,
        "configurednative_1.0.0.tar.gz",
    );
    run_r(
        &r_home,
        &generated_library,
        "stopifnot(identical(configurednative::run(), 42L))",
    );
}

#[test]
fn consumed_install_exclusions_do_not_reintroduce_root_resources() {
    let work = tempfile::tempdir().unwrap();
    let source = work.path().join("instignoredroot");
    let original = work.path().join("original");
    let generated = work.path().join("generated");
    fs::create_dir(&original).unwrap();
    fs::create_dir(&generated).unwrap();
    package(
        &source,
        "instignoredroot",
        "",
        "run <- function() nzchar(system.file('hidden/value', package='instignoredroot'))\n",
    );
    fs::create_dir_all(source.join("inst/hidden")).unwrap();
    fs::write(source.join("inst/hidden/value"), "excluded").unwrap();
    fs::write(source.join(".Rinstignore"), "^inst/hidden\n").unwrap();
    let r_home = discover_r_home();
    install_package(&r_home, &source, &original);
    run_r(
        &r_home,
        &original,
        "stopifnot(identical(instignoredroot::run(), FALSE))",
    );
    let output = work.path().join("output");
    assert_success(
        &build(&source, &output, &original, &work.path().join("cache")),
        "install-exclusion build",
    );
    install_package(&r_home, &output, &generated);
    run_r(
        &r_home,
        &generated,
        "stopifnot(identical(instignoredroot::run(), FALSE))",
    );
}

#[test]
fn a_failed_rebuild_preserves_the_previous_complete_output() {
    let work = tempfile::tempdir().unwrap();
    let source = work.path().join("preservedroot");
    let library = work.path().join("library");
    fs::create_dir(&library).unwrap();
    package(&source, "preservedroot", "", "run <- function() 42L\n");
    let output = work.path().join("output");
    let cache = work.path().join("cache");
    assert_success(&build(&source, &output, &library, &cache), "initial build");
    let before = fs::read(output.join("R/zzz-slinker-generated.R")).unwrap();
    fs::write(
        source.join("R/code.R"),
        "run <- function(pkg) requireNamespace(pkg)\n",
    )
    .unwrap();
    let failed = build(&source, &output, &library, &cache);
    assert!(!failed.status.success());
    assert_eq!(
        before,
        fs::read(output.join("R/zzz-slinker-generated.R")).unwrap()
    );
    let missing = Command::new(env!("CARGO_BIN_EXE_slinker"))
        .arg("build")
        .arg(&source)
        .arg("--output")
        .arg(&output)
        .env(
            "SLINKER_NATIVE_SUMMARIES",
            work.path().join("missing-audits.json"),
        )
        .env("SLINKER_CACHE_DIR", &cache)
        .output()
        .unwrap();
    assert!(!missing.status.success());
    assert_eq!(
        before,
        fs::read(output.join("R/zzz-slinker-generated.R")).unwrap()
    );
    let r_home = discover_r_home();
    install_package(&r_home, &output, &library);
    run_r(
        &r_home,
        &library,
        "stopifnot(identical(preservedroot::run(), 42L))",
    );
}

#[test]
fn default_output_does_not_become_a_source_input() {
    let work = tempfile::tempdir().unwrap();
    let source = work.path().join("defaultroot");
    package(&source, "defaultroot", "", "run <- function() 42L\n");
    for _ in 0..2 {
        let result = Command::new(env!("CARGO_BIN_EXE_slinker"))
            .arg("build")
            .arg(&source)
            .env("SLINKER_CACHE_DIR", work.path().join("cache"))
            .output()
            .unwrap();
        assert_success(&result, "default output build");
    }
    assert!(
        work.path()
            .join("defaultroot-slinked/R/zzz-slinker-generated.R")
            .is_file()
    );
    assert!(!source.join("target").exists());
}
