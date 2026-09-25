use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

#[test]
fn build_defaults_to_current_package_and_emits_installable_source() {
    let r_home = discover_r_home();
    let fixture = tempfile::tempdir().expect("fixture tempdir");
    let source = fixture.path().join("rootonly");
    write_package(
        &source,
        "rootonly",
        "",
        "export(hello)\n",
        "hello <- function(name = 'world') paste0('hello ', name)\n",
    );
    let original = fs::read(source.join("R/code.R")).expect("original root source");
    let output = fixture.path().join("generated-rootonly");
    let result = Command::new(env!("CARGO_BIN_EXE_slinker"))
        .arg("build")
        .arg("--output")
        .arg(&output)
        .current_dir(&source)
        .env("R_HOME", &r_home)
        .output()
        .expect("run slinker build");
    assert_success(&result, "slinker build root-only");
    assert!(output.join("DESCRIPTION").is_file());
    assert!(output.join("NAMESPACE").is_file());
    assert!(output.join("R/zzz-slinker-generated.R").is_file());
    assert!(!output.join("R/code.R").exists());
    assert_eq!(
        fs::read(source.join("R/code.R")).expect("root source after build"),
        original
    );

    let validation = fixture.path().join("validation");
    fs::create_dir(&validation).expect("validation library");
    install_package(&r_home, &output, &validation);
    run_r(
        &r_home,
        &validation,
        "library(rootonly); stopifnot(identical(hello('linker'), 'hello linker'))",
    );
}

#[test]
fn build_links_pure_r_dependency_absent_from_runtime_library() {
    let r_home = discover_r_home();
    let fixture = tempfile::tempdir().expect("fixture tempdir");
    let dependency_source = fixture.path().join("tinylinked");
    write_package(
        &dependency_source,
        "tinylinked",
        "",
        "export(bfun)\n",
        "state <- new.env(parent = emptyenv())\nstate$loaded <- FALSE\nbfun <- function(x) paste0('B:', x, ':', state$loaded)\n.onLoad <- function(libname, pkgname) state$loaded <- identical(pkgname, 'tinylinked')\n",
    );
    let build_library = fixture.path().join("build-library");
    fs::create_dir(&build_library).expect("build library");
    install_package(&r_home, &dependency_source, &build_library);

    let root_source = fixture.path().join("linkroot");
    write_package(
        &root_source,
        "linkroot",
        "Imports: tinylinked\n",
        "importFrom(tinylinked, bfun)\nexport(afun)\nexport(qualified)\n",
        "afun <- function(x) bfun(x)\nqualified <- function(x) tinylinked::bfun(x)\n",
    );
    let output = fixture.path().join("generated-linkroot");
    let result = Command::new(env!("CARGO_BIN_EXE_slinker"))
        .args(["build", "--lib"])
        .arg(&build_library)
        .args(["--output"])
        .arg(&output)
        .arg(&root_source)
        .env("R_HOME", &r_home)
        .output()
        .expect("run linked build");
    assert_success(&result, "slinker build linked fixture");
    let description =
        fs::read_to_string(output.join("DESCRIPTION")).expect("generated DESCRIPTION");
    let namespace = fs::read_to_string(output.join("NAMESPACE")).expect("generated NAMESPACE");
    assert!(!description.contains("tinylinked"));
    assert!(!namespace.contains("tinylinked"));

    let validation = fixture.path().join("validation");
    fs::create_dir(&validation).expect("validation library");
    install_package(&r_home, &output, &validation);
    run_r(
        &r_home,
        &validation,
        "library(linkroot); value <- afun('x'); if (!identical(value, 'B:x:TRUE')) stop(sprintf('unexpected value: %s', value)); stopifnot(identical(qualified('q'), 'B:q:TRUE')); stopifnot(isNamespaceLoaded('tinylinked'))",
    );
}

#[test]
fn build_preserves_transitive_external_contract() {
    let r_home = discover_r_home();
    let fixture = tempfile::tempdir().expect("fixture tempdir");
    let build_library = fixture.path().join("build-library");
    fs::create_dir(&build_library).expect("build library");

    let external_source = fixture.path().join("tinyexternal");
    write_package(
        &external_source,
        "tinyexternal",
        "",
        "export(efun)\n",
        "efun <- function(x) paste0('E:', x)\n",
    );
    install_package(&r_home, &external_source, &build_library);

    let linked_source = fixture.path().join("tinybridge");
    write_package(
        &linked_source,
        "tinybridge",
        "Imports: tinyexternal (>= 1.0.0)\n",
        "importFrom(tinyexternal, efun)\nexport(bfun)\n",
        "bfun <- function(x) paste0('B:', efun(x))\n",
    );
    install_package(&r_home, &linked_source, &build_library);

    let root_source = fixture.path().join("externalroot");
    write_package(
        &root_source,
        "externalroot",
        "Imports: tinybridge\n",
        "importFrom(tinybridge, bfun)\nexport(run)\n",
        "run <- function(x) bfun(x)\n",
    );
    let output = fixture.path().join("generated-externalroot");
    let result = Command::new(env!("CARGO_BIN_EXE_slinker"))
        .args(["build", "--lib"])
        .arg(&build_library)
        .args(["--external", "tinyexternal", "--output"])
        .arg(&output)
        .arg(&root_source)
        .env("R_HOME", &r_home)
        .output()
        .expect("run external build");
    assert_success(&result, "slinker build external fixture");
    let description =
        fs::read_to_string(output.join("DESCRIPTION")).expect("generated DESCRIPTION");
    assert!(!description.contains("tinybridge"));
    assert!(description.contains("tinyexternal (>= 1.0.0)"));

    let validation = fixture.path().join("validation");
    fs::create_dir(&validation).expect("validation library");
    install_package(&r_home, &external_source, &validation);
    install_package(&r_home, &output, &validation);
    run_r(
        &r_home,
        &validation,
        "library(externalroot); stopifnot(identical(run('x'), 'B:E:x')); stopifnot(isNamespaceLoaded('tinybridge')); stopifnot(length(find.package('tinybridge', quiet=TRUE)) == 0L); stopifnot(isNamespaceLoaded('tinyexternal'))",
    );

    let collision = fixture.path().join("collision");
    fs::create_dir(&collision).expect("collision library");
    install_package(&r_home, &external_source, &collision);
    install_package(&r_home, &linked_source, &collision);
    install_package(&r_home, &output, &collision);
    let collision_result = run_r_output(
        &r_home,
        &collision,
        "loadNamespace('tinybridge'); library(externalroot)",
    );
    assert!(!collision_result.status.success());
    assert!(String::from_utf8_lossy(&collision_result.stderr).contains("LinkedNamespaceCollision"));
}

#[test]
fn explicit_external_promotes_declared_suggests_contract() {
    let r_home = discover_r_home();
    let fixture = tempfile::tempdir().expect("fixture tempdir");
    let build_library = fixture.path().join("build-library");
    fs::create_dir(&build_library).expect("build library");
    let external_source = fixture.path().join("suggestedexternal");
    write_package(
        &external_source,
        "suggestedexternal",
        "",
        "export(efun)\n",
        "efun <- function(x) paste0('S:', x)\n",
    );
    install_package(&r_home, &external_source, &build_library);
    let root_source = fixture.path().join("suggestroot");
    write_package(
        &root_source,
        "suggestroot",
        "Suggests: suggestedexternal (>= 1.0.0)\n",
        "export(run)\n",
        "run <- function(x) suggestedexternal::efun(x)\n",
    );
    let output = fixture.path().join("generated-suggestroot");
    let result = Command::new(env!("CARGO_BIN_EXE_slinker"))
        .args(["build", "--lib"])
        .arg(&build_library)
        .args(["--external", "suggestedexternal", "--output"])
        .arg(&output)
        .arg(&root_source)
        .env("R_HOME", &r_home)
        .output()
        .expect("run explicit Suggests build");
    assert_success(&result, "slinker build explicit Suggests");
    let description = slinker::Description::parse(
        &fs::read_to_string(output.join("DESCRIPTION")).expect("generated DESCRIPTION"),
    );
    let rendered = |relations: Vec<&slinker::Relation>| {
        relations
            .into_iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
    };
    assert_eq!(
        rendered(description.imports_parsed().values().collect()),
        ["suggestedexternal (>= 1.0.0)"]
    );
    assert_eq!(
        rendered(description.suggests_parsed().values().collect()),
        ["suggestedexternal (>= 1.0.0)"]
    );

    let validation = fixture.path().join("validation");
    fs::create_dir(&validation).expect("validation library");
    install_package(&r_home, &external_source, &validation);
    install_package(&r_home, &output, &validation);
    run_r(
        &r_home,
        &validation,
        "library(suggestroot); stopifnot(identical(run('x'), 'S:x'))",
    );
}

fn write_package(root: &Path, name: &str, extra: &str, namespace: &str, code: &str) {
    fs::create_dir_all(root.join("R")).expect("R directory");
    fs::write(
        root.join("DESCRIPTION"),
        format!(
            "Package: {name}\nVersion: 1.0.0\nTitle: Fixture {name}\nDescription: Generated linker test fixture.\nAuthors@R: person('A', 'B', email='a@example.com', role=c('aut','cre'))\nLicense: MIT\nEncoding: UTF-8\n{extra}"
        ),
    )
    .expect("DESCRIPTION");
    fs::write(root.join("NAMESPACE"), namespace).expect("NAMESPACE");
    fs::write(root.join("R/code.R"), code).expect("R code");
}

fn install_package(r_home: &Path, package: &Path, library: &Path) {
    let output = Command::new(r_executable(r_home))
        .args(["CMD", "INSTALL", "--no-test-load"])
        .arg(format!("--library={}", library.display()))
        .arg(package)
        .env("R_HOME", r_home)
        .output()
        .expect("R CMD INSTALL");
    assert_success(&output, "R CMD INSTALL");
}

fn run_r(r_home: &Path, library: &Path, expression: &str) {
    let output = run_r_output(r_home, library, expression);
    assert_success(&output, "target R expression");
}

fn run_r_output(r_home: &Path, library: &Path, expression: &str) -> Output {
    Command::new(r_executable(r_home))
        .args([
            "--slave",
            "--no-save",
            "--no-restore",
            "--vanilla",
            "-e",
            expression,
        ])
        .env("R_HOME", r_home)
        .env("R_LIBS", library)
        .env_remove("R_LIBS_USER")
        .env_remove("R_LIBS_SITE")
        .output()
        .expect("run target R")
}

fn discover_r_home() -> PathBuf {
    let output = if cfg!(windows) {
        Command::new("cmd").args(["/c", "R RHOME"]).output()
    } else {
        Command::new("R").arg("RHOME").output()
    }
    .expect("R RHOME");
    assert_success(&output, "R RHOME");
    let home = String::from_utf8(output.stdout).expect("R home UTF-8");
    dunce::canonicalize(
        home.lines()
            .rev()
            .find(|line| !line.trim().is_empty())
            .expect("R home line")
            .trim(),
    )
    .expect("canonical R home")
}

fn r_executable(r_home: &Path) -> PathBuf {
    [
        r_home.join("bin/x64/R.exe"),
        r_home.join("bin/R.exe"),
        r_home.join("bin/R"),
    ]
    .into_iter()
    .find(|path| path.is_file())
    .expect("target R executable")
}

fn assert_success(output: &Output, operation: &str) {
    assert!(
        output.status.success(),
        "{operation} failed ({})\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
