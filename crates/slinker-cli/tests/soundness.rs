mod common;

use common::{assert_success, discover_r_home, install_package, run_r, slinker};
use std::fs;
use std::path::Path;

fn package(path: &Path, name: &str, imports: &str, code: &str) {
    fs::create_dir_all(path.join("R")).unwrap();
    fs::write(path.join("DESCRIPTION"), format!("Package: {name}\nVersion: 1.0.0\nTitle: Soundness fixture\nDescription: Original R is the behavioral oracle.\nLicense: MIT\nAuthor: Test\nMaintainer: Test <test@example.com>\n{imports}")).unwrap();
    fs::write(path.join("NAMESPACE"), "export(run)\n").unwrap();
    fs::write(path.join("R/fixture.R"), code).unwrap();
}

#[test]
fn structural_payload_members_retain_each_callable_as_in_original_r() {
    let r = discover_r_home();
    for (label, members, call) in [
        (
            "duplicates",
            "list(same=function() 1L, same=function() hidden())",
            "bundle[[2L]]()",
        ),
        (
            "delimiters",
            "list('a$b'=function() 1L, a=list(b=function() hidden()))",
            "bundle$a$b()",
        ),
        (
            "expression",
            "as.expression(list(function() hidden()))",
            "bundle[[1L]]()",
        ),
        (
            "pairlist",
            "pairlist(same=function() 1L, same=function() hidden())",
            "bundle[[2L]]()",
        ),
        (
            "primitive",
            "list(sum)",
            "bundle[[1L]](structure(1L, class='foo'))",
        ),
        (
            "language",
            "as.call(list(quote(identity), function() hidden()))",
            "bundle[[2L]]()",
        ),
        (
            "altrep",
            "structure(1:1000000, callback=function() hidden())",
            "attr(bundle, 'callback')()",
        ),
        (
            "expressionattribute",
            "structure(expression(1L), callback=function() hidden())",
            "attr(bundle, 'callback')()",
        ),
    ] {
        let temp = tempfile::tempdir().unwrap();
        let original = temp.path().join("original");
        fs::create_dir(&original).unwrap();
        let dep = temp.path().join("dep");
        package(
            &dep,
            "sounddep",
            "",
            &format!(
                "hidden <- function() 'expected'\nSummary.foo <- function(..., na.rm=FALSE) 'expected'\nbundle <- {members}\nrun <- function() {call}\n"
            ),
        );
        install_package(&r, &dep, &original);
        let root = temp.path().join("root");
        package(
            &root,
            "soundroot",
            "Imports: sounddep\n",
            "run <- function() sounddep::run()",
        );
        install_package(&r, &root, &original);
        let oracle = "stopifnot(identical(soundroot::run(), 'expected'))";
        run_r(&r, &original, oracle);
        let output = temp.path().join("generated");
        let result = slinker(&[
            "build".as_ref(),
            root.as_os_str(),
            "--lib".as_ref(),
            original.as_os_str(),
            "--output".as_ref(),
            output.as_os_str(),
        ]);
        assert_success(&result, &format!("build {label} payload"));
        let generated = temp.path().join("generated-library");
        fs::create_dir(&generated).unwrap();
        install_package(&r, &output, &generated);
        for mode in ["absent", "installed", "loaded"] {
            let libraries = if mode == "absent" {
                generated.clone().into_os_string()
            } else {
                std::env::join_paths([&generated, &original]).unwrap()
            };
            let preload = if mode == "loaded" {
                "loadNamespace('sounddep');"
            } else {
                ""
            };
            run_r(&r, libraries, &format!("{preload} {oracle}"));
        }
    }
}

#[test]
fn foreign_closure_enclosures_retain_their_namespace_without_lexical_references() {
    let r = discover_r_home();
    let temp = tempfile::tempdir().unwrap();
    let original = temp.path().join("original");
    fs::create_dir(&original).unwrap();
    let foreign = temp.path().join("foreign");
    package(&foreign, "soundforeign", "", "run <- function() 42L");
    install_package(&r, &foreign, &original);
    let dep = temp.path().join("dep");
    package(
        &dep,
        "sounddep",
        "Imports: soundforeign\n",
        "run <- soundforeign::run",
    );
    common::install_package_using(&r, &dep, &original, &original);
    let root = temp.path().join("root");
    package(
        &root,
        "soundroot",
        "Imports: sounddep\n",
        "run <- function() sounddep::run()",
    );
    common::install_package_using(&r, &root, &original, &original);
    let oracle = "stopifnot(identical(soundroot::run(), 42L))";
    run_r(&r, &original, oracle);
    let output = temp.path().join("generated");
    assert_success(
        &slinker(&[
            "build".as_ref(),
            root.as_os_str(),
            "--lib".as_ref(),
            original.as_os_str(),
            "--output".as_ref(),
            output.as_os_str(),
        ]),
        "build foreign closure enclosure",
    );
    let generated = temp.path().join("generated-library");
    fs::create_dir(&generated).unwrap();
    install_package(&r, &output, &generated);
    for mode in ["absent", "installed", "loaded"] {
        let libraries = if mode == "absent" {
            generated.clone().into_os_string()
        } else {
            std::env::join_paths([&generated, &original]).unwrap()
        };
        let preload = if mode == "loaded" {
            "loadNamespace('sounddep'); loadNamespace('soundforeign');"
        } else {
            ""
        };
        run_r(&r, libraries, &format!("{preload} {oracle}"));
    }
}

#[test]
fn namespace_objects_in_payloads_cannot_expose_removed_bindings() {
    let r = discover_r_home();
    let temp = tempfile::tempdir().unwrap();
    let original = temp.path().join("original");
    fs::create_dir(&original).unwrap();
    let dep = temp.path().join("dep");
    package(
        &dep,
        "sounddep",
        "",
        "hidden <- 42L\nrun <- function() NULL",
    );
    install_package(&r, &dep, &original);
    let root = temp.path().join("root");
    package(
        &root,
        "soundroot",
        "Imports: sounddep\n",
        "box <- list(asNamespace('sounddep'))\nrun <- function() box[[1L]]$hidden",
    );
    common::install_package_using(&r, &root, &original, &original);
    run_r(&r, &original, "stopifnot(identical(soundroot::run(),42L))");
    let output = temp.path().join("generated");
    let result = slinker(&[
        "build".as_ref(),
        root.as_os_str(),
        "--lib".as_ref(),
        original.as_os_str(),
        "--output".as_ref(),
        output.as_os_str(),
    ]);
    assert!(
        !result.status.success(),
        "unproven payload namespace built successfully"
    );
    assert!(
        String::from_utf8_lossy(&result.stderr).contains("UnsupportedObject"),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(!output.exists());
}

#[test]
fn lossy_function_defaults_block_before_publication() {
    let r = discover_r_home();
    let temp = tempfile::tempdir().unwrap();
    let original = temp.path().join("original");
    fs::create_dir(&original).unwrap();
    let root = temp.path().join("root");
    package(
        &root,
        "soundroot",
        "",
        "run <- function(x) class(x)\nformals(run)$x <- structure(1, class='special')",
    );
    install_package(&r, &root, &original);
    run_r(
        &r,
        &original,
        "stopifnot(identical(soundroot::run(), 'special'))",
    );
    let output = temp.path().join("generated");
    let result = slinker(&[
        "build".as_ref(),
        root.as_os_str(),
        "--output".as_ref(),
        output.as_os_str(),
    ]);
    assert!(!result.status.success(), "lossy default built successfully");
    assert!(
        String::from_utf8_lossy(&result.stderr).contains("source_mismatch"),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(!output.exists());
}

#[test]
fn unproven_string_calls_block_instead_of_removing_the_runtime_target() {
    let r = discover_r_home();
    let cases = [
        (
            "unknown",
            "run <- function(nm='left') do.call(nm, list())",
            "left",
        ),
        (
            "lazy",
            "ignore <- function(x) NULL\nrun <- function() { nm <- 'left'; ignore(nm <- 'right'); do.call(nm,list()) }",
            "left",
        ),
        (
            "capture",
            "run <- function() { nm <- 'left'; f <- function() nm; nm <- 'right'; do.call(f(),list()) }",
            "right",
        ),
        (
            "split",
            "run <- function() { x <- strsplit('x,', ',', fixed=TRUE)[[1L]]; nm <- if(length(x)==1L) 'left' else 'right'; do.call(nm,list()) }",
            "left",
        ),
        (
            "concat",
            "run <- function() { nm <- c(NULL,'left')[[1L]]; do.call(nm,list()) }",
            "left",
        ),
        (
            "length",
            "methods <- list(a=function() 1L,b=1L)\nrun <- function() { nm <- if(length(methods)==2L) 'left' else 'right'; do.call(nm,list()) }",
            "left",
        ),
        (
            "names",
            "methods <- list(z=function() 1L,a=function() 2L)\nrun <- function() { nm <- if(names(methods)[[1L]]=='z') 'left' else 'right'; do.call(nm,list()) }",
            "left",
        ),
        (
            "dots",
            "pick <- function(..., nm='left') nm\nrun <- function() do.call(pick(n='right'),list())",
            "left",
        ),
    ];
    for (label, code, expected) in cases {
        let temp = tempfile::tempdir().unwrap();
        let library = temp.path().join("library");
        fs::create_dir(&library).unwrap();
        let dep = temp.path().join("dep");
        package(
            &dep,
            "sounddep",
            "",
            &format!("left <- function() 'left'\nright <- function() 'right'\n{code}"),
        );
        install_package(&r, &dep, &library);
        run_r(
            &r,
            &library,
            &format!("stopifnot(identical(sounddep::run(), '{expected}'))"),
        );
        let root = temp.path().join("root");
        package(
            &root,
            "soundroot",
            "Imports: sounddep\n",
            "run <- function() sounddep::run()",
        );
        let output = temp.path().join("generated");
        let result = slinker(&[
            "build".as_ref(),
            root.as_os_str(),
            "--lib".as_ref(),
            library.as_os_str(),
            "--output".as_ref(),
            output.as_os_str(),
        ]);
        assert!(
            !result.status.success(),
            "{label}: unproven call built successfully"
        );
        assert!(
            String::from_utf8_lossy(&result.stderr).contains("DynamicLookup"),
            "{label}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(!output.exists(), "{label}: failed build published output");
    }
}

#[test]
fn linked_unary_operator_and_closure_attributes_match_original_r() {
    let r = discover_r_home();
    let temp = tempfile::tempdir().unwrap();
    let original = temp.path().join("original");
    fs::create_dir(&original).unwrap();
    let dep = temp.path().join("dep");
    package(
        &dep,
        "sounddep",
        "",
        "`!` <- function(x) FALSE\nspecial <- function() 1L\nattr(special, 'answer') <- 42L\nrun <- function() list(!FALSE, attr(special,'answer'))",
    );
    install_package(&r, &dep, &original);
    run_r(
        &r,
        &original,
        "stopifnot(identical(sounddep::run(), list(FALSE,42L)))",
    );
    let root = temp.path().join("root");
    package(
        &root,
        "soundroot",
        "Imports: sounddep\n",
        "run <- function() sounddep::run()",
    );
    let output = temp.path().join("generated");
    let result = slinker(&[
        "build".as_ref(),
        root.as_os_str(),
        "--lib".as_ref(),
        original.as_os_str(),
        "--output".as_ref(),
        output.as_os_str(),
    ]);
    assert_success(&result, "build operator and attribute fixture");
    let generated = temp.path().join("generated-library");
    fs::create_dir(&generated).unwrap();
    install_package(&r, &output, &generated);
    for mode in ["absent", "installed", "loaded"] {
        let libraries = if mode == "absent" {
            generated.display().to_string()
        } else {
            std::env::join_paths([&generated, &original])
                .unwrap()
                .to_string_lossy()
                .into_owned()
        };
        let preload = if mode == "loaded" {
            "loadNamespace('sounddep');"
        } else {
            ""
        };
        run_r(
            &r,
            libraries,
            &format!("{preload} stopifnot(identical(soundroot::run(),list(FALSE,42L)))"),
        );
    }
}

#[test]
fn root_support_binding_collisions_block_before_publication() {
    let r = discover_r_home();
    for name in [".slinker_runtime", ".slinker_original_on_load"] {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("root");
        package(
            &root,
            "soundroot",
            "",
            &format!("{name} <- function() 42L\nrun <- function() {name}()"),
        );
        let original = temp.path().join("original");
        fs::create_dir(&original).unwrap();
        install_package(&r, &root, &original);
        run_r(&r, &original, "stopifnot(identical(soundroot::run(),42L))");
        let output = temp.path().join("generated");
        let result = slinker(&[
            "build".as_ref(),
            root.as_os_str(),
            "--output".as_ref(),
            output.as_os_str(),
        ]);
        assert!(
            !result.status.success(),
            "{name}: colliding Root built successfully"
        );
        assert!(String::from_utf8_lossy(&result.stderr).contains(name));
        assert!(!output.exists());
    }
}

#[test]
fn imported_and_lifecycle_created_support_names_block_before_publication() {
    let r = discover_r_home();
    for imported in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let library = temp.path().join("library");
        fs::create_dir(&library).unwrap();
        let root = temp.path().join("root");
        if imported {
            let dep = temp.path().join("dep");
            package(
                &dep,
                "sounddep",
                "",
                ".slinker_runtime <- function() 42L\nrun <- .slinker_runtime",
            );
            fs::write(dep.join("NAMESPACE"), "export(.slinker_runtime,run)\n").unwrap();
            install_package(&r, &dep, &library);
            package(
                &root,
                "soundroot",
                "Imports: sounddep\n",
                "run <- function() .slinker_runtime()",
            );
            fs::write(
                root.join("NAMESPACE"),
                "export(run)\nimportFrom(sounddep,.slinker_runtime)\n",
            )
            .unwrap();
        } else {
            package(
                &root,
                "soundroot",
                "",
                ".onLoad <- function(libname,pkgname) makeActiveBinding('.slinker_runtime',function(value) 42L,asNamespace('soundroot'))\nrun <- function() .slinker_runtime",
            );
        }
        install_package(&r, &root, &library);
        run_r(&r, &library, "stopifnot(identical(soundroot::run(),42L))");
        let output = temp.path().join("generated");
        let result = slinker(&[
            "build".as_ref(),
            root.as_os_str(),
            "--lib".as_ref(),
            library.as_os_str(),
            "--output".as_ref(),
            output.as_os_str(),
        ]);
        assert!(!result.status.success());
        assert!(String::from_utf8_lossy(&result.stderr).contains(".slinker_runtime"));
        assert!(!output.exists());
    }
}

#[test]
fn recursive_root_hook_does_not_repeat_bootstrap() {
    let r = discover_r_home();
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("root");
    package(
        &root,
        "soundroot",
        "",
        "count <- 0L\n.onLoad <- function(libname,pkgname) { count <<- count+1L; if(count==1L) .onLoad(libname,pkgname) }\nrun <- function() count",
    );
    let original = temp.path().join("original");
    fs::create_dir(&original).unwrap();
    install_package(&r, &root, &original);
    run_r(&r, &original, "stopifnot(identical(soundroot::run(),2L))");
    let output = temp.path().join("generated");
    assert_success(
        &slinker(&[
            "build".as_ref(),
            root.as_os_str(),
            "--output".as_ref(),
            output.as_os_str(),
        ]),
        "build recursive Root hook",
    );
    let generated = temp.path().join("generated-library");
    fs::create_dir(&generated).unwrap();
    install_package(&r, &output, &generated);
    run_r(&r, &generated, "stopifnot(identical(soundroot::run(),2L))");
}

#[test]
fn attributed_private_closures_with_reserved_names_match_original_r() {
    let r = discover_r_home();
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("root");
    package(
        &root,
        "soundroot",
        "Imports: methods\n",
        "box <- local({ e <- new.env(parent=baseenv()); assign('function', methods::new('MethodDefinition', .Data=function() 42L), envir=e); e })\nrun <- function() box[['function']]()",
    );
    let original = temp.path().join("original");
    fs::create_dir(&original).unwrap();
    install_package(&r, &root, &original);
    run_r(&r, &original, "stopifnot(identical(soundroot::run(),42L))");
    let output = temp.path().join("generated");
    assert_success(
        &slinker(&[
            "build".as_ref(),
            root.as_os_str(),
            "--output".as_ref(),
            output.as_os_str(),
        ]),
        "build attributed private closure with reserved name",
    );
    let generated = temp.path().join("generated-library");
    fs::create_dir(&generated).unwrap();
    install_package(&r, &output, &generated);
    run_r(&r, &generated, "stopifnot(identical(soundroot::run(),42L))");
}

#[test]
fn namespace_queries_and_private_environment_callbacks_link_their_functions() {
    let r = discover_r_home();
    for code in [
        "run <- function() c(getExportedValue('sounddep','run')(), utils::getFromNamespace('run','sounddep')())",
        "box <- local({ e <- new.env(parent=emptyenv()); e$f <- function() sounddep::run(); e })\nrun <- function() c(box$f(),box$f())",
    ] {
        let temp = tempfile::tempdir().unwrap();
        let original = temp.path().join("original");
        fs::create_dir(&original).unwrap();
        let dep = temp.path().join("dep");
        package(
            &dep,
            "sounddep",
            "",
            "run <- function() 42L\nunused <- function() stop('unused')",
        );
        install_package(&r, &dep, &original);
        let root = temp.path().join("root");
        package(&root, "soundroot", "Imports: sounddep, utils\n", code);
        install_package(&r, &root, &original);
        run_r(
            &r,
            &original,
            "stopifnot(identical(soundroot::run(),c(42L,42L)))",
        );
        let output = temp.path().join("generated");
        assert_success(
            &slinker(&[
                "build".as_ref(),
                root.as_os_str(),
                "--lib".as_ref(),
                original.as_os_str(),
                "--output".as_ref(),
                output.as_os_str(),
            ]),
            "build explicit function references",
        );
        let generated = temp.path().join("generated-library");
        fs::create_dir(&generated).unwrap();
        install_package(&r, &output, &generated);
        for mode in ["absent", "installed", "loaded"] {
            let libraries = if mode == "absent" {
                generated.display().to_string()
            } else {
                std::env::join_paths([&generated, &original])
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            };
            let preload = if mode == "loaded" {
                "loadNamespace('sounddep');"
            } else {
                ""
            };
            run_r(
                &r,
                libraries,
                &format!("{preload} stopifnot(identical(soundroot::run(),c(42L,42L)))"),
            );
        }
    }
}

#[test]
fn dispatched_comparisons_do_not_prove_repeated_conditions_equal() {
    let r = discover_r_home();
    let temp = tempfile::tempdir().unwrap();
    let original = temp.path().join("original");
    fs::create_dir(&original).unwrap();
    let dep = temp.path().join("dep");
    package(
        &dep,
        "sounddep",
        "",
        "state <- new.env(parent=emptyenv())\nstate$n <- 0L\nOps.flip <- function(e1,e2) { state$n <- state$n+1L; state$n==2L }\nfallback <- function() 42L\nrun <- function(value) { if(value==0L) fallback <- function() 1L; if(value==0L) fallback() else 0L }",
    );
    fs::write(dep.join("NAMESPACE"), "export(run)\nS3method(Ops,flip)\n").unwrap();
    install_package(&r, &dep, &original);
    let root = temp.path().join("root");
    package(
        &root,
        "soundroot",
        "Imports: sounddep\n",
        "run <- function() sounddep::run(structure(1L,class='flip'))",
    );
    install_package(&r, &root, &original);
    run_r(&r, &original, "stopifnot(identical(soundroot::run(),42L))");
    let output = temp.path().join("generated");
    assert_success(
        &slinker(&[
            "build".as_ref(),
            root.as_os_str(),
            "--lib".as_ref(),
            original.as_os_str(),
            "--output".as_ref(),
            output.as_os_str(),
        ]),
        "build dispatching comparisons",
    );
    let generated = temp.path().join("generated-library");
    fs::create_dir(&generated).unwrap();
    install_package(&r, &output, &generated);
    run_r(&r, &generated, "stopifnot(identical(soundroot::run(),42L))");
}

#[test]
fn intervening_calls_do_not_prove_guard_bindings_unchanged() {
    let r = discover_r_home();
    let temp = tempfile::tempdir().unwrap();
    let original = temp.path().join("original");
    fs::create_dir(&original).unwrap();
    let dep = temp.path().join("dep");
    package(
        &dep,
        "sounddep",
        "",
        "change <- function() assign('value',NULL,envir=parent.frame())\nfallback <- function() 42L\nrun <- function(value) { if(is.null(value)) fallback <- function() 1L; change(); if(is.null(value)) fallback() else 0L }",
    );
    install_package(&r, &dep, &original);
    let root = temp.path().join("root");
    package(
        &root,
        "soundroot",
        "Imports: sounddep\n",
        "run <- function() sounddep::run(1L)",
    );
    install_package(&r, &root, &original);
    run_r(&r, &original, "stopifnot(identical(soundroot::run(),42L))");
    let output = temp.path().join("generated");
    assert_success(
        &slinker(&[
            "build".as_ref(),
            root.as_os_str(),
            "--lib".as_ref(),
            original.as_os_str(),
            "--output".as_ref(),
            output.as_os_str(),
        ]),
        "build mutable guard fixture",
    );
    let generated = temp.path().join("generated-library");
    fs::create_dir(&generated).unwrap();
    install_package(&r, &output, &generated);
    run_r(&r, &generated, "stopifnot(identical(soundroot::run(),42L))");
}
