mod common;

use common::{assert_success, discover_r_home, install_package, install_package_using, run_r};
use std::fs;
use std::path::Path;
use std::process::{Command, Output};

fn package(root: &Path, name: &str, imports: &str, exports: &str, code: &str) {
    fs::create_dir_all(root.join("R")).unwrap();
    fs::write(root.join("DESCRIPTION"), format!(
        "Package: {name}\nVersion: 1.0.0\nTitle: Resolved Call Fixture\nDescription: Tests installed callable assumptions.\nAuthors@R: person('A', 'B', email='a@example.com', role=c('aut','cre'))\nLicense: MIT\n{imports}"
    )).unwrap();
    fs::write(root.join("NAMESPACE"), exports).unwrap();
    fs::write(root.join("R/code.R"), code).unwrap();
}

fn build(source: &Path, output: &Path, library: &Path, cache: &Path, threads: &str) -> Output {
    Command::new(env!("CARGO_BIN_EXE_slinker"))
        .arg("build")
        .arg(source)
        .arg("--output")
        .arg(output)
        .arg("--lib")
        .arg(library)
        .args(["--threads", threads])
        .env("SLINKER_CACHE_DIR", cache)
        .output()
        .unwrap()
}

fn blocked_observation(name: &str, code: &str, oracle: &str, dependency_code: &str) {
    let work = tempfile::tempdir().unwrap();
    let dependency = work.path().join("obligationdep");
    let source = work.path().join(name);
    let original = work.path().join("original");
    fs::create_dir(&original).unwrap();
    package(
        &dependency,
        "obligationdep",
        "",
        "export(entry)\n",
        dependency_code,
    );
    package(
        &source,
        name,
        "Imports: obligationdep\n",
        "export(run)\n",
        code,
    );
    let r_home = discover_r_home();
    install_package(&r_home, &dependency, &original);
    install_package_using(&r_home, &source, &original, &original);
    run_r(&r_home, &original, oracle);
    let output = work.path().join("output");
    let result = build(&source, &output, &original, &work.path().join("cache"), "1");
    assert!(
        !result.status.success(),
        "unestablished observation accepted: {code}"
    );
    assert!(!output.exists());
}

#[test]
fn namespace_members_and_reflective_generics_keep_their_obligations() {
    let work = tempfile::tempdir().unwrap();
    let dependency = work.path().join("memberdep");
    let source = work.path().join("memberroot");
    let original = work.path().join("original");
    let generated = work.path().join("generated");
    fs::create_dir(&original).unwrap();
    fs::create_dir(&generated).unwrap();
    package(
        &dependency,
        "memberdep",
        "",
        "export(entry)\nS3method(gen,a)\nS3method(gen,b)\n",
        "hidden <- 42L\nentry <- function() 1L\ngen <- function(x) UseMethod('gen')\ngen.a <- function(x) 'a'\ngen.b <- function(x) 'b'\n",
    );
    package(
        &source,
        "memberroot",
        "Imports: memberdep\n",
        "export(run)\n",
        "run <- function() { ns <- asNamespace('memberdep'); g <- get('gen', envir=ns, inherits=FALSE); c(as.character(ns$hidden), as.character(getNamespace('memberdep')[['hidden']]), g(structure(1, class='a')), g(structure(1, class='b'))) }\n",
    );
    let r_home = discover_r_home();
    install_package(&r_home, &dependency, &original);
    install_package_using(&r_home, &source, &original, &original);
    let oracle = "stopifnot(identical(memberroot::run(), c('42','42','a','b')))";
    run_r(&r_home, &original, oracle);
    let output = work.path().join("output");
    let result = build(&source, &output, &original, &work.path().join("cache"), "1");
    assert_success(&result, "namespace member and reflective generic build");
    install_package(&r_home, &output, &generated);
    run_r(&r_home, &generated, oracle);
    let installed = std::env::join_paths([&generated, &original]).unwrap();
    run_r(&r_home, &installed, oracle);
    run_r(
        &r_home,
        &installed,
        &format!("loadNamespace('memberdep'); {oracle}"),
    );
}

#[test]
fn reflective_generic_callers_remain_open() {
    let work = tempfile::tempdir().unwrap();
    let dependency = work.path().join("reflectdep");
    let source = work.path().join("reflectroot");
    let original = work.path().join("original");
    let generated = work.path().join("generated");
    fs::create_dir(&original).unwrap();
    fs::create_dir(&generated).unwrap();
    package(
        &dependency,
        "reflectdep",
        "",
        "export(runner)\n",
        "g <- function(x) UseMethod('g')\ng.default <- function(x) 'default'\ng.foo <- function(x) 'foo'\nrunner <- function(x) { declare(slinker(x=s3('foo'))); direct <- g(x); f <- get('g'); c(direct, f(structure(1, class='bar'))) }\ng.bar <- function(x) 'bar'\n",
    );
    package(
        &source,
        "reflectroot",
        "Imports: reflectdep\n",
        "export(run)\n",
        "run <- function() reflectdep::runner(structure(1, class='foo'))\n",
    );
    let r_home = discover_r_home();
    install_package(&r_home, &dependency, &original);
    install_package_using(&r_home, &source, &original, &original);
    let oracle = "stopifnot(identical(reflectroot::run(), c('foo', 'bar')))";
    run_r(&r_home, &original, oracle);
    let output = work.path().join("output");
    let result = build(&source, &output, &original, &work.path().join("cache"), "1");
    assert_success(&result, "reflective generic build");
    install_package(&r_home, &output, &generated);
    run_r(&r_home, &generated, oracle);
    let installed = std::env::join_paths([&generated, &original]).unwrap();
    run_r(&r_home, &installed, oracle);
    run_r(
        &r_home,
        &installed,
        &format!("loadNamespace('reflectdep'); {oracle}"),
    );
}

#[test]
fn retained_primitive_callables_keep_group_methods() {
    let work = tempfile::tempdir().unwrap();
    let dependency = work.path().join("primitivedep");
    let source = work.path().join("primitiveroot");
    let original = work.path().join("original");
    let generated = work.path().join("generated");
    fs::create_dir(&original).unwrap();
    fs::create_dir(&generated).unwrap();
    package(
        &dependency,
        "primitivedep",
        "",
        "export(run)\n",
        "operations <- list(sum)\nSummary.foo <- function(..., na.rm=FALSE) { NextMethod(); 42L }\nrun <- function(x) operations[[1L]](x)\n",
    );
    package(
        &source,
        "primitiveroot",
        "Imports: primitivedep\n",
        "export(run)\n",
        "run <- function() primitivedep::run(structure(1, class='foo'))\n",
    );
    let r_home = discover_r_home();
    install_package(&r_home, &dependency, &original);
    install_package_using(&r_home, &source, &original, &original);
    let oracle = "stopifnot(identical(primitiveroot::run(), 42L))";
    run_r(&r_home, &original, oracle);
    let output = work.path().join("output");
    let result = build(&source, &output, &original, &work.path().join("cache"), "1");
    assert_success(&result, "primitive callable build");
    install_package(&r_home, &output, &generated);
    run_r(&r_home, &generated, oracle);
    let installed = std::env::join_paths([&generated, &original]).unwrap();
    run_r(&r_home, &installed, oracle);
    run_r(
        &r_home,
        &installed,
        &format!("loadNamespace('primitivedep'); {oracle}"),
    );
}

#[test]
fn unknown_external_dispatch_blocks_before_publication() {
    for generic_call in ["UseMethod(nm)", "UseMethod(object='x', gen=nm)"] {
        let work = tempfile::tempdir().unwrap();
        let dependency = work.path().join("computeddep");
        let caller = work.path().join("computedcaller");
        let source = work.path().join("computedroot");
        let original = work.path().join("original");
        fs::create_dir(&original).unwrap();
        package(
            &dependency,
            "computeddep",
            "",
            "export(g)\n",
            &format!("g <- function(x) {{ nm <- 'g'; {generic_call} }}\n"),
        );
        package(
            &caller,
            "computedcaller",
            "Imports: computeddep\n",
            "export(run)\n",
            "g.foo <- function(x) 'expected'\ng.character <- function(x) 'expected'\nrun <- function(x) computeddep::g(x)\n",
        );
        package(
            &source,
            "computedroot",
            "Imports: computedcaller\n",
            "export(run)\n",
            "run <- function() computedcaller::run(structure(1, class='foo'))\n",
        );
        let r_home = discover_r_home();
        install_package(&r_home, &dependency, &original);
        install_package_using(&r_home, &caller, &original, &original);
        install_package_using(&r_home, &source, &original, &original);
        run_r(
            &r_home,
            &original,
            "stopifnot(identical(computedroot::run(), 'expected'))",
        );
        let output = work.path().join("output");
        let result = Command::new(env!("CARGO_BIN_EXE_slinker"))
            .arg("build")
            .arg(&source)
            .arg("--output")
            .arg(&output)
            .arg("--lib")
            .arg(&original)
            .args(["--external", "computeddep"])
            .env("SLINKER_CACHE_DIR", work.path().join("cache"))
            .output()
            .unwrap();
        assert!(
            !result.status.success(),
            "unknown External dispatch was accepted"
        );
        assert!(!output.exists());
        assert!(
            String::from_utf8_lossy(&result.stderr)
                .contains("could not establish the callee's S3 dispatch generics")
        );
    }
}

#[test]
fn replaced_platform_callable_blocks_specialization() {
    blocked_observation(
        "mutatedroot",
        "run <- function() { unlockBinding('packageVersion', asNamespace('utils')); assign('packageVersion', function(pkg, lib.loc=NULL) package_version('9.9.9'), envir=asNamespace('utils')); lockBinding('packageVersion', asNamespace('utils')); as.character(utils::packageVersion('obligationdep')) }\n",
        "stopifnot(identical(mutatedroot::run(), '9.9.9'))",
        "entry <- function() 42L\n",
    );
}

#[test]
fn raw_namespace_registry_observations_block() {
    blocked_observation(
        "registryroot",
        "run <- function() { obligationdep::entry(); 'obligationdep' %in% grep('obligation', loadedNamespaces(), value=TRUE) }\n",
        "stopifnot(isTRUE(registryroot::run()))",
        "entry <- function() 42L\n",
    );
}

#[test]
fn root_namespace_enumeration_and_support_reads_block() {
    for (name, code, oracle) in [
        (
            "namesroot",
            "run <- function() ls(environment(run), all.names=TRUE)\n",
            "stopifnot(!any(grepl('slinker', namesroot::run(), fixed=TRUE)))",
        ),
        (
            "supportroot",
            "run <- function() tryCatch(get('.slinker_runtime', envir=environment(run), inherits=FALSE), error=function(e) 42L)\n",
            "stopifnot(identical(supportroot::run(), 42L))",
        ),
    ] {
        blocked_observation(name, code, oracle, "entry <- function() 42L\n");
    }
}

#[test]
fn namespace_enumeration_through_closure_and_partial_arguments_blocks() {
    for (name, code) in [
        (
            "closureroot",
            "run <- function() { obligationdep::entry(); length(as.list(environment(run), all.names=TRUE)) }\n",
        ),
        (
            "partialenvroot",
            "run <- function() length(eapply(en=asNamespace('obligationdep'), FUN=identity, all.names=TRUE))\n",
        ),
    ] {
        blocked_observation(
            name,
            code,
            &format!("stopifnot({name}::run() > 0L)"),
            "hidden <- 42L\nentry <- function() 1L\n",
        );
    }
}

#[test]
fn delayed_load_failure_is_not_moved_to_root_loading() {
    blocked_observation(
        "loadfailure",
        "run <- function() requireNamespace('obligationdep', quietly=TRUE)\n",
        "stopifnot(identical(loadfailure::run(), FALSE))",
        "entry <- function() 42L\n.onLoad <- function(libname,pkgname) stop('load failure')\n",
    );
}

#[test]
fn an_unexported_qualified_lookup_blocks_before_publication() {
    let work = tempfile::tempdir().unwrap();
    let dependency = work.path().join("exportdep");
    let source = work.path().join("exportroot");
    let original = work.path().join("original");
    fs::create_dir(&original).unwrap();
    package(
        &dependency,
        "exportdep",
        "",
        "export(run)\n",
        "hidden <- function() 7L\nrun <- function() 1L\n",
    );
    package(
        &source,
        "exportroot",
        "Imports: exportdep\n",
        "export(run)\n",
        "run <- function() tryCatch(exportdep::hidden(), error=function(e) 42L)\n",
    );
    let r_home = discover_r_home();
    install_package(&r_home, &dependency, &original);
    install_package_using(&r_home, &source, &original, &original);
    run_r(
        &r_home,
        &original,
        "stopifnot(identical(exportroot::run(), 42L))",
    );
    let output = work.path().join("output");
    let result = build(&source, &output, &original, &work.path().join("cache"), "1");
    assert!(!result.status.success(), "unexported lookup was accepted");
    assert!(String::from_utf8_lossy(&result.stderr).contains("exported"));
    assert!(!output.exists());
}

#[test]
fn external_requirements_include_depends_constraints() {
    let work = tempfile::tempdir().unwrap();
    let dependency = work.path().join("requireddep");
    let source = work.path().join("requiredroot");
    let original = work.path().join("original");
    fs::create_dir(&original).unwrap();
    package(
        &dependency,
        "requireddep",
        "",
        "export(run)\n",
        "run <- function() 42L\n",
    );
    package(
        &source,
        "requiredroot",
        "Depends: requireddep (>= 1.0.0)\n",
        "export(run)\n",
        "run <- function() requireddep::run()\n",
    );
    let r_home = discover_r_home();
    install_package(&r_home, &dependency, &original);
    install_package_using(&r_home, &source, &original, &original);
    run_r(
        &r_home,
        &original,
        "stopifnot(identical(requiredroot::run(), 42L))",
    );
    let output = work.path().join("output");
    let result = Command::new(env!("CARGO_BIN_EXE_slinker"))
        .arg("build")
        .arg(&source)
        .arg("--output")
        .arg(&output)
        .arg("--lib")
        .arg(&original)
        .args(["--external", "requireddep"])
        .env("SLINKER_CACHE_DIR", work.path().join("cache"))
        .output()
        .unwrap();
    assert_success(&result, "Depends requirement build");
    let description = fs::read_to_string(output.join("DESCRIPTION")).unwrap();
    assert!(
        description.contains("requireddep (>= 1.0.0)"),
        "{description}"
    );
}

#[test]
fn namespace_replacement_does_not_erase_control_argument_evaluation() {
    let work = tempfile::tempdir().unwrap();
    let dependency = work.path().join("controldep");
    let source = work.path().join("controlroot");
    let original = work.path().join("original");
    fs::create_dir(&original).unwrap();
    package(
        &dependency,
        "controldep",
        "",
        "export(run)\n",
        "run <- function() 1L\n",
    );
    package(
        &source,
        "controlroot",
        "Imports: controldep\n",
        "export(run)\n",
        "run <- function() { controldep::run(); tryCatch(asNamespace('controldep', base.OK=stop('evaluated control')), error=function(e) 42L) }\n",
    );
    let r_home = discover_r_home();
    install_package(&r_home, &dependency, &original);
    install_package_using(&r_home, &source, &original, &original);
    run_r(
        &r_home,
        &original,
        "stopifnot(identical(controlroot::run(), 42L))",
    );
    let output = work.path().join("output");
    let result = build(&source, &output, &original, &work.path().join("cache"), "1");
    assert!(
        !result.status.success(),
        "control argument evaluation was erased"
    );
    assert!(String::from_utf8_lossy(&result.stderr).contains("arguments"));
    assert!(!output.exists());
}

#[test]
fn replacing_an_installed_stop_helper_preserves_namespace_fallthrough() {
    let work = tempfile::tempdir().unwrap();
    let dependency = work.path().join("exitdep");
    let source = work.path().join("exitroot");
    let original = work.path().join("original");
    let generated = work.path().join("generated");
    fs::create_dir(&original).unwrap();
    fs::create_dir(&generated).unwrap();
    package(
        &dependency,
        "exitdep",
        "",
        "export(main, aaa_stop)\n",
        "aaa_stop <- function() stop('expected')\nfallback <- function() 42L\nmain <- function(flag) { if (flag) aaa_stop() else fallback <- function() 1L; fallback() }\n",
    );
    package(
        &source,
        "exitroot",
        "Imports: exitdep\n",
        "export(run, probe)\n",
        "probe <- function() exitdep::aaa_stop()\nrun <- function() { utils::assignInNamespace('aaa_stop', function() NULL, ns='exitdep'); exitdep::main(TRUE) }\n",
    );
    let r_home = discover_r_home();
    install_package(&r_home, &dependency, &original);
    install_package_using(&r_home, &source, &original, &original);
    let oracle = "stopifnot(identical(exitroot::run(), 42L))";
    run_r(&r_home, &original, oracle);
    let output = work.path().join("output");
    let result = Command::new(env!("CARGO_BIN_EXE_slinker"))
        .arg("build")
        .arg(&source)
        .arg("--output")
        .arg(&output)
        .arg("--lib")
        .arg(&original)
        .args(["--threads", "1"])
        .env("SLINKER_CACHE_DIR", work.path().join("cache"))
        .output()
        .unwrap();
    assert_success(&result, "mutable helper build");
    install_package(&r_home, &output, &generated);
    run_r(&r_home, &generated, oracle);
    let installed = std::env::join_paths([&generated, &original]).unwrap();
    run_r(&r_home, &installed, oracle);
    run_r(
        &r_home,
        &installed,
        &format!("loadNamespace('exitdep'); {oracle}"),
    );
}

#[test]
fn defaulted_resource_lookup_is_independent_of_helper_name_and_discovery_order() {
    let work = tempfile::tempdir().unwrap();
    let dependency = work.path().join("resourceorderdep");
    let source = work.path().join("resourceorderroot");
    let original = work.path().join("original");
    fs::create_dir(&original).unwrap();
    package(
        &dependency,
        "resourceorderdep",
        "",
        "export(runner)\n",
        "runner <- function(x) x\n",
    );
    let r_home = discover_r_home();
    install_package(&r_home, &dependency, &original);
    for helper in ["z_path", "A_path"] {
        package(
            &source,
            "resourceorderroot",
            "Imports: resourceorderdep\n",
            "export(run)\n",
            &format!(
                "{helper} <- function(package = 'resourceorderdep') system.file('DESCRIPTION', package = package)\nrun <- function() {{ resourceorderdep::runner(1); nzchar({helper}()) }}\n"
            ),
        );
        install_package_using(&r_home, &source, &original, &original);
        let oracle = "stopifnot(identical(resourceorderroot::run(), TRUE))";
        run_r(&r_home, &original, oracle);
        for threads in ["1", "2", "5"] {
            let case = work.path().join(format!("{helper}-{threads}"));
            let generated = case.join("library");
            fs::create_dir_all(&generated).unwrap();
            let output = case.join("output");
            for _ in 0..2 {
                let result = Command::new(env!("CARGO_BIN_EXE_slinker"))
                    .arg("build")
                    .arg(&source)
                    .arg("--output")
                    .arg(&output)
                    .arg("--lib")
                    .arg(&original)
                    .args(["--threads", threads])
                    .env("SLINKER_CACHE_DIR", case.join("cache"))
                    .output()
                    .unwrap();
                assert_success(&result, "defaulted resource build");
                install_package(&r_home, &output, &generated);
                run_r(&r_home, &generated, oracle);
            }
            let installed = std::env::join_paths([&generated, &original]).unwrap();
            run_r(&r_home, &installed, oracle);
            run_r(
                &r_home,
                &installed,
                &format!("loadNamespace('resourceorderdep'); {oracle}"),
            );
        }
    }
}

#[test]
fn partial_matching_selects_the_same_s3_method_as_r() {
    let work = tempfile::tempdir().unwrap();
    let dependency = work.path().join("matchingdep");
    let source = work.path().join("matchingroot");
    let original = work.path().join("original");
    let generated = work.path().join("generated");
    fs::create_dir(&original).unwrap();
    fs::create_dir(&generated).unwrap();
    package(
        &dependency,
        "matchingdep",
        "",
        "export(run)\nS3method(gen, a)\nS3method(gen, b)\n",
        "gen <- function(object, other) UseMethod(obj=object, gen='gen')\ngen.a <- function(object, other) 'a'\ngen.b <- function(object, other) 'b'\nrun <- function(x, y) { declare(slinker(x=s3('a'), y=s3('b'))); gen(ob=y, x) }\n",
    );
    package(
        &source,
        "matchingroot",
        "Imports: matchingdep\n",
        "export(run)\n",
        "run <- function() matchingdep::run(structure(1, class='a'), structure(2, class='b'))\n",
    );
    let r_home = discover_r_home();
    install_package(&r_home, &dependency, &original);
    install_package_using(&r_home, &source, &original, &original);
    let oracle = "stopifnot(identical(matchingroot::run(), 'b'))";
    run_r(&r_home, &original, oracle);
    let output = work.path().join("output");
    let result = Command::new(env!("CARGO_BIN_EXE_slinker"))
        .arg("build")
        .arg(&source)
        .arg("--output")
        .arg(&output)
        .arg("--lib")
        .arg(&original)
        .args(["--threads", "1"])
        .env("SLINKER_CACHE_DIR", work.path().join("cache"))
        .output()
        .unwrap();
    assert_success(&result, "partial matching build");
    install_package(&r_home, &output, &generated);
    run_r(&r_home, &generated, oracle);
    let installed = std::env::join_paths([&generated, &original]).unwrap();
    run_r(&r_home, &installed, oracle);
    run_r(
        &r_home,
        &installed,
        &format!("loadNamespace('matchingdep'); {oracle}"),
    );
}

#[test]
fn namespace_replacement_preserves_invisible_success() {
    let work = tempfile::tempdir().unwrap();
    let dependency = work.path().join("visibilitydep");
    let source = work.path().join("visibilityroot");
    let original = work.path().join("original");
    let generated = work.path().join("generated");
    fs::create_dir(&original).unwrap();
    fs::create_dir(&generated).unwrap();
    package(
        &dependency,
        "visibilitydep",
        "",
        "export(run)\n",
        "run <- function() 1L\n",
    );
    package(
        &source,
        "visibilityroot",
        "Imports: visibilitydep\n",
        "export(run)\n",
        "run <- function() requireNamespace('visibilitydep', quietly=TRUE)\n",
    );
    let r_home = discover_r_home();
    install_package(&r_home, &dependency, &original);
    install_package_using(&r_home, &source, &original, &original);
    let oracle =
        "stopifnot(identical(withVisible(visibilityroot::run()), list(value=TRUE, visible=FALSE)))";
    run_r(&r_home, &original, oracle);
    let output = work.path().join("output");
    assert_success(
        &build(&source, &output, &original, &work.path().join("cache"), "1"),
        "visibility build",
    );
    install_package(&r_home, &output, &generated);
    run_r(&r_home, &generated, oracle);
    let installed = std::env::join_paths([&generated, &original]).unwrap();
    run_r(&r_home, &installed, oracle);
    run_r(
        &r_home,
        &installed,
        &format!("loadNamespace('visibilitydep'); {oracle}"),
    );
}

#[test]
fn namespace_replacement_does_not_erase_duplicate_argument_errors() {
    let work = tempfile::tempdir().unwrap();
    let dependency = work.path().join("argumentdep");
    let source = work.path().join("argumentroot");
    let original = work.path().join("original");
    fs::create_dir(&original).unwrap();
    package(
        &dependency,
        "argumentdep",
        "",
        "export(run)\n",
        "run <- function() 1L\n",
    );
    package(
        &source,
        "argumentroot",
        "Imports: argumentdep\n",
        "export(run)\n",
        "run <- function() requireNamespace('argumentdep', quietly=TRUE, quietly=TRUE)\n",
    );
    let r_home = discover_r_home();
    install_package(&r_home, &dependency, &original);
    install_package_using(&r_home, &source, &original, &original);
    run_r(
        &r_home,
        &original,
        "stopifnot(grepl('matched by multiple', tryCatch(argumentroot::run(), error=conditionMessage), fixed=TRUE))",
    );
    let output = work.path().join("output");
    let result = build(&source, &output, &original, &work.path().join("cache"), "1");
    assert!(
        !result.status.success(),
        "duplicate arguments were silently replaced: {}",
        String::from_utf8_lossy(&result.stdout)
    );
    assert!(!output.exists());
}

#[test]
fn dispatched_and_does_not_prune_a_true_branch() {
    let work = tempfile::tempdir().unwrap();
    let dependency = work.path().join("guarddep");
    let source = work.path().join("guardroot");
    let original = work.path().join("original");
    let generated = work.path().join("generated");
    fs::create_dir(&original).unwrap();
    fs::create_dir(&generated).unwrap();
    package(
        &dependency,
        "guarddep",
        "",
        "export(hidden)\n",
        "hidden <- function() 42L\n",
    );
    package(
        &source,
        "guardroot",
        "Imports: guarddep\n",
        "export(run)\nS3method(Ops, flag)\n",
        "Ops.flag <- function(...) TRUE\nrun <- function() { flag <- structure(FALSE, class='flag'); if (flag & requireNamespace('slinkerabsentguardfixture', quietly=TRUE)) guarddep::hidden() else 1L }\n",
    );
    let r_home = discover_r_home();
    install_package(&r_home, &dependency, &original);
    install_package_using(&r_home, &source, &original, &original);
    let oracle = "stopifnot(identical(guardroot::run(), 42L))";
    run_r(&r_home, &original, oracle);
    let output = work.path().join("output");
    assert_success(
        &build(&source, &output, &original, &work.path().join("cache"), "1"),
        "dispatched guard build",
    );
    install_package(&r_home, &output, &generated);
    run_r(&r_home, &generated, oracle);
    let installed = std::env::join_paths([&generated, &original]).unwrap();
    run_r(&r_home, &installed, oracle);
    run_r(
        &r_home,
        &installed,
        &format!("loadNamespace('guarddep'); {oracle}"),
    );
}

#[test]
fn shadowed_control_forms_block_before_publication() {
    let work = tempfile::tempdir().unwrap();
    let dependency = work.path().join("controldep");
    let original = work.path().join("original");
    fs::create_dir(&original).unwrap();
    package(
        &dependency,
        "controldep",
        "",
        "export(hidden)\n",
        "hidden <- function() 42L\n",
    );
    let r_home = discover_r_home();
    install_package(&r_home, &dependency, &original);
    for (name, code) in [
        (
            "if",
            "`if` <- function(condition, yes, no) yes\nrun <- function() if (requireNamespace('slinkerabsentguardfixture', quietly=TRUE)) controldep::hidden() else 1L\n",
        ),
        (
            "return",
            "`return` <- function(...) NULL\nrun <- function() { return(1L); controldep::hidden() }\n",
        ),
    ] {
        let source = work.path().join("controlroot");
        package(
            &source,
            "controlroot",
            "Imports: controldep\n",
            "export(run)\n",
            code,
        );
        install_package_using(&r_home, &source, &original, &original);
        run_r(
            &r_home,
            &original,
            "stopifnot(identical(controlroot::run(), 42L))",
        );
        let output = work.path().join(format!("output-{name}"));
        let result = build(
            &source,
            &output,
            &original,
            &work.path().join(format!("cache-{name}")),
            "1",
        );
        assert!(!result.status.success(), "shadowed {name} was accepted");
        assert!(String::from_utf8_lossy(&result.stderr).contains("shadowed"));
        assert!(!output.exists());
    }
}

#[test]
fn a_local_eval_does_not_execute_its_quoted_argument() {
    let work = tempfile::tempdir().unwrap();
    let source = work.path().join("localevalroot");
    let original = work.path().join("original");
    let generated = work.path().join("generated");
    fs::create_dir(&original).unwrap();
    fs::create_dir(&generated).unwrap();
    package(
        &source,
        "localevalroot",
        "",
        "export(run)\n",
        "run <- function() { eval <- function(expr) 42L; eval(quote(slinkerunavailablequotefixture::hidden())) }\n",
    );
    let r_home = discover_r_home();
    install_package(&r_home, &source, &original);
    let oracle = "stopifnot(identical(localevalroot::run(), 42L))";
    run_r(&r_home, &original, oracle);
    let output = work.path().join("output");
    assert_success(
        &build(&source, &output, &original, &work.path().join("cache"), "1"),
        "local eval build",
    );
    install_package(&r_home, &output, &generated);
    run_r(&r_home, &generated, oracle);
}

#[test]
fn base_eval_with_unestablished_code_blocks_before_publication() {
    let work = tempfile::tempdir().unwrap();
    let source = work.path().join("baseevalroot");
    let original = work.path().join("original");
    fs::create_dir(&original).unwrap();
    package(
        &source,
        "baseevalroot",
        "",
        "export(run)\n",
        "run <- function() eval(quote(42L))\n",
    );
    let r_home = discover_r_home();
    install_package(&r_home, &source, &original);
    run_r(
        &r_home,
        &original,
        "stopifnot(identical(baseevalroot::run(), 42L))",
    );
    let output = work.path().join("output");
    let result = build(&source, &output, &original, &work.path().join("cache"), "1");
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("executable code and environment"));
    assert!(!output.exists());
}

#[test]
fn unused_effective_imports_do_not_silently_drop_unreplayable_effects() {
    let work = tempfile::tempdir().unwrap();
    let source = work.path().join("importorderroot");
    let original = work.path().join("original");
    let generated = work.path().join("generated");
    fs::create_dir(&original).unwrap();
    fs::create_dir(&generated).unwrap();
    let r_home = discover_r_home();
    for (name, marker) in [("zzzimportdep", "zzz"), ("aaaimportdep", "aaa")] {
        let dependency = work.path().join(name);
        package(
            &dependency,
            name,
            "",
            "export(unused)\n",
            &format!(
                "unused <- function() NULL\n.onLoad <- function(...) options(tracke.import.order=paste0(getOption('tracke.import.order',''), '{marker}'))\n"
            ),
        );
        install_package(&r_home, &dependency, &original);
    }
    package(
        &source,
        "importorderroot",
        "Imports: zzzimportdep, aaaimportdep\n",
        "import(zzzimportdep)\nimport(aaaimportdep)\nexport(run)\n",
        "run <- function() getOption('tracke.import.order')\n",
    );
    install_package_using(&r_home, &source, &original, &original);
    let oracle = "stopifnot(identical(importorderroot::run(), 'zzzaaa'))";
    run_r(&r_home, &original, oracle);
    let output = work.path().join("output");
    let result = build(&source, &output, &original, &work.path().join("cache"), "1");
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("shared process state"));
    assert!(!output.exists());
}

#[test]
fn effective_import_initialization_preserves_namespace_dependencies() {
    let work = tempfile::tempdir().unwrap();
    let source = work.path().join("localimportroot");
    let first = work.path().join("zzlocaldep");
    let second = work.path().join("aalocaldep");
    let original = work.path().join("original");
    let generated = work.path().join("generated");
    fs::create_dir(&original).unwrap();
    fs::create_dir(&generated).unwrap();
    package(
        &first,
        "zzlocaldep",
        "",
        "export(peek_first)\n",
        ".state <- NULL\n.onLoad <- function(...) .state <<- 'zzz'\npeek_first <- function() .state\n",
    );
    package(
        &second,
        "aalocaldep",
        "Imports: zzlocaldep\n",
        "importFrom(zzlocaldep, peek_first)\nexport(peek_second)\n",
        ".state <- NULL\n.onLoad <- function(...) .state <<- paste0(peek_first(), 'aaa')\npeek_second <- function() .state\n",
    );
    package(
        &source,
        "localimportroot",
        "Imports: zzlocaldep, aalocaldep\n",
        "import(zzlocaldep)\nimport(aalocaldep)\nexport(run)\n",
        "run <- function() peek_second()\n",
    );
    let r_home = discover_r_home();
    install_package(&r_home, &first, &original);
    install_package_using(&r_home, &second, &original, &original);
    install_package_using(&r_home, &source, &original, &original);
    let oracle = "stopifnot(identical(localimportroot::run(), 'zzzaaa'))";
    run_r(&r_home, &original, oracle);
    let output = work.path().join("output");
    assert_success(
        &build(&source, &output, &original, &work.path().join("cache"), "1"),
        "namespace initialization build",
    );
    install_package(&r_home, &output, &generated);
    run_r(&r_home, &generated, oracle);
    let installed = std::env::join_paths([&generated, &original]).unwrap();
    run_r(&r_home, &installed, oracle);
    run_r(
        &r_home,
        &installed,
        &format!("loadNamespace('zzlocaldep'); loadNamespace('aalocaldep'); {oracle}"),
    );
}

#[test]
fn a_linked_namespace_is_loaded_when_its_own_code_runs() {
    let work = tempfile::tempdir().unwrap();
    let dependency = work.path().join("selfloadeddep");
    let source = work.path().join("selfloadedroot");
    let original = work.path().join("original");
    let generated = work.path().join("generated");
    fs::create_dir(&original).unwrap();
    fs::create_dir(&generated).unwrap();
    package(
        &dependency,
        "selfloadeddep",
        "",
        "export(run)\n",
        "hidden <- function() 42L\nrun <- function() if (isNamespaceLoaded('selfloadeddep')) hidden() else 1L\n",
    );
    package(
        &source,
        "selfloadedroot",
        "Imports: selfloadeddep\n",
        "export(run)\n",
        "run <- function() selfloadeddep::run()\n",
    );
    let r_home = discover_r_home();
    install_package(&r_home, &dependency, &original);
    install_package_using(&r_home, &source, &original, &original);
    let oracle = "stopifnot(identical(selfloadedroot::run(), 42L))";
    run_r(&r_home, &original, oracle);
    let output = work.path().join("output");
    assert_success(
        &build(&source, &output, &original, &work.path().join("cache"), "1"),
        "self loaded build",
    );
    install_package(&r_home, &output, &generated);
    run_r(&r_home, &generated, oracle);
    let installed = std::env::join_paths([&generated, &original]).unwrap();
    run_r(&r_home, &installed, oracle);
    run_r(
        &r_home,
        &installed,
        &format!("loadNamespace('selfloadeddep'); {oracle}"),
    );
}

#[test]
fn an_unestablished_linked_loaded_observation_blocks() {
    let work = tempfile::tempdir().unwrap();
    let dependency = work.path().join("observeddep");
    let source = work.path().join("observedroot");
    let original = work.path().join("original");
    fs::create_dir(&original).unwrap();
    package(
        &dependency,
        "observeddep",
        "",
        "export(run)\n",
        "run <- function() NULL\n",
    );
    package(
        &source,
        "observedroot",
        "Imports: observeddep\n",
        "export(run)\n",
        "run <- function() { observeddep::run(); isNamespaceLoaded('observeddep') }\n",
    );
    let r_home = discover_r_home();
    install_package(&r_home, &dependency, &original);
    install_package_using(&r_home, &source, &original, &original);
    run_r(
        &r_home,
        &original,
        "stopifnot(identical(observedroot::run(), TRUE))",
    );
    let output = work.path().join("output");
    let result = build(&source, &output, &original, &work.path().join("cache"), "1");
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("loaded-state"));
    assert!(!output.exists());
}
