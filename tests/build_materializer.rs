mod common;

use common::{assert_success, discover_r_home, install_package, run_r, run_r_output};
use std::fs;
use std::path::Path;
use std::process::Command;

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
fn linked_on_load_outside_the_namespace_environment_still_runs() {
    let r_home = discover_r_home();
    let fixture = tempfile::tempdir().expect("fixture tempdir");
    let dependency_source = fixture.path().join("wrappedload");
    write_package(
        &dependency_source,
        "wrappedload",
        "",
        "export(loaded)\n",
        "state <- new.env(parent = emptyenv())\nstate$loaded <- FALSE\nloaded <- function() state$loaded\n.onLoad <- local(function(libname, pkgname) state$loaded <- TRUE)\n",
    );
    let build_library = fixture.path().join("build-library");
    fs::create_dir(&build_library).expect("build library");
    install_package(&r_home, &dependency_source, &build_library);
    run_r(
        &r_home,
        &build_library,
        "library(wrappedload); stopifnot(isTRUE(loaded()))",
    );

    let root_source = fixture.path().join("wraproot");
    write_package(
        &root_source,
        "wraproot",
        "Imports: wrappedload\n",
        "importFrom(wrappedload, loaded)\nexport(check)\n",
        "check <- function() loaded()\n",
    );
    let output = fixture.path().join("generated-wraproot");
    let result = Command::new(env!("CARGO_BIN_EXE_slinker"))
        .args(["build", "--lib"])
        .arg(&build_library)
        .args(["--output"])
        .arg(&output)
        .arg(&root_source)
        .output()
        .expect("run linked build");
    assert_success(&result, "slinker build wrapped .onLoad fixture");

    let validation = fixture.path().join("validation");
    fs::create_dir(&validation).expect("validation library");
    install_package(&r_home, &output, &validation);
    run_r(
        &r_home,
        &validation,
        "library(wraproot); stopifnot(isTRUE(check()))",
    );
}

#[test]
fn linked_code_keeps_internal_access_to_an_external_package() {
    let r_home = discover_r_home();
    let fixture = tempfile::tempdir().expect("fixture tempdir");
    let build_library = fixture.path().join("build-library");
    fs::create_dir(&build_library).expect("build library");

    let external_source = fixture.path().join("tinyvault");
    write_package(
        &external_source,
        "tinyvault",
        "",
        "export(open_vault)\n",
        "open_vault <- function() 'open'\nsecret <- function(x) paste0('S:', x)\n",
    );
    install_package(&r_home, &external_source, &build_library);

    let linked_source = fixture.path().join("tinyagent");
    write_package(
        &linked_source,
        "tinyagent",
        "Imports: tinyvault (>= 1.0.0)\n",
        "export(reveal)\n",
        "reveal <- function(x) tinyvault:::secret(x)\n",
    );
    install_package(&r_home, &linked_source, &build_library);

    let root_source = fixture.path().join("vaultroot");
    write_package(
        &root_source,
        "vaultroot",
        "Imports: tinyagent\n",
        "importFrom(tinyagent, reveal)\nexport(run)\n",
        "run <- function(x) reveal(x)\n",
    );
    let output = fixture.path().join("generated-vaultroot");
    let result = Command::new(env!("CARGO_BIN_EXE_slinker"))
        .args(["build", "--lib"])
        .arg(&build_library)
        .args(["--external", "tinyvault", "--output"])
        .arg(&output)
        .arg(&root_source)
        .output()
        .expect("run internal-access build");
    assert_success(&result, "slinker build internal-access fixture");

    let validation = fixture.path().join("validation");
    fs::create_dir(&validation).expect("validation library");
    install_package(&r_home, &external_source, &validation);
    install_package(&r_home, &output, &validation);
    run_r(
        &r_home,
        &validation,
        "library(vaultroot); stopifnot(identical(run('x'), 'S:x')); stopifnot(length(find.package('tinyagent', quiet = TRUE)) == 0L)",
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

#[test]
fn blocked_preflight_reports_every_blocker_and_publishes_nothing() {
    let fixture = tempfile::tempdir().expect("fixture tempdir");
    let source = fixture.path().join("blockedroot");
    write_package(
        &source,
        "blockedroot",
        "",
        "export(discover, dispatch)\n",
        "discover <- function(package) requireNamespace(package)\ndispatch <- function(x, generic) UseMethod(generic)\n",
    );
    let output = fixture.path().join("generated-blockedroot");
    let result = Command::new(env!("CARGO_BIN_EXE_slinker"))
        .args(["build", "--output"])
        .arg(&output)
        .arg(&source)
        .output()
        .expect("run blocked build");

    assert!(!result.status.success());
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(stderr.contains("DynamicPackageDiscovery"), "{stderr}");
    assert!(stderr.contains("ObjectSystem"), "{stderr}");
    assert!(!output.exists());
    assert_eq!(
        fs::read_dir(fixture.path())
            .expect("fixture directory")
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().starts_with(".slinker"))
            .count(),
        0
    );
}

#[test]
fn private_environments_and_registrations_survive_linking() {
    let r_home = discover_r_home();
    let fixture = tempfile::tempdir().expect("fixture tempdir");
    let state_code = "counter <- local({\n  n <- 0L\n  function() {\n    n <<- n + 1L\n    n\n  }\n})\nstore <- local({\n  value <- NULL\n  list(get = function() value, set = function(x) value <<- x)\n})\nget_value <- store$get\nset_value <- store$set\nmake <- function() structure(list(), class = 'tinystate')\nformat.tinystate <- function(x, ...) 'formatted tinystate'\nunused <- function() stop('never linked')\n";
    let dependency_source = fixture.path().join("tinystate");
    write_package(
        &dependency_source,
        "tinystate",
        "",
        "export(counter, get_value, set_value, make, unused)\nS3method(format, tinystate)\n",
        state_code,
    );
    let build_library = fixture.path().join("build-library");
    fs::create_dir(&build_library).expect("build library");
    install_package(&r_home, &dependency_source, &build_library);

    let root_source = fixture.path().join("stateroot");
    write_package(
        &root_source,
        "stateroot",
        "Imports: tinystate\n",
        "importFrom(tinystate, counter, get_value, set_value, make)\nexport(run, local_counter)\n",
        "run <- function() {\n  counter()\n  set_value(7)\n  c(counter(), get_value(), format(make()))\n}\nlocal_counter <- local({\n  n <- 10L\n  function() {\n    n <<- n + 1L\n    n\n  }\n})\n",
    );
    let output = fixture.path().join("generated-stateroot");
    let result = Command::new(env!("CARGO_BIN_EXE_slinker"))
        .args(["build", "--lib"])
        .arg(&build_library)
        .arg("--output")
        .arg(&output)
        .arg(&root_source)
        .output()
        .expect("run private environment build");
    assert_success(&result, "slinker build private environment fixture");

    let validation = fixture.path().join("validation");
    fs::create_dir(&validation).expect("validation library");
    install_package(&r_home, &output, &validation);
    run_r(
        &r_home,
        &validation,
        r#"
        library(stateroot)
        stopifnot(identical(run(), c("2", "7", "formatted tinystate")))
        stopifnot(identical(local_counter(), 11L), identical(local_counter(), 12L))
        stopifnot(exists("unused", envir = asNamespace("tinystate"), inherits = FALSE))
        removed <- tryCatch(get("unused", envir = asNamespace("tinystate")), error = conditionMessage)
        stopifnot(grepl("`tinystate::unused` was removed by slinker", removed, fixed = TRUE))
        stopifnot(setequal(getNamespaceExports("tinystate"), c("counter", "get_value", "set_value", "make", "unused")))
        stopifnot(exists(".packageName", envir = asNamespace("tinystate"), inherits = FALSE))
        stopifnot(length(find.package("tinystate", quiet = TRUE)) == 0L)
        "#,
    );
}

#[test]
fn real_pure_r_packages_build_install_and_run() {
    let r_home = discover_r_home();
    let fixture = tempfile::tempdir().expect("fixture tempdir");
    let validation = fixture.path().join("validation");
    fs::create_dir(&validation).expect("validation library");
    for package in ["praise", "pkgconfig"] {
        let output = fixture.path().join(package);
        let result = Command::new(env!("CARGO_BIN_EXE_slinker"))
            .args(["build", "--output"])
            .arg(&output)
            .arg(
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("tests/fixtures")
                    .join(package),
            )
            .output()
            .expect("run slinker build");
        assert_success(&result, package);
        install_package(&r_home, &output, &validation);
    }
    run_r(
        &r_home,
        &validation,
        r#"
        library(praise)
        stopifnot(exists(".slinker_target", envir = asNamespace("praise")$.slinker_runtime, inherits = FALSE))
        parts <- praise:::praise_parts
        stopifnot(praise("${adjective}") %in% parts$adjective)
        exclamation <- praise("${EXCLAMATION}")
        stopifnot(identical(exclamation, toupper(exclamation)))
        stopifnot(tolower(exclamation) %in% parts$exclamation)
        library(pkgconfig)
        configure <- function() {
          set_config(greeting = "hello")
          get_config("greeting")
        }
        stopifnot(identical(configure(), "hello"))
        stopifnot(identical(get_config("missing", fallback = "fallback"), "fallback"))
        "#,
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
