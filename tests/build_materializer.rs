mod common;

use common::{assert_success, discover_r_home, install_package, run_r};
use std::fs;
use std::path::{Path, PathBuf};
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
        "export(bfun, own)\n",
        "state <- new.env(parent = emptyenv())\nstate$loaded <- FALSE\nbfun <- function(x) paste0('B:', x, ':', state$loaded)\nown <- function() identical(asNamespace('tinylinked'), environment(own)) && requireNamespace('tinylinked', quietly = TRUE) && identical(tinylinked::bfun('s'), bfun('s'))\n.onLoad <- function(libname, pkgname) state$loaded <- identical(pkgname, 'tinylinked')\n",
    );
    let build_library = fixture.path().join("build-library");
    fs::create_dir(&build_library).expect("build library");
    install_package(&r_home, &dependency_source, &build_library);

    let root_source = fixture.path().join("linkroot");
    write_package(
        &root_source,
        "linkroot",
        "Imports: tinylinked\n",
        "importFrom(tinylinked, bfun, own)\nexport(afun, qualified, owncheck)\n",
        "afun <- function(x) bfun(x)\nqualified <- function(x) tinylinked::bfun(x)\nowncheck <- function() own()\n",
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

    let original = fixture.path().join("original");
    fs::create_dir(&original).expect("original library");
    install_package(&r_home, &dependency_source, &original);
    install_package(&r_home, &root_source, &original);
    let behavior = "library(linkroot); value <- afun('x'); if (!identical(value, 'B:x:TRUE')) stop(sprintf('unexpected value: %s', value)); stopifnot(identical(qualified('q'), 'B:q:TRUE'), isTRUE(owncheck()))";
    run_r(&r_home, &original, behavior);

    let validation = fixture.path().join("validation");
    fs::create_dir(&validation).expect("validation library");
    install_package(&r_home, &output, &validation);
    let installed = fixture.path().join("installed");
    fs::create_dir(&installed).expect("library with the real Linked package");
    install_package(&r_home, &dependency_source, &installed);
    install_package(&r_home, &output, &installed);
    let private = "stopifnot(!isNamespaceLoaded('tinylinked'), !any(grepl(':', loadedNamespaces(), fixed = TRUE))); withCallingHandlers(invisible(sessionInfo()), warning = function(w) stop(w)); linked <- environment(get('bfun', envir = parent.env(asNamespace('linkroot')))); stopifnot(isNamespace(linked), identical(unname(getNamespaceName(linked)), 'tinylinked'), identical(environmentName(linked), 'tinylinked'))";
    for library in [&validation, &installed] {
        run_r(&r_home, library, &format!("{behavior}; {private}"));
    }
}

#[test]
fn unregistered_lexical_methods_dispatch_from_linked_code_as_in_the_original() {
    let r_home = discover_r_home();
    let fixture = tempfile::tempdir().expect("fixture tempdir");
    let dependency_source = fixture.path().join("lexlinked");
    write_package(
        &dependency_source,
        "lexlinked",
        "Imports: stats\n",
        "export(show_format, show_add, show_median, show_print, show_summary_name)\nS3method(print, registered)\n",
        concat!(
            "show_format <- function(x) format(x)\n",
            "show_add <- function(a, b) a + b\n",
            "show_median <- function(x) stats::median(x)\n",
            "show_print <- function(x) print(x)\n",
            "show_summary_name <- function() 'summary.lexical'\n",
            "format.lexical <- function(x, ...) 'lexical-format'\n",
            "`+.lexical` <- function(e1, e2) 'lexical-plus'\n",
            "median.lexical <- function(x, na.rm = FALSE, ...) 'lexical-median'\n",
            "print.registered <- function(x, ...) cat('registered\n')\n",
            "print.lexical <- function(x, ...) cat('lexical-print\n')\n",
            "summary.lexical <- function(object, ...) 'never dispatched'\n",
        ),
    );
    let build_library = fixture.path().join("build-library");
    fs::create_dir(&build_library).expect("build library");
    install_package(&r_home, &dependency_source, &build_library);

    let root_source = fixture.path().join("lexroot");
    write_package(
        &root_source,
        "lexroot",
        "Imports: lexlinked\n",
        "importFrom(lexlinked, show_format, show_add, show_median, show_print)\nexport(go)\n",
        concat!(
            "go <- function() {\n",
            "  lexical <- structure(1, class = 'lexical')\n",
            "  registered <- structure(1, class = 'registered')\n",
            "  list(\n",
            "    format = show_format(lexical),\n",
            "    plus = show_add(lexical, lexical),\n",
            "    median = show_median(lexical),\n",
            "    lexical_print = utils::capture.output(show_print(lexical)),\n",
            "    registered_print = utils::capture.output(show_print(registered))\n",
            "  )\n",
            "}\n",
        ),
    );
    let output = fixture.path().join("generated-lexroot");
    let result = Command::new(env!("CARGO_BIN_EXE_slinker"))
        .args(["build", "--lib"])
        .arg(&build_library)
        .arg("--output")
        .arg(&output)
        .arg(&root_source)
        .output()
        .expect("run lexical dispatch build");
    assert_success(&result, "slinker build lexical dispatch fixture");

    let behavior = "library(lexroot); stopifnot(identical(go(), list(format = 'lexical-format', plus = 'lexical-plus', median = 'lexical-median', lexical_print = 'lexical-print', registered_print = 'registered')))";
    let original = fixture.path().join("original");
    fs::create_dir(&original).expect("original library");
    install_package(&r_home, &dependency_source, &original);
    install_package(&r_home, &root_source, &original);
    run_r(&r_home, &original, behavior);

    let validation = fixture.path().join("validation");
    fs::create_dir(&validation).expect("validation library");
    install_package(&r_home, &output, &validation);
    let installed = fixture.path().join("installed");
    fs::create_dir(&installed).expect("library with the real Linked package");
    install_package(&r_home, &dependency_source, &installed);
    install_package(&r_home, &output, &installed);
    let demand_driven = "imports <- parent.env(asNamespace('lexroot')); linked <- environment(get('show_format', envir = imports)); stopifnot(is.function(get('format.lexical', envir = linked)), is.function(get('print.lexical', envir = linked))); message <- tryCatch(get('summary.lexical', envir = linked), error = conditionMessage); stopifnot(grepl('`lexlinked::summary.lexical` was removed by slinker', message, fixed = TRUE))";
    for library in [&validation, &installed] {
        run_r(&r_home, library, &format!("{behavior}; {demand_driven}"));
    }
}

struct OptionalFixture {
    dependency: PathBuf,
    optional: PathBuf,
    root: PathBuf,
}

fn optional_fixture(fixture: &Path, suggests: &str, mode_code: &str) -> OptionalFixture {
    let optional = fixture.join("optpkg");
    write_package(
        &optional,
        "optpkg",
        "",
        "export(marker)\n",
        "marker <- function() 'opt-ran'\n",
    );
    let dependency = fixture.join("optdep");
    write_package(&dependency, "optdep", suggests, "export(mode)\n", mode_code);
    let root = fixture.join("optroot");
    write_package(
        &root,
        "optroot",
        "Imports: optdep\n",
        "importFrom(optdep, mode)\nexport(go)\n",
        "go <- function() mode()\n",
    );
    OptionalFixture {
        dependency,
        optional,
        root,
    }
}

fn library_with(r_home: &Path, root: &Path, packages: &[&Path]) -> PathBuf {
    let library = tempfile::Builder::new()
        .tempdir_in(root)
        .expect("library")
        .keep();
    for package in packages {
        install_package(r_home, package, &library);
    }
    library
}

fn build_optional_root(
    fixture: &OptionalFixture,
    library: &Path,
    output: &Path,
    flags: &[&str],
) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_slinker"))
        .args(["build", "--lib"])
        .arg(library)
        .args(flags)
        .arg("--output")
        .arg(output)
        .arg(&fixture.root)
        .output()
        .expect("run optional-package build")
}

#[test]
fn unselected_optional_availability_varies_in_the_original_and_blocks_the_build() {
    let r_home = discover_r_home();
    let scratch = tempfile::tempdir().expect("fixture tempdir");
    let fixture = optional_fixture(
        scratch.path(),
        "Suggests: optpkg\n",
        "mode <- function() if (requireNamespace('optpkg', quietly = TRUE)) 'with-opt' else 'without-opt'\n",
    );
    let absent = library_with(
        &r_home,
        scratch.path(),
        &[&fixture.dependency, &fixture.root],
    );
    let present = library_with(
        &r_home,
        scratch.path(),
        &[&fixture.dependency, &fixture.root, &fixture.optional],
    );
    run_r(
        &r_home,
        &absent,
        "library(optroot); stopifnot(identical(go(), 'without-opt'))",
    );
    run_r(
        &r_home,
        &present,
        "library(optroot); stopifnot(identical(go(), 'with-opt'))",
    );

    for library in [&absent, &present] {
        let output = scratch.path().join("generated-optroot");
        let result = build_optional_root(&fixture, library, &output, &[]);
        let stderr = String::from_utf8_lossy(&result.stderr);
        assert!(!result.status.success(), "{stderr}");
        assert!(stderr.contains("OptionalAvailability"), "{stderr}");
        assert!(
            stderr.contains("unselected optional package `optpkg`"),
            "{stderr}"
        );
        assert!(!output.exists());
    }
}

#[test]
fn external_optional_package_builds_and_matches_the_original() {
    let r_home = discover_r_home();
    let scratch = tempfile::tempdir().expect("fixture tempdir");
    let fixture = optional_fixture(
        scratch.path(),
        "Suggests: optpkg\n",
        "mode <- function() if (requireNamespace('optpkg', quietly = TRUE)) paste('with-opt', optpkg::marker()) else 'without-opt'\n",
    );
    let build = library_with(
        &r_home,
        scratch.path(),
        &[&fixture.dependency, &fixture.optional],
    );
    let original = library_with(
        &r_home,
        scratch.path(),
        &[&fixture.dependency, &fixture.root, &fixture.optional],
    );
    let behavior = "library(optroot); stopifnot(identical(go(), 'with-opt opt-ran'))";
    run_r(&r_home, &original, behavior);

    let external_output = scratch.path().join("generated-external");
    let external = build_optional_root(
        &fixture,
        &build,
        &external_output,
        &["--external", "optpkg"],
    );
    assert_success(
        &external,
        "slinker build with the optional package External",
    );
    let external_library = library_with(
        &r_home,
        scratch.path(),
        &[&fixture.optional, &external_output],
    );
    run_r(&r_home, &external_library, behavior);
}

#[test]
fn unused_suggests_entry_neither_blocks_nor_becomes_a_dependency() {
    let r_home = discover_r_home();
    let scratch = tempfile::tempdir().expect("fixture tempdir");
    let fixture = optional_fixture(
        scratch.path(),
        "Suggests: optpkg\n",
        "mode <- function() 'plain'\n",
    );
    let build = library_with(&r_home, scratch.path(), &[&fixture.dependency]);
    let output = scratch.path().join("generated-optroot");
    let result = build_optional_root(&fixture, &build, &output, &[]);
    assert_success(&result, "slinker build with an unused Suggests entry");
    let description =
        fs::read_to_string(output.join("DESCRIPTION")).expect("generated DESCRIPTION");
    assert!(!description.contains("optpkg"), "{description}");

    let runtime = library_with(&r_home, scratch.path(), &[&output]);
    run_r(
        &r_home,
        &runtime,
        "library(optroot); stopifnot(identical(go(), 'plain'))",
    );
}

#[test]
fn guard_on_a_required_package_that_is_not_imported_runs_its_branch() {
    let r_home = discover_r_home();
    let scratch = tempfile::tempdir().expect("fixture tempdir");
    let required = scratch.path().join("optpkg");
    write_package(
        &required,
        "optpkg",
        "",
        "export(marker)\n",
        "marker <- function() 'opt-ran'\n",
    );
    let fixture = optional_fixture(
        scratch.path(),
        "Imports: optpkg\n",
        "mode <- function() if (requireNamespace('optpkg', quietly = TRUE)) optpkg::marker() else 'without-opt'\n",
    );
    let build = library_with(&r_home, scratch.path(), &[&required, &fixture.dependency]);
    let original = library_with(
        &r_home,
        scratch.path(),
        &[&required, &fixture.dependency, &fixture.root],
    );
    let behavior = "library(optroot); stopifnot(identical(go(), 'opt-ran'))";
    run_r(&r_home, &original, behavior);

    let output = scratch.path().join("generated-optroot");
    let result = build_optional_root(&fixture, &build, &output, &[]);
    assert_success(
        &result,
        "slinker build with a required, unimported guard package",
    );
    let runtime = library_with(&r_home, scratch.path(), &[&output]);
    run_r(&r_home, &runtime, behavior);
}

fn write_dataset_package(r_home: &Path, root: &Path, name: &str, lazy: bool, code: &str) {
    write_package(
        root,
        name,
        &format!("LazyData: {}\n", if lazy { "true" } else { "false" }),
        "export(read_alpha, load_multi)\n",
        code,
    );
    let data = root.join("data");
    fs::create_dir_all(&data).expect("data directory");
    let directory = data.display().to_string().replace('\\', "/");
    run_r(
        r_home,
        "",
        &format!(
            "alpha <- structure(list(a = 1:3, b = letters[1:3]), class = c('tbl_x', 'data.frame'), row.names = 1:3, note = 'hi'); beta <- c(x = 1.5); gamma <- 3; unused_big <- 1:10; save(alpha, file = '{directory}/alpha.rda'); save(beta, gamma, file = '{directory}/multi.rda'); save(unused_big, file = '{directory}/unused_big.rda')"
        ),
    );
}

const DATASET_FUNCTIONS: &str = concat!(
    "read_alpha <- function() dsdata::alpha\n",
    "load_multi <- function(env) {\n",
    "  data(multi, package = 'dsdata', envir = env)\n",
    "  mget(c('beta', 'gamma'), envir = env)\n",
    "}\n",
);

#[test]
fn linked_datasets_behave_as_in_the_original_and_only_reached_ones_are_carried() {
    let r_home = discover_r_home();
    let fixture = tempfile::tempdir().expect("fixture tempdir");
    let dependency = fixture.path().join("dsdata");
    write_dataset_package(&r_home, &dependency, "dsdata", true, DATASET_FUNCTIONS);
    let build_library = fixture.path().join("build-library");
    fs::create_dir(&build_library).expect("build library");
    install_package(&r_home, &dependency, &build_library);

    let root = fixture.path().join("dsroot");
    write_package(
        &root,
        "dsroot",
        "Imports: dsdata\n",
        "importFrom(dsdata, read_alpha, load_multi)\nexport(direct, loaded, via_linked, via_linked_data)\n",
        concat!(
            "direct <- function() dsdata::alpha\n",
            "loaded <- function() {\n",
            "  env <- new.env()\n",
            "  name <- data(multi, package = 'dsdata', envir = env)\n",
            "  list(name, sort(ls(env)), env$beta, env$gamma)\n",
            "}\n",
            "via_linked <- function() read_alpha()\n",
            "via_linked_data <- function() load_multi(new.env())\n",
        ),
    );
    let output = fixture.path().join("generated-dsroot");
    let result = Command::new(env!("CARGO_BIN_EXE_slinker"))
        .args(["build", "--lib"])
        .arg(&build_library)
        .arg("--output")
        .arg(&output)
        .arg(&root)
        .output()
        .expect("run dataset build");
    assert_success(&result, "slinker build dataset fixture");

    let behavior = concat!(
        "library(dsroot); ",
        "expected <- structure(list(a = 1:3, b = letters[1:3]), class = c('tbl_x', 'data.frame'), row.names = 1:3, note = 'hi'); ",
        "stopifnot(identical(direct(), expected), identical(via_linked(), expected), identical(direct(), direct())); ",
        "stopifnot(identical(loaded(), list('multi', c('beta', 'gamma'), c(x = 1.5), 3))); ",
        "stopifnot(identical(via_linked_data(), list(beta = c(x = 1.5), gamma = 3)))",
    );
    let original = fixture.path().join("original");
    fs::create_dir(&original).expect("original library");
    install_package(&r_home, &dependency, &original);
    install_package(&r_home, &root, &original);
    run_r(&r_home, &original, behavior);

    let validation = fixture.path().join("validation");
    fs::create_dir(&validation).expect("validation library");
    install_package(&r_home, &output, &validation);
    let installed = fixture.path().join("installed");
    fs::create_dir(&installed).expect("library with the real Linked package");
    install_package(&r_home, &dependency, &installed);
    install_package(&r_home, &output, &installed);

    let artifact = concat!(
        "directory <- system.file('slinker', 'datalib', 'dsdata', 'data', package = 'dsroot'); ",
        "objects <- new.env(); lazyLoad(file.path(directory, 'Rdata'), envir = objects); ",
        "stopifnot(identical(sort(ls(objects)), c('alpha', 'beta', 'gamma'))); ",
        "stopifnot(identical(names(readRDS(file.path(directory, 'Rdata.rds'))), 'multi')); ",
        "imports <- parent.env(asNamespace('dsroot')); linked <- environment(get('read_alpha', envir = imports)); ",
        "stopifnot(identical(unname(getNamespaceName(linked)), 'dsdata'), identical(getExportedValue(linked, 'alpha'), direct()))",
    );
    run_r(&r_home, &validation, &format!("{behavior}; {artifact}"));
    run_r(
        &r_home,
        &installed,
        &format!("loadNamespace('dsdata'); {behavior}; {artifact}"),
    );
}

#[test]
fn dynamic_or_file_backed_dataset_access_blocks_the_build() {
    let r_home = discover_r_home();
    let fixture = tempfile::tempdir().expect("fixture tempdir");
    for (name, lazy, access, expected) in [
        (
            "dsdata",
            true,
            "pick <- function(name) data(list = name, package = 'dsdata')\n",
            "DynamicLookup",
        ),
        (
            "dsfiles",
            false,
            "pick <- function() data(alpha, package = 'dsfiles')\n",
            "UnsupportedObject",
        ),
    ] {
        let dependency = fixture.path().join(name);
        write_dataset_package(
            &r_home,
            &dependency,
            name,
            lazy,
            &DATASET_FUNCTIONS.replace("dsdata", name),
        );
        let build_library = fixture.path().join(format!("{name}-library"));
        fs::create_dir(&build_library).expect("build library");
        install_package(&r_home, &dependency, &build_library);
        let root = fixture.path().join(format!("{name}-root"));
        write_package(
            &root,
            &format!("{name}root"),
            &format!("Imports: {name}\n"),
            &format!("importFrom({name}, read_alpha)\nexport(pick)\n"),
            access,
        );
        let output = fixture.path().join(format!("generated-{name}"));
        let result = Command::new(env!("CARGO_BIN_EXE_slinker"))
            .args(["build", "--lib"])
            .arg(&build_library)
            .arg("--output")
            .arg(&output)
            .arg(&root)
            .output()
            .expect("run blocked dataset build");
        let stderr = String::from_utf8_lossy(&result.stderr);
        assert!(!result.status.success(), "{name}: {stderr}");
        assert!(stderr.contains(expected), "{name}: {stderr}");
        assert!(!output.exists(), "{name} published a blocked build");
    }
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
fn graph_export_is_identical_across_runs() {
    let r_home = discover_r_home();
    let fixture = tempfile::tempdir().expect("fixture tempdir");
    let source = fixture.path().join("privategraph");
    write_package(
        &source,
        "privategraph",
        "",
        "export(count)\n",
        "counter <- local({\n  n <- 0\n  helper <- function() n + 1\n  function() helper()\n})\ncount <- function() counter()\n",
    );
    let library = fixture.path().join("library");
    fs::create_dir(&library).expect("library");
    install_package(&r_home, &source, &library);
    let graph = |run: &str| {
        let output = Command::new(env!("CARGO_BIN_EXE_slinker"))
            .args(["analyze", "privategraph", "--json", "--lib"])
            .arg(&library)
            .env("SLINKER_CACHE_DIR", fixture.path().join(run))
            .output()
            .expect("run slinker analyze");
        assert_success(&output, "slinker analyze --json");
        output.stdout
    };
    let first = graph("first");
    let second = graph("second");

    assert!(String::from_utf8_lossy(&first).contains("private:"));
    assert!(first == second, "--json output differs between runs");
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
        "library(externalroot); stopifnot(identical(run('x'), 'B:E:x')); stopifnot(!isNamespaceLoaded('tinybridge')); stopifnot(length(find.package('tinybridge', quiet=TRUE)) == 0L); stopifnot(isNamespaceLoaded('tinyexternal'))",
    );

    let installed = fixture.path().join("installed");
    fs::create_dir(&installed).expect("library with the real Linked package");
    install_package(&r_home, &external_source, &installed);
    install_package(&r_home, &linked_source, &installed);
    install_package(&r_home, &output, &installed);
    let coexists = r#"
        linked <- function() environment(get("bfun", envir = parent.env(asNamespace("externalroot"))))
        stopifnot(identical(run("x"), "B:E:x"), identical(tinybridge::bfun("y"), "B:E:y"))
        stopifnot(!identical(linked(), asNamespace("tinybridge")))
        stopifnot(identical(unname(getNamespaceName(linked())), "tinybridge"))
    "#;
    for order in [
        "loadNamespace('tinybridge'); library(externalroot)",
        "library(externalroot); loadNamespace('tinybridge')",
    ] {
        run_r(&r_home, &installed, &format!("{order}\n{coexists}"));
    }
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
    let state_code = "counter <- local({\n  n <- 0L\n  function() {\n    n <<- n + 1L\n    n\n  }\n})\nstore <- local({\n  value <- NULL\n  list(get = function() value, set = function(x) value <<- x)\n})\nget_value <- store$get\nset_value <- store$set\nmake <- function() structure(list(), class = 'tinystate')\nformat.tinystate <- function(x, ...) 'formatted tinystate'\nunused <- function() stop('never linked')\nzzz_unused <- function() stop('never linked')\n";
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
    let installed = fixture.path().join("installed");
    fs::create_dir(&installed).expect("library with the real Linked package");
    install_package(&r_home, &dependency_source, &installed);
    install_package(&r_home, &output, &installed);
    let check = r#"
        library(stateroot)
        stopifnot(identical(run(), c("2", "7", "formatted tinystate")))
        stopifnot(identical(local_counter(), 11L), identical(local_counter(), 12L))
        imports <- parent.env(asNamespace("stateroot"))
        linked <- environment(get("make", envir = imports))
        stopifnot(identical(unname(getNamespaceName(linked)), "tinystate"))
        stopifnot(identical(topenv(environment(get("counter", envir = imports))), linked))
        stopifnot(!isNamespaceLoaded("tinystate") || !identical(linked, asNamespace("tinystate")))
        stopifnot(exists("unused", envir = linked, inherits = FALSE))
        removed <- tryCatch(get("unused", envir = linked), error = conditionMessage)
        stopifnot(grepl("`tinystate::unused` was removed by slinker", removed, fixed = TRUE))
        removed_last <- tryCatch(get("zzz_unused", envir = linked), error = conditionMessage)
        stopifnot(grepl("`tinystate::zzz_unused` was removed by slinker", removed_last, fixed = TRUE))
        stopifnot(setequal(getNamespaceExports(linked), c("counter", "get_value", "set_value", "make", "unused")))
        stopifnot(identical(get(".packageName", envir = linked, inherits = FALSE), "tinystate"))
    "#;
    run_r(
        &r_home,
        &validation,
        &format!("{check}\nstopifnot(length(find.package('tinystate', quiet = TRUE)) == 0L)"),
    );
    run_r(
        &r_home,
        &installed,
        &format!("{check}\nstopifnot(!isNamespaceLoaded('tinystate'))"),
    );
    run_r(
        &r_home,
        &installed,
        &format!("loadNamespace('tinystate')\n{check}"),
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

#[test]
fn payload_closures_reach_their_private_namespace() {
    let r_home = discover_r_home();
    let fixture = tempfile::tempdir().expect("fixture tempdir");
    let dependency_source = fixture.path().join("tinypay");
    write_package(
        &dependency_source,
        "tinypay",
        "",
        "export(wrapped, shout)\n",
        "shout <- function() 'S'\nwrapped <- local({\n  inner <- function() identical(environment(tinypay::shout), topenv()) && identical(asNamespace('tinypay'), topenv())\n  function() inner()\n})\n",
    );
    let build_library = fixture.path().join("build-library");
    fs::create_dir(&build_library).expect("build library");
    install_package(&r_home, &dependency_source, &build_library);
    let root_source = fixture.path().join("payroot");
    write_package(
        &root_source,
        "payroot",
        "Imports: tinypay\n",
        "importFrom(tinypay, wrapped)\nexport(check)\n",
        "check <- function() wrapped()\n",
    );
    let output = fixture.path().join("generated-payroot");
    let result = Command::new(env!("CARGO_BIN_EXE_slinker"))
        .args(["build", "--lib"])
        .arg(&build_library)
        .arg("--output")
        .arg(&output)
        .arg(&root_source)
        .output()
        .expect("run payload closure build");
    assert_success(&result, "slinker build payload closure fixture");

    let behavior = "library(payroot); stopifnot(isTRUE(check()))";
    let original = fixture.path().join("original");
    fs::create_dir(&original).expect("original library");
    install_package(&r_home, &dependency_source, &original);
    install_package(&r_home, &root_source, &original);
    run_r(&r_home, &original, behavior);

    let absent = fixture.path().join("absent");
    fs::create_dir(&absent).expect("library without the real Linked package");
    install_package(&r_home, &output, &absent);
    let installed = fixture.path().join("installed");
    fs::create_dir(&installed).expect("library with the real Linked package");
    install_package(&r_home, &dependency_source, &installed);
    install_package(&r_home, &output, &installed);
    run_r(&r_home, &absent, behavior);
    run_r(&r_home, &installed, behavior);
    run_r(
        &r_home,
        &installed,
        &format!("loadNamespace('tinypay'); {behavior}"),
    );
}

#[test]
fn unresolved_names_continue_to_the_global_environment_as_in_the_original() {
    let r_home = discover_r_home();
    let fixture = tempfile::tempdir().expect("fixture tempdir");
    let dependency_source = fixture.path().join("freename");
    write_package(
        &dependency_source,
        "freename",
        "",
        "export(hooked)\n",
        "hooked <- function() if (exists('user_hook')) user_hook() else 'no hook'\n",
    );
    let build_library = fixture.path().join("build-library");
    fs::create_dir(&build_library).expect("build library");
    install_package(&r_home, &dependency_source, &build_library);
    let root_source = fixture.path().join("freeroot");
    write_package(
        &root_source,
        "freeroot",
        "Imports: freename\n",
        "importFrom(freename, hooked)\nexport(check)\n",
        "check <- function() hooked()\n",
    );
    let output = fixture.path().join("generated-freeroot");
    let result = Command::new(env!("CARGO_BIN_EXE_slinker"))
        .args(["build", "--lib"])
        .arg(&build_library)
        .arg("--output")
        .arg(&output)
        .arg(&root_source)
        .output()
        .expect("run free-name build");
    assert_success(&result, "slinker build free-name fixture");

    let behavior = "library(freeroot); stopifnot(identical(check(), 'no hook')); user_hook <- function() 'global hook'; stopifnot(identical(check(), 'global hook'))";
    let original = fixture.path().join("original");
    fs::create_dir(&original).expect("original library");
    install_package(&r_home, &dependency_source, &original);
    install_package(&r_home, &root_source, &original);
    run_r(&r_home, &original, behavior);

    let absent = fixture.path().join("absent");
    fs::create_dir(&absent).expect("library without the real Linked package");
    install_package(&r_home, &output, &absent);
    let installed = fixture.path().join("installed");
    fs::create_dir(&installed).expect("library with the real Linked package");
    install_package(&r_home, &dependency_source, &installed);
    install_package(&r_home, &output, &installed);
    run_r(&r_home, &absent, behavior);
    run_r(&r_home, &installed, behavior);
    run_r(
        &r_home,
        &installed,
        &format!("loadNamespace('freename'); {behavior}"),
    );
}

#[test]
fn linked_namespace_information_reproduces_exports_and_spec() {
    let r_home = discover_r_home();
    let fixture = tempfile::tempdir().expect("fixture tempdir");
    let dependency_source = fixture.path().join("infopkg");
    write_package(
        &dependency_source,
        "infopkg",
        "",
        "export(described, unused_export)\n",
        "described <- function() c(sort(names(.__NAMESPACE__.$exports)), .__NAMESPACE__.$spec[['name']], asNamespace('infopkg')$.__NAMESPACE__.$spec[['version']])\nunused_export <- function() 1\n",
    );
    let build_library = fixture.path().join("build-library");
    fs::create_dir(&build_library).expect("build library");
    install_package(&r_home, &dependency_source, &build_library);
    let root_source = fixture.path().join("inforoot");
    write_package(
        &root_source,
        "inforoot",
        "Imports: infopkg\n",
        "importFrom(infopkg, described)\nexport(check)\n",
        "check <- function() described()\n",
    );
    let output = fixture.path().join("generated-inforoot");
    let result = Command::new(env!("CARGO_BIN_EXE_slinker"))
        .args(["build", "--lib"])
        .arg(&build_library)
        .arg("--output")
        .arg(&output)
        .arg(&root_source)
        .output()
        .expect("run namespace information build");
    assert_success(&result, "slinker build namespace information fixture");

    let behavior = "library(inforoot); stopifnot(identical(check(), c('described', 'unused_export', 'infopkg', '1.0.0')))";
    let original = fixture.path().join("original");
    fs::create_dir(&original).expect("original library");
    install_package(&r_home, &dependency_source, &original);
    install_package(&r_home, &root_source, &original);
    run_r(&r_home, &original, behavior);

    let absent = fixture.path().join("absent");
    fs::create_dir(&absent).expect("library without the real Linked package");
    install_package(&r_home, &output, &absent);
    let installed = fixture.path().join("installed");
    fs::create_dir(&installed).expect("library with the real Linked package");
    install_package(&r_home, &dependency_source, &installed);
    install_package(&r_home, &output, &installed);
    run_r(&r_home, &absent, behavior);
    run_r(&r_home, &installed, behavior);
    run_r(
        &r_home,
        &installed,
        &format!("loadNamespace('infopkg'); {behavior}"),
    );
}

#[test]
fn payload_bundle_preserves_identity_within_its_namespace() {
    let r_home = discover_r_home();
    let fixture = tempfile::tempdir().expect("fixture tempdir");
    let dependency_source = fixture.path().join("tinybundle");
    write_package(
        &dependency_source,
        "tinybundle",
        "",
        "export(probe)\n",
        r#"shared <- new.env(parent = emptyenv())
cycle <- local({
  self <- new.env(parent = emptyenv())
  self$self <- self
  self
})
boxes <- list(a = shared, b = shared)
tagged <- structure(list(1), class = "tagged", home = shared)
counter <- local({
  count <- 0L
  function() {
    count <<- count + 1L
    count
  }
})
alias <- counter
child <- local({
  parent <- new.env(parent = emptyenv())
  parent$tag <- "parent"
  new.env(parent = parent)
})
tool <- list(ext = tools::file_ext)
probe <- function() c(
  shared = identical(boxes$a, boxes$b) && identical(boxes$a, shared) && identical(attr(tagged, "home"), shared),
  cycle = identical(cycle$self, cycle),
  enclosure = identical(environment(counter), environment(alias)) && counter() == 1L && alias() == 2L,
  parent = identical(parent.env(child)$tag, "parent"),
  attributes = inherits(tagged, "tagged"),
  external = identical(environment(tool$ext), asNamespace("tools"))
)
"#,
    );
    let build_library = fixture.path().join("build-library");
    fs::create_dir(&build_library).expect("build library");
    install_package(&r_home, &dependency_source, &build_library);
    let root_source = fixture.path().join("bundleroot");
    write_package(
        &root_source,
        "bundleroot",
        "Imports: tinybundle\n",
        "importFrom(tinybundle, probe)\nexport(check)\n",
        "check <- function() probe()\n",
    );
    let output = fixture.path().join("generated-bundleroot");
    let result = Command::new(env!("CARGO_BIN_EXE_slinker"))
        .args(["build", "--lib"])
        .arg(&build_library)
        .arg("--output")
        .arg(&output)
        .arg(&root_source)
        .output()
        .expect("run payload bundle build");
    assert_success(&result, "slinker build payload bundle fixture");
    assert!(output.join("inst/slinker/payload/tinybundle.rds").is_file());

    let behavior = "library(bundleroot); result <- check(); if (!all(result)) print(result); stopifnot(all(result))";
    let original = fixture.path().join("original");
    fs::create_dir(&original).expect("original library");
    install_package(&r_home, &dependency_source, &original);
    install_package(&r_home, &root_source, &original);
    run_r(&r_home, &original, behavior);

    let absent = fixture.path().join("absent");
    fs::create_dir(&absent).expect("library without the real Linked package");
    install_package(&r_home, &output, &absent);
    let installed = fixture.path().join("installed");
    fs::create_dir(&installed).expect("library with the real Linked package");
    install_package(&r_home, &dependency_source, &installed);
    install_package(&r_home, &output, &installed);
    run_r(&r_home, &absent, behavior);
    run_r(&r_home, &installed, behavior);
    run_r(
        &r_home,
        &installed,
        &format!("loadNamespace('tinybundle'); {behavior}"),
    );
}

#[test]
fn payload_namespace_reference_hidden_from_analysis_blocks() {
    let r_home = discover_r_home();
    let fixture = tempfile::tempdir().expect("fixture tempdir");
    let hidden_source = fixture.path().join("hiddenbase");
    write_package(
        &hidden_source,
        "hiddenbase",
        "",
        "export(value)\n",
        "value <- 1\n",
    );
    let dependency_source = fixture.path().join("hiddenref");
    write_package(
        &dependency_source,
        "hiddenref",
        "Imports: hiddenbase\n",
        "import(hiddenbase)\nexport(home)\n",
        "model <- local({\n  f <- y ~ x\n  environment(f) <- asNamespace('hiddenbase')\n  f\n})\nhome <- function() environmentName(environment(model))\n",
    );
    let build_library = fixture.path().join("build-library");
    fs::create_dir(&build_library).expect("build library");
    install_package(&r_home, &hidden_source, &build_library);
    install_package(&r_home, &dependency_source, &build_library);
    let root_source = fixture.path().join("hiddenroot");
    write_package(
        &root_source,
        "hiddenroot",
        "Imports: hiddenref\n",
        "importFrom(hiddenref, home)\nexport(check)\n",
        "check <- function() home()\n",
    );
    let original = fixture.path().join("original");
    fs::create_dir(&original).expect("original library");
    install_package(&r_home, &hidden_source, &original);
    install_package(&r_home, &dependency_source, &original);
    install_package(&r_home, &root_source, &original);
    run_r(
        &r_home,
        &original,
        "library(hiddenroot); stopifnot(identical(check(), 'hiddenbase'))",
    );

    let output = fixture.path().join("generated-hiddenroot");
    let result = Command::new(env!("CARGO_BIN_EXE_slinker"))
        .args(["build", "--lib"])
        .arg(&build_library)
        .arg("--output")
        .arg(&output)
        .arg(&root_source)
        .output()
        .expect("run hidden namespace build");
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(!result.status.success(), "{stderr}");
    assert!(
        stderr.contains("payload bundle of `hiddenref` refers to namespace `hiddenbase`"),
        "{stderr}"
    );
    assert!(!output.exists());
}

#[test]
fn name_addressed_queries_answer_for_the_linked_copy() {
    let r_home = discover_r_home();
    let fixture = tempfile::tempdir().expect("fixture tempdir");
    let probe = "probe <- function() {\n  ns <- topenv()\n  c(\n    loaded = isNamespaceLoaded('tinyq'),\n    member = 'tinyq' %in% loadedNamespaces(),\n    exported = identical(environment(getExportedValue('tinyq', 'probe')), ns),\n    fetched = identical(environment(utils::getFromNamespace('probe', 'tinyq')), ns),\n    name = identical(getNamespaceName('tinyq'), c(name = 'tinyq')),\n    version = identical(getNamespaceVersion('tinyq'), c(version = '1.0.0')),\n    described = identical(utils::packageDescription('tinyq')$Version, '1.0.0'),\n    versioned = identical(as.character(utils::packageVersion('tinyq')), '1.0.0')\n  )\n}\n";
    let linked_source = fixture.path().join("tinyq");
    write_package(&linked_source, "tinyq", "", "export(probe)\n", probe);
    let build_library = fixture.path().join("build-library");
    fs::create_dir(&build_library).expect("build library");
    install_package(&r_home, &linked_source, &build_library);
    let newer_source = fixture.path().join("newer").join("tinyq");
    write_package(&newer_source, "tinyq", "", "export(probe)\n", probe);
    let description = fs::read_to_string(newer_source.join("DESCRIPTION")).expect("DESCRIPTION");
    fs::write(
        newer_source.join("DESCRIPTION"),
        description.replace("Version: 1.0.0", "Version: 2.0.0"),
    )
    .expect("newer DESCRIPTION");

    let root_source = fixture.path().join("queryroot");
    write_package(
        &root_source,
        "queryroot",
        "Imports: tinyq\n",
        "importFrom(tinyq, probe)\nexport(check)\n",
        "check <- function() all(probe()) && isNamespaceLoaded('tinyq')\n",
    );
    let output = fixture.path().join("generated-queryroot");
    let result = Command::new(env!("CARGO_BIN_EXE_slinker"))
        .args(["build", "--lib"])
        .arg(&build_library)
        .arg("--output")
        .arg(&output)
        .arg(&root_source)
        .output()
        .expect("run query build");
    assert_success(&result, "slinker build query fixture");

    let behavior = "library(queryroot); result <- get('probe', envir = asNamespace('queryroot'))(); if (!all(result)) print(result); stopifnot(isTRUE(check()))";
    let original = fixture.path().join("original");
    fs::create_dir(&original).expect("original library");
    install_package(&r_home, &linked_source, &original);
    install_package(&r_home, &root_source, &original);
    run_r(&r_home, &original, behavior);

    let absent = fixture.path().join("absent");
    fs::create_dir(&absent).expect("library without the real Linked package");
    install_package(&r_home, &output, &absent);
    let installed = fixture.path().join("installed");
    fs::create_dir(&installed).expect("library with a newer real Linked package");
    install_package(&r_home, &newer_source, &installed);
    install_package(&r_home, &output, &installed);
    run_r(&r_home, &absent, behavior);
    run_r(&r_home, &installed, behavior);
    run_r(
        &r_home,
        &installed,
        &format!("loadNamespace('tinyq'); {behavior}"),
    );
}

#[test]
fn linked_native_library_whose_init_fails_in_the_worker_blocks() {
    let r_home = discover_r_home();
    let fixture = tempfile::tempdir().expect("fixture tempdir");
    let provider_source = fixture.path().join("ccprov");
    write_package(&provider_source, "ccprov", "", "useDynLib(ccprov)\n", "");
    fs::create_dir_all(provider_source.join("src")).expect("src directory");
    fs::write(
        provider_source.join("src/ccprov.c"),
        "#include <R.h>\n#include <Rinternals.h>\n#include <R_ext/Rdynload.h>\nstatic int ccprov_value(void) { return 7; }\nvoid R_init_ccprov(DllInfo *dll) { R_RegisterCCallable(\"ccprov\", \"ccprov_value\", (DL_FUNC) &ccprov_value); }\n",
    )
    .expect("provider C source");
    let linked_source = fixture.path().join("ccuse");
    write_package(
        &linked_source,
        "ccuse",
        "Imports: ccprov\n",
        "import(ccprov)\nuseDynLib(ccuse, .registration = TRUE, .fixes = \"C_\")\nexport(tick)\n",
        "tick <- function() .Call(C_ccuse_tick)\n",
    );
    fs::create_dir_all(linked_source.join("src")).expect("src directory");
    fs::write(
        linked_source.join("src/ccuse.c"),
        "#include <R.h>\n#include <Rinternals.h>\n#include <R_ext/Rdynload.h>\nstatic int (*value)(void) = NULL;\nSEXP ccuse_tick(void) { return Rf_ScalarInteger(value()); }\nstatic const R_CallMethodDef calls[] = {{\"ccuse_tick\", (DL_FUNC) &ccuse_tick, 0}, {NULL, NULL, 0}};\nvoid R_init_ccuse(DllInfo *dll) { R_registerRoutines(dll, NULL, calls, NULL, NULL); value = (int (*)(void)) R_GetCCallable(\"ccprov\", \"ccprov_value\"); }\n",
    )
    .expect("consumer C source");
    let build_library = fixture.path().join("build-library");
    fs::create_dir(&build_library).expect("build library");
    install_package(&r_home, &provider_source, &build_library);
    install_package(&r_home, &linked_source, &build_library);
    run_r(
        &r_home,
        &build_library,
        "stopifnot(identical(ccuse::tick(), 7L))",
    );

    let root_source = fixture.path().join("ccroot");
    write_package(
        &root_source,
        "ccroot",
        "Imports: ccuse\n",
        "importFrom(ccuse, tick)\nexport(check)\n",
        "check <- function() tick()\n",
    );
    let output = fixture.path().join("generated-ccroot");
    let result = Command::new(env!("CARGO_BIN_EXE_slinker"))
        .args(["build", "--lib"])
        .arg(&build_library)
        .arg("--output")
        .arg(&output)
        .arg(&root_source)
        .output()
        .expect("run failing-init build");
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(!result.status.success(), "{stderr}");
    assert!(
        stderr.contains("native component `ccuse` failed to load in the worker")
            && stderr.contains("function 'ccprov_value' not provided by package 'ccprov'"),
        "{stderr}"
    );
    assert!(!output.exists());
}

#[test]
fn linked_native_lookups_by_name_reach_their_own_dll_copy() {
    let r_home = discover_r_home();
    let fixture = tempfile::tempdir().expect("fixture tempdir");
    let linked_source = fixture.path().join("tinyc");
    write_package(
        &linked_source,
        "tinyc",
        "",
        "useDynLib(tinyc, .registration = TRUE, .fixes = \"C_\")\nexport(by_name, by_symbol, same_copy, loaded_libraries)\n",
        "by_name <- function() .Call(\"tinyc_tick\", PACKAGE = \"tinyc\")\nby_symbol <- function() .Call(C_tinyc_tick)\nsame_copy <- function() identical(getNativeSymbolInfo(\"tinyc_tick\", \"tinyc\")$dll[[\"path\"]], C_tinyc_tick$dll[[\"path\"]])\nloaded_libraries <- function() getNamespaceInfo('tinyc', 'dynlibs')\n",
    );
    fs::create_dir_all(linked_source.join("src")).expect("src directory");
    fs::write(
        linked_source.join("src/tinyc.c"),
        "#include <R.h>\n#include <Rinternals.h>\n#include <R_ext/Rdynload.h>\nstatic int count = 0;\nSEXP tinyc_tick(void) { return Rf_ScalarInteger(++count); }\nstatic const R_CallMethodDef calls[] = {{\"tinyc_tick\", (DL_FUNC) &tinyc_tick, 0}, {NULL, NULL, 0}};\nvoid R_init_tinyc(DllInfo *dll) { R_registerRoutines(dll, NULL, calls, NULL, NULL); R_useDynamicSymbols(dll, FALSE); }\n",
    )
    .expect("C source");
    let build_library = fixture.path().join("build-library");
    fs::create_dir(&build_library).expect("build library");
    install_package(&r_home, &linked_source, &build_library);

    let root_source = fixture.path().join("nativeroot");
    write_package(
        &root_source,
        "nativeroot",
        "Imports: tinyc\n",
        "importFrom(tinyc, by_name, by_symbol, same_copy, loaded_libraries)\nexport(ticks, same_copy, loaded_libraries)\n",
        "ticks <- function() c(by_name(), by_symbol(), by_name())\n",
    );
    let output = fixture.path().join("generated-nativeroot");
    let build = |summaries: Option<&Path>| {
        let mut command = Command::new(env!("CARGO_BIN_EXE_slinker"));
        command
            .args(["build", "--lib"])
            .arg(&build_library)
            .arg("--output")
            .arg(&output)
            .arg(&root_source)
            .env_remove("SLINKER_NATIVE_SUMMARIES");
        if let Some(summaries) = summaries {
            command.env("SLINKER_NATIVE_SUMMARIES", summaries);
        }
        command.output().expect("run native build")
    };
    let unaudited = build(None);
    let stderr = String::from_utf8_lossy(&unaudited.stderr);
    assert!(!unaudited.status.success(), "{stderr}");
    assert!(!output.exists());
    let key = "package `tinyc` version `1.0.0` image `";
    let fingerprint = stderr
        .find(key)
        .map(|start| &stderr[start + key.len()..])
        .and_then(|rest| rest.split('`').next())
        .unwrap_or_else(|| panic!("UnknownNativeEffects names the manifest key: {stderr}"));
    let summaries = fixture.path().join("native-summaries.json");
    fs::write(
        &summaries,
        format!(
            r#"{{"schema": 1, "packages": [{{"package": "tinyc", "version": "1.0.0", "image_fingerprint": "{fingerprint}", "components": [{{"component": "tinyc", "safety": "safe"}}]}}]}}"#
        ),
    )
    .expect("audited native summary: tinyc.c makes no R callbacks");
    assert_success(&build(Some(&summaries)), "slinker build native fixture");

    let behavior = "library(nativeroot); stopifnot(identical(ticks(), 1:3), isTRUE(same_copy()), identical(loaded_libraries(), structure('tinyc', names = '')))";
    let original = fixture.path().join("original");
    fs::create_dir(&original).expect("original library");
    install_package(&r_home, &linked_source, &original);
    install_package(&r_home, &root_source, &original);
    run_r(&r_home, &original, behavior);

    let installed = fixture.path().join("installed");
    fs::create_dir(&installed).expect("library with the real Linked package");
    install_package(&r_home, &linked_source, &installed);
    install_package(&r_home, &output, &installed);
    run_r(&r_home, &installed, behavior);
    let tick_real = "for (i in 1:5) tinyc::by_name()";
    run_r(
        &r_home,
        &installed,
        &format!("loadNamespace('tinyc'); {tick_real}; {behavior}"),
    );
    run_r(
        &r_home,
        &installed,
        &format!(
            "library(nativeroot); loadNamespace('tinyc'); {tick_real}; stopifnot(identical(ticks(), 1:3), isTRUE(same_copy()), identical(loaded_libraries(), structure('tinyc', names = '')))"
        ),
    );
}

#[test]
fn s3_registrations_on_linked_generics_reach_the_private_namespace() {
    let r_home = discover_r_home();
    let fixture = tempfile::tempdir().expect("fixture tempdir");
    let linked_source = fixture.path().join("tinygen");
    write_package(
        &linked_source,
        "tinygen",
        "",
        "export(describe, run)\n",
        "describe <- function(x) UseMethod('describe')\nrun <- function(x) describe(x)\n.onLoad <- function(libname, pkgname) registerS3method('describe', 'viaenvir', function(x) 'envir', envir = asNamespace('tinygen'))\n",
    );
    let build_library = fixture.path().join("build-library");
    fs::create_dir(&build_library).expect("build library");
    install_package(&r_home, &linked_source, &build_library);

    let root_source = fixture.path().join("s3root");
    write_package(
        &root_source,
        "s3root",
        "Imports: tinygen\n",
        "importFrom(tinygen, describe, run)\nS3method(describe, eager)\nS3method(tinygen::describe, delayed)\nexport(check)\n",
        "describe.eager <- function(x) 'eager'\ndescribe.delayed <- function(x) 'delayed'\ncheck <- function() vapply(c('eager', 'delayed', 'viaenvir'), function(cls) run(structure(1, class = cls)), '')\n",
    );
    let output = fixture.path().join("generated-s3root");
    let result = Command::new(env!("CARGO_BIN_EXE_slinker"))
        .args(["build", "--lib"])
        .arg(&build_library)
        .arg("--output")
        .arg(&output)
        .arg(&root_source)
        .output()
        .expect("run S3 build");
    assert_success(&result, "slinker build S3 fixture");

    let behavior =
        "library(s3root); stopifnot(identical(unname(check()), c('eager', 'delayed', 'envir')))";
    let original = fixture.path().join("original");
    fs::create_dir(&original).expect("original library");
    install_package(&r_home, &linked_source, &original);
    install_package(&r_home, &root_source, &original);
    run_r(&r_home, &original, behavior);

    let absent = fixture.path().join("absent");
    fs::create_dir(&absent).expect("library without the real Linked package");
    install_package(&r_home, &output, &absent);
    let installed = fixture.path().join("installed");
    fs::create_dir(&installed).expect("library with the real Linked package");
    install_package(&r_home, &linked_source, &installed);
    install_package(&r_home, &output, &installed);
    run_r(&r_home, &absent, behavior);
    run_r(&r_home, &installed, behavior);
    run_r(
        &r_home,
        &installed,
        &format!("loadNamespace('tinygen'); {behavior}"),
    );
    run_r(
        &r_home,
        &installed,
        &format!("{behavior}; loadNamespace('tinygen'); {behavior}"),
    );
}

#[test]
fn root_registrations_resolve_linked_bindings_only_after_activation() {
    let r_home = discover_r_home();
    let fixture = tempfile::tempdir().expect("fixture tempdir");
    let linked_source = fixture.path().join("tinyfmt");
    write_package(
        &linked_source,
        "tinyfmt",
        "",
        "export(fmt, describe)\n",
        "fmt <- function(x, ...) 'linked'\ndescribe <- function(x) UseMethod('describe')\n",
    );
    let build_library = fixture.path().join("build-library");
    fs::create_dir(&build_library).expect("build library");
    install_package(&r_home, &linked_source, &build_library);

    let root_source = fixture.path().join("preroot");
    write_package(
        &root_source,
        "preroot",
        "Imports: tinyfmt\n",
        "S3method(format, foo)\nS3method(describe, bar)\nS3method(format, plain)\nS3method(format, late)\nexport(describe)\n",
        "format.foo <- tinyfmt::fmt\ndescribe <- tinyfmt::describe\ndescribe.bar <- function(x) 'bar'\nformat.plain <- function(x, ...) 'plain'\nformat.late <- function(x, ...) paste0('late:', tinyfmt::fmt(x))\n",
    );
    let output = fixture.path().join("generated-preroot");
    let result = Command::new(env!("CARGO_BIN_EXE_slinker"))
        .args(["build", "--lib"])
        .arg(&build_library)
        .arg("--output")
        .arg(&output)
        .arg(&root_source)
        .output()
        .expect("run pre-bootstrap registration build");
    assert_success(&result, "slinker build pre-bootstrap registration fixture");
    let namespace = fs::read_to_string(output.join("NAMESPACE")).expect("generated NAMESPACE");
    assert!(namespace.contains("S3method(\"format\", \"plain\", \"format.plain\")"));
    assert!(namespace.contains("S3method(\"format\", \"late\", \"format.late\")"));

    let behavior = "withCallingHandlers(library(preroot), warning = function(w) stop(w)); formatted <- vapply(c('foo', 'plain', 'late'), function(cls) format(structure(1, class = cls)), ''); stopifnot(identical(unname(formatted), c('linked', 'plain', 'late:linked')), identical(tryCatch(describe(structure(1, class = 'bar')), error = conditionMessage), \"no applicable method for 'describe' applied to an object of class \\\"bar\\\"\"), exists('describe.bar', envir = asNamespace('preroot')[['.__S3MethodsTable__.']], inherits = FALSE))";
    let original = fixture.path().join("original");
    fs::create_dir(&original).expect("original library");
    install_package(&r_home, &linked_source, &original);
    install_package(&r_home, &root_source, &original);
    run_r(&r_home, &original, behavior);

    let private = format!("{behavior}; stopifnot(!isNamespaceLoaded('tinyfmt'))");
    let absent = fixture.path().join("absent");
    fs::create_dir(&absent).expect("library without the real Linked package");
    install_package(&r_home, &output, &absent);
    let installed = fixture.path().join("installed");
    fs::create_dir(&installed).expect("library with the real Linked package");
    install_package(&r_home, &linked_source, &installed);
    install_package(&r_home, &output, &installed);
    run_r(&r_home, &absent, &private);
    run_r(&r_home, &installed, &private);
    run_r(
        &r_home,
        &installed,
        &format!("loadNamespace('tinyfmt'); {behavior}"),
    );
}

#[test]
fn imports_environments_keep_every_original_import_name() {
    let r_home = discover_r_home();
    let fixture = tempfile::tempdir().expect("fixture tempdir");
    let source_package = fixture.path().join("impsrc");
    write_package(
        &source_package,
        "impsrc",
        "",
        "export(used, unused, reexp, file_ext, gen, gen2)\n",
        "used <- function() 'used'\nunused <- function() 'unused'\nreexp <- function() 'reexp'\nfile_ext <- function(x) 'impsrc'\ngen <- function(x) UseMethod('gen')\ngen2 <- function(x) UseMethod('gen2')\n",
    );
    let middle_package = fixture.path().join("impmid");
    write_package(
        &middle_package,
        "impmid",
        "Imports: impsrc, tools\n",
        "importFrom(tools, file_ext)\nimportFrom(impsrc, used, unused, reexp, file_ext, gen, gen2)\nexport(run, reexp)\nS3method(gen, mid)\nS3method(gen2, mid)\n",
        "run <- function() c(used(), file_ext('a.b'), gen(structure(1, class = 'mid')))\ngen.mid <- function(x) 'mid'\ngen2.mid <- function(x) 'mid2'\n",
    );
    let build_library = fixture.path().join("build-library");
    fs::create_dir(&build_library).expect("build library");
    install_package(&r_home, &source_package, &build_library);
    install_package(&r_home, &middle_package, &build_library);

    let root_source = fixture.path().join("improot");
    write_package(
        &root_source,
        "improot",
        "Imports: impmid\n",
        "importFrom(impmid, run, reexp)\nexport(check)\n",
        "check <- function() run()\n",
    );
    let output = fixture.path().join("generated-improot");
    let result = Command::new(env!("CARGO_BIN_EXE_slinker"))
        .args(["build", "--lib"])
        .arg(&build_library)
        .arg("--output")
        .arg(&output)
        .arg(&root_source)
        .output()
        .expect("run imports environment build");
    assert_success(&result, "slinker build imports environment fixture");

    let behavior = "library(improot); stopifnot(identical(check(), c('used', 'impsrc', 'mid'))); ns <- environment(get('run', envir = asNamespace('improot'))); imports <- parent.env(ns); root_imports <- parent.env(asNamespace('improot')); stopifnot(identical(sort(ls(imports, all.names = TRUE)), sort(c('file_ext', 'gen', 'gen2', 'reexp', 'unused', 'used'))), identical(sort(ls(root_imports, all.names = TRUE)), c('reexp', 'run')), identical(sort(getNamespaceExports(ns)), c('reexp', 'run')), identical(get('used', envir = imports)(), 'used'))";
    let original = fixture.path().join("original");
    fs::create_dir(&original).expect("original library");
    install_package(&r_home, &source_package, &original);
    install_package(&r_home, &middle_package, &original);
    install_package(&r_home, &root_source, &original);
    run_r(
        &r_home,
        &original,
        &format!(
            "{behavior}; stopifnot(identical(get('unused', envir = imports)(), 'unused'), identical(getExportedValue(ns, 'reexp')(), 'reexp'))"
        ),
    );

    let generated = format!(
        "{behavior}; removed <- function(read) tryCatch({{ read(); 'read' }}, error = conditionMessage); stopifnot(identical(removed(function() get('unused', envir = imports)), '`impsrc::unused` was removed by slinker because the build never reached it'), identical(removed(function() getExportedValue(ns, 'reexp')), '`impsrc::reexp` was removed by slinker because the build never reached it'), identical(removed(function() get('reexp', envir = root_imports)), '`impmid::reexp` was removed by slinker because the build never reached it'), !isNamespaceLoaded('impsrc'), !isNamespaceLoaded('impmid'))"
    );
    let absent = fixture.path().join("absent");
    fs::create_dir(&absent).expect("library without the real Linked packages");
    install_package(&r_home, &output, &absent);
    let installed = fixture.path().join("installed");
    fs::create_dir(&installed).expect("library with the real Linked packages");
    install_package(&r_home, &source_package, &installed);
    install_package(&r_home, &middle_package, &installed);
    install_package(&r_home, &output, &installed);
    run_r(&r_home, &absent, &generated);
    run_r(&r_home, &installed, &generated);
    run_r(
        &r_home,
        &installed,
        &format!("loadNamespace('impmid'); {behavior}"),
    );
}

#[test]
fn delayed_registrations_on_suggested_generics_reach_the_real_package() {
    let r_home = discover_r_home();
    let fixture = tempfile::tempdir().expect("fixture tempdir");
    let optional_source = fixture.path().join("tinylate");
    write_package(
        &optional_source,
        "tinylate",
        "",
        "export(render)\n",
        "render <- function(x) UseMethod('render')\n",
    );
    let linked_source = fixture.path().join("tinyopt");
    write_package(
        &linked_source,
        "tinyopt",
        "Suggests: tinylate\n",
        "export(make_opt)\nS3method(tinylate::render, optcls)\n",
        "make_opt <- function() structure(1, class = 'optcls')\nrender.optcls <- function(x) 'opt'\n",
    );
    let build_library = fixture.path().join("build-library");
    fs::create_dir(&build_library).expect("build library");
    install_package(&r_home, &linked_source, &build_library);

    let root_source = fixture.path().join("optroot");
    write_package(
        &root_source,
        "optroot",
        "Imports: tinyopt\nSuggests: tinylate\n",
        "importFrom(tinyopt, make_opt)\nexport(objects)\nS3method(tinylate::render, rootcls)\n",
        "objects <- function() list(make_opt(), structure(1, class = 'rootcls'))\nrender.rootcls <- function(x) 'root'\n",
    );
    let output = fixture.path().join("generated-optroot");
    let result = Command::new(env!("CARGO_BIN_EXE_slinker"))
        .args(["build", "--lib"])
        .arg(&build_library)
        .arg("--output")
        .arg(&output)
        .arg(&root_source)
        .output()
        .expect("run optional registration build");
    assert_success(&result, "slinker build optional registration fixture");

    let rendered =
        "stopifnot(identical(vapply(objects(), tinylate::render, ''), c('opt', 'root')))";
    let behaviors = [
        format!("library(optroot); loadNamespace('tinylate'); {rendered}"),
        format!("loadNamespace('tinylate'); library(optroot); {rendered}"),
    ];
    let original = fixture.path().join("original");
    fs::create_dir(&original).expect("original library");
    install_package(&r_home, &optional_source, &original);
    install_package(&r_home, &linked_source, &original);
    install_package(&r_home, &root_source, &original);
    for behavior in &behaviors {
        run_r(&r_home, &original, behavior);
    }

    let absent = fixture.path().join("absent");
    fs::create_dir(&absent).expect("library without the real Linked package");
    install_package(&r_home, &optional_source, &absent);
    install_package(&r_home, &output, &absent);
    let installed = fixture.path().join("installed");
    fs::create_dir(&installed).expect("library with the real Linked package");
    install_package(&r_home, &optional_source, &installed);
    install_package(&r_home, &linked_source, &installed);
    install_package(&r_home, &output, &installed);
    for behavior in &behaviors {
        run_r(&r_home, &absent, behavior);
        run_r(&r_home, &installed, behavior);
        run_r(
            &r_home,
            &installed,
            &format!("loadNamespace('tinyopt'); {behavior}"),
        );
    }
}

#[test]
fn rlang_package_queries_answer_for_the_linked_copy() {
    let r_home = discover_r_home();
    let located = Command::new(common::r_executable(&r_home))
        .args([
            "--vanilla",
            "--slave",
            "-e",
            "cat(dirname(find.package('rlang')))",
        ])
        .output()
        .expect("locate rlang");
    assert_success(&located, "locate rlang");
    let rlang_library = PathBuf::from(String::from_utf8_lossy(&located.stdout).trim());
    let fixture = tempfile::tempdir().expect("fixture tempdir");
    let linked_source = fixture.path().join("tinyinst");
    write_package(
        &linked_source,
        "tinyinst",
        "Imports: rlang\n",
        "export(checked)\n",
        "checked <- function() {\n  rlang::check_installed('tinyinst', reason = 'to check')\n  rlang::is_installed('tinyinst') && identical(rlang::ns_env('tinyinst'), topenv())\n}\n",
    );
    let build_library = fixture.path().join("build-library");
    fs::create_dir(&build_library).expect("build library");
    install_package(&r_home, &linked_source, &build_library);
    let root_source = fixture.path().join("rlangroot");
    write_package(
        &root_source,
        "rlangroot",
        "Imports: tinyinst, rlang\n",
        "importFrom(tinyinst, checked)\nexport(check)\n",
        "check <- function() checked()\n",
    );
    let output = fixture.path().join("generated-rlangroot");
    let result = Command::new(env!("CARGO_BIN_EXE_slinker"))
        .args(["build", "--external", "rlang", "--lib"])
        .arg(&build_library)
        .arg("--lib")
        .arg(&rlang_library)
        .arg("--output")
        .arg(&output)
        .arg(&root_source)
        .output()
        .expect("run rlang build");
    assert_success(&result, "slinker build rlang fixture");

    let behavior = "library(rlangroot); stopifnot(isTRUE(check()))";
    let libraries = |library: &Path| {
        std::env::join_paths([library, rlang_library.as_path()]).expect("library path")
    };
    let original = fixture.path().join("original");
    fs::create_dir(&original).expect("original library");
    install_package(&r_home, &linked_source, &original);
    install_package(&r_home, &root_source, &original);
    run_r(&r_home, libraries(&original), behavior);

    let absent = fixture.path().join("absent");
    fs::create_dir(&absent).expect("library without the real Linked package");
    install_package(&r_home, &output, &absent);
    let installed = fixture.path().join("installed");
    fs::create_dir(&installed).expect("library with the real Linked package");
    install_package(&r_home, &linked_source, &installed);
    install_package(&r_home, &output, &installed);
    run_r(&r_home, libraries(&absent), behavior);
    run_r(&r_home, libraries(&installed), behavior);
    run_r(
        &r_home,
        libraries(&installed),
        &format!("loadNamespace('tinyinst'); {behavior}"),
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

#[test]
fn linked_namespace_information_reports_the_original_imports_and_s3_methods() {
    let r_home = discover_r_home();
    let fixture = tempfile::tempdir().expect("fixture tempdir");
    let generic_source = fixture.path().join("infogen");
    write_package(
        &generic_source,
        "infogen",
        "",
        "export(gen)\n",
        "gen <- function(x) UseMethod('gen')\n",
    );
    let dependency_source = fixture.path().join("infodep");
    write_package(
        &dependency_source,
        "infodep",
        "Imports: infogen\n",
        "import(infogen)\nimportFrom(stats, setNames, median)\nimportFrom(utils, head)\nS3method(gen, eager)\nS3method(infogen::gen, delayed)\nS3method(print, infodep)\nexport(report)\n",
        "gen.eager <- function(x) 'eager'\ngen.delayed <- function(x) 'delayed'\nprint.infodep <- function(x, ...) invisible(x)\nreport <- function() {\n  gen(structure(1, class = c('eager', 'delayed')))\n  list(imports = getNamespaceInfo('infodep', 'imports'), same = identical(getNamespaceImports('infodep'), getNamespaceInfo('infodep', 'imports')), s3 = getNamespaceInfo('infodep', 'S3methods'), dynlibs = getNamespaceInfo('infodep', 'dynlibs'), lexical = .__NAMESPACE__.$imports)\n}\n",
    );
    let build_library = fixture.path().join("build-library");
    fs::create_dir(&build_library).expect("build library");
    install_package(&r_home, &generic_source, &build_library);
    install_package(&r_home, &dependency_source, &build_library);
    let root_source = fixture.path().join("inforoot");
    write_package(
        &root_source,
        "inforoot",
        "Imports: infodep\n",
        "importFrom(infodep, report)\nexport(check)\n",
        "check <- function() report()\n",
    );
    let output = fixture.path().join("generated-inforoot");
    let result = Command::new(env!("CARGO_BIN_EXE_slinker"))
        .args(["build", "--lib"])
        .arg(&build_library)
        .arg("--output")
        .arg(&output)
        .arg(&root_source)
        .output()
        .expect("run namespace information build");
    assert_success(&result, "slinker build namespace information fixture");

    let expected = fixture.path().join("expected.rds");
    let expected = expected
        .to_string_lossy()
        .replace(std::path::MAIN_SEPARATOR, "/");
    let original = fixture.path().join("original");
    fs::create_dir(&original).expect("original library");
    install_package(&r_home, &generic_source, &original);
    install_package(&r_home, &dependency_source, &original);
    install_package(&r_home, &root_source, &original);
    run_r(
        &r_home,
        &original,
        &format!("library(inforoot); saveRDS(check(), '{expected}')"),
    );

    let behavior = format!(
        "library(inforoot); actual <- check(); expected <- readRDS('{expected}'); stopifnot(isTRUE(actual$same), identical(actual$imports, expected$imports), identical(actual$lexical, expected$imports), identical(actual$dynlibs, expected$dynlibs), identical(actual$s3, expected$s3))"
    );
    let absent = fixture.path().join("absent");
    fs::create_dir(&absent).expect("library without the real Linked packages");
    install_package(&r_home, &output, &absent);
    let installed = fixture.path().join("installed");
    fs::create_dir(&installed).expect("library with the real Linked packages");
    install_package(&r_home, &generic_source, &installed);
    install_package(&r_home, &dependency_source, &installed);
    install_package(&r_home, &output, &installed);
    run_r(&r_home, &absent, &behavior);
    run_r(&r_home, &installed, &behavior);
    run_r(
        &r_home,
        &installed,
        &format!("loadNamespace('infodep'); {behavior}"),
    );
}

#[test]
fn enumerating_a_linked_namespace_blocks_the_build() {
    let r_home = discover_r_home();
    let fixture = tempfile::tempdir().expect("fixture tempdir");
    let dependency_source = fixture.path().join("enumdep");
    write_package(
        &dependency_source,
        "enumdep",
        "",
        "export(kept)\n",
        "kept <- function() 1\nunreached <- function() 2\n",
    );
    let build_library = fixture.path().join("build-library");
    fs::create_dir(&build_library).expect("build library");
    install_package(&r_home, &dependency_source, &build_library);
    let build = |name: &str, code: &str| {
        let root_source = fixture.path().join(name);
        write_package(
            &root_source,
            name,
            "Imports: enumdep\n",
            "importFrom(enumdep, kept)\nexport(f)\n",
            code,
        );
        let output = fixture.path().join(format!("generated-{name}"));
        let result = Command::new(env!("CARGO_BIN_EXE_slinker"))
            .args(["build", "--lib"])
            .arg(&build_library)
            .arg("--output")
            .arg(&output)
            .arg(&root_source)
            .output()
            .expect("run enumeration build");
        (result, output)
    };

    for (name, code) in [
        (
            "aslist",
            "f <- function() { kept(); as.list(asNamespace('enumdep')) }\n",
        ),
        (
            "mgetls",
            "f <- function() { kept(); ns <- asNamespace('enumdep'); mget(ls(ns), ns) }\n",
        ),
        (
            "eapplyns",
            "f <- function() { kept(); eapply(asNamespace('enumdep'), class) }\n",
        ),
    ] {
        let (result, output) = build(name, code);
        let stderr = String::from_utf8_lossy(&result.stderr);
        assert!(!result.status.success(), "{name}: {stderr}");
        assert!(stderr.contains("reads every binding"), "{name}: {stderr}");
        assert!(!output.exists(), "{name}");
    }

    let (result, output) = build(
        "targeted",
        "f <- function() kept() + asNamespace('enumdep')$kept()\n",
    );
    assert_success(&result, "slinker build targeted namespace lookup");
    assert!(output.exists());
}

#[test]
fn syntax_forms_dispatch_to_unregistered_lexical_methods_as_in_the_original() {
    let r_home = discover_r_home();
    let fixture = tempfile::tempdir().expect("fixture tempdir");
    let dependency_source = fixture.path().join("synlinked");
    write_package(
        &dependency_source,
        "synlinked",
        "",
        "export(sub, sub2, dollar, negate, assign_sub, assign_dollar, assign_names)\n",
        concat!(
            "sub <- function(x) x[1]\n",
            "sub2 <- function(x) x[[1]]\n",
            "dollar <- function(x) x$a\n",
            "negate <- function(x) -x\n",
            "assign_sub <- function(x) { x[1] <- 0; x }\n",
            "assign_dollar <- function(x) { x$a <- 0; x }\n",
            "assign_names <- function(x) { names(x) <- 'n'; x }\n",
            "`[.syn` <- function(x, ...) 'sub'\n",
            "`[[.syn` <- function(x, ...) 'sub2'\n",
            "`$.syn` <- function(x, name) 'dollar'\n",
            "`-.syn` <- function(e1, e2) 'negate'\n",
            "`[<-.syn` <- function(x, ..., value) 'assign_sub'\n",
            "`$<-.syn` <- function(x, name, value) 'assign_dollar'\n",
            "`names<-.syn` <- function(x, value) 'assign_names'\n",
            "`[[<-.syn` <- function(x, ..., value) 'never'\n",
        ),
    );
    let build_library = fixture.path().join("build-library");
    fs::create_dir(&build_library).expect("build library");
    install_package(&r_home, &dependency_source, &build_library);
    let root_source = fixture.path().join("synroot");
    write_package(
        &root_source,
        "synroot",
        "Imports: synlinked\n",
        "importFrom(synlinked, sub, sub2, dollar, negate, assign_sub, assign_dollar, assign_names)\nexport(go)\n",
        concat!(
            "go <- function() {\n",
            "  x <- structure(list(a = 1), class = 'syn')\n",
            "  c(sub(x), sub2(x), dollar(x), negate(x), assign_sub(x), assign_dollar(x), assign_names(x))\n",
            "}\n",
        ),
    );
    let output = fixture.path().join("generated-synroot");
    let result = Command::new(env!("CARGO_BIN_EXE_slinker"))
        .args(["build", "--lib"])
        .arg(&build_library)
        .arg("--output")
        .arg(&output)
        .arg(&root_source)
        .output()
        .expect("run syntax dispatch build");
    assert_success(&result, "slinker build syntax dispatch fixture");
    let behavior = "library(synroot); stopifnot(identical(go(), c('sub', 'sub2', 'dollar', 'negate', 'assign_sub', 'assign_dollar', 'assign_names')))";
    let original = fixture.path().join("original");
    fs::create_dir(&original).expect("original library");
    install_package(&r_home, &dependency_source, &original);
    install_package(&r_home, &root_source, &original);
    run_r(&r_home, &original, behavior);
    let validation = fixture.path().join("validation");
    fs::create_dir(&validation).expect("validation library");
    install_package(&r_home, &output, &validation);
    run_r(
        &r_home,
        &validation,
        &format!(
            "{behavior}; linked <- environment(get('sub', envir = parent.env(asNamespace('synroot')))); message <- tryCatch(get('[[<-.syn', envir = linked), error = conditionMessage); stopifnot(grepl('was removed by slinker', message, fixed = TRUE))"
        ),
    );
}
