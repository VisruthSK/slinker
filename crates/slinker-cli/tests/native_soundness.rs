mod common;

use common::{assert_success, discover_r_home, install_package, run_r, run_r_output};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const NATIVE_SOURCE: &str = r#"
#ifndef _WIN32
#define _GNU_SOURCE
#include <dlfcn.h>
#else
#include <windows.h>
#endif
#include <stdio.h>
#include <string.h>
#include <R.h>
#include <Rinternals.h>
#include <R_ext/Rdynload.h>
static int count = 0;
SEXP zero(void) { return Rf_ScalarInteger(42); }
SEXP same_c(void) { return Rf_ScalarInteger(7); }
SEXP same_call(void) { return Rf_ScalarInteger(42); }
SEXP external_tick(SEXP call, SEXP op, SEXP args, SEXP env) {
    return Rf_ScalarInteger(++count);
}
SEXP asset(void) {
    char path[4096];
#ifdef _WIN32
    HMODULE module;
    if (!GetModuleHandleExA(GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS |
        GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT, (LPCSTR)&asset, &module) ||
        !GetModuleFileNameA(module, path, sizeof(path))) return Rf_ScalarInteger(-2);
    char *separator = strrchr(path, '\\');
#else
    Dl_info info;
    if (!dladdr((void *)&asset, &info) || strlen(info.dli_fname) >= sizeof(path))
        return Rf_ScalarInteger(-2);
    strcpy(path, info.dli_fname);
    char *separator = strrchr(path, '/');
#endif
    if (!separator) return Rf_ScalarInteger(-2);
    strcpy(separator + 1, "value.txt");
    FILE *file = fopen(path, "r");
    int value = -1;
    if (file) { if (fscanf(file, "%d", &value) != 1) value = -3; fclose(file); }
    return Rf_ScalarInteger(value);
}
static const R_CMethodDef c_methods[] = {
    {"same", (DL_FUNC)&same_c, 0, NULL}, {NULL, NULL, 0, NULL}
};
static const R_CallMethodDef call_methods[] = {
    {"same", (DL_FUNC)&same_call, 0}, {"zero", (DL_FUNC)&zero, 0},
    {"asset", (DL_FUNC)&asset, 0}, {NULL, NULL, 0}
};
static const R_ExternalMethodDef external_methods[] = {
    {"external_tick", (DL_FUNC)&external_tick, -1}, {NULL, NULL, 0}
};
void R_init_nativedep(DllInfo *dll) {
    R_registerRoutines(dll, c_methods, call_methods, NULL, external_methods);
    R_useDynamicSymbols(dll, FALSE);
}
"#;

struct Fixture {
    directory: tempfile::TempDir,
    r: PathBuf,
    library: PathBuf,
    root: PathBuf,
    output: PathBuf,
    summaries: PathBuf,
}

impl Fixture {
    fn new(code: &str) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let r = discover_r_home();
        let library = directory.path().join("original");
        fs::create_dir(&library).unwrap();
        let dep = directory.path().join("nativedep");
        package(&dep, "nativedep", "", code);
        fs::write(
            dep.join("NAMESPACE"),
            "useDynLib(nativedep, .registration=TRUE, .fixes='C_')\nexport(run)\n",
        )
        .unwrap();
        fs::create_dir(dep.join("src")).unwrap();
        fs::write(dep.join("src/native.c"), NATIVE_SOURCE).unwrap();
        install_package(&r, &dep, &library);
        // Install a companion file next to the actual DLL on every target architecture.
        run_r(
            &r,
            &library,
            "paths <- list.files(system.file('libs', package='nativedep'), recursive=TRUE, full.names=TRUE); paths <- paths[endsWith(paths, .Platform$dynlib.ext)]; stopifnot(length(paths) == 1L); writeLines('42', file.path(dirname(paths), 'value.txt'))",
        );
        let root = directory.path().join("nativeroot");
        package(
            &root,
            "nativeroot",
            "Imports: nativedep\n",
            "run <- function() nativedep::run()\n",
        );
        install_package(&r, &root, &library);
        let output = directory.path().join("generated");
        let summaries = directory.path().join("summaries.json");
        Self {
            directory,
            r,
            library,
            root,
            output,
            summaries,
        }
    }

    fn build(&self, audited: bool) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_slinker"));
        command
            .arg("build")
            .arg(&self.root)
            .arg("--lib")
            .arg(&self.library)
            .arg("--output")
            .arg(&self.output)
            .env_remove("SLINKER_NATIVE_SUMMARIES");
        if audited {
            command.env("SLINKER_NATIVE_SUMMARIES", &self.summaries);
        }
        command.output().unwrap()
    }

    fn audited_build(&self) -> Output {
        let result = self.build(false);
        assert!(!result.status.success());
        let stderr = String::from_utf8_lossy(&result.stderr);
        let key = "package `nativedep` version `1.0.0` image `";
        let fingerprint = stderr
            .find(key)
            .map(|start| &stderr[start + key.len()..])
            .and_then(|rest| rest.split('`').next())
            .unwrap_or_else(|| panic!("native audit key missing: {stderr}"));
        fs::write(&self.summaries, format!(r#"{{"schema":1,"packages":[{{"package":"nativedep","version":"1.0.0","origin":"installed","image_fingerprint":"{fingerprint}","components":[{{"component":"nativedep","safety":"safe"}}]}}]}}"#)).unwrap();
        self.build(true)
    }

    fn compare(&self, oracle: &str, after_load: &str) {
        assert_success(
            &run_r_output(&self.r, &self.library, oracle),
            "original native oracle",
        );
        assert_success(&self.audited_build(), "build native regression");
        let generated = self.directory.path().join("generated-library");
        fs::create_dir(&generated).unwrap();
        install_package(&self.r, &self.output, &generated);
        assert_success(
            &run_r_output(&self.r, &generated, oracle),
            "generated native oracle, dependency absent",
        );
        let libraries = std::env::join_paths([&generated, &self.library]).unwrap();
        run_r(&self.r, &libraries, oracle);
        run_r(
            &self.r,
            &libraries,
            &format!("loadNamespace('nativedep'); {oracle}"),
        );
        if !after_load.is_empty() {
            run_r(&self.r, &libraries, after_load);
        }
    }
}

fn package(path: &Path, name: &str, imports: &str, code: &str) {
    fs::create_dir_all(path.join("R")).unwrap();
    fs::write(path.join("DESCRIPTION"), format!("Package: {name}\nVersion: 1.0.0\nTitle: Native regression\nDescription: Original R is the oracle.\nLicense: MIT\nAuthor: Test\nMaintainer: Test <test@example.com>\n{imports}")).unwrap();
    fs::write(path.join("NAMESPACE"), "export(run)\n").unwrap();
    fs::write(path.join("R/code.R"), code).unwrap();
}

#[test]
fn native_string_argument_validation_is_not_lost_by_pointer_lowering() {
    let fixture = Fixture::new(
        "run <- function() tryCatch(.Call('zero', 1L, PACKAGE='nativedep'), error=function(e) 'arity')\n",
    );
    fixture.compare("stopifnot(identical(nativeroot::run(), 'arity'))", "");
}

#[test]
fn native_relocations_preserve_the_registered_interface_and_symbol_class() {
    let fixture = Fixture::new(
        "run <- function() list(.Call('same', PACKAGE='nativedep'), class(C_zero$address), .Call(C_zero), tryCatch(.Call(C_same), error=function(e) 'wrong-interface'))\n",
    );
    fixture.compare(
        "stopifnot(identical(nativeroot::run(), list(42L, 'RegisteredNativeSymbol', 42L, 'wrong-interface')))",
        "",
    );
}

#[test]
fn external2_uses_the_private_dll_after_the_real_dependency_loads() {
    let fixture = Fixture::new(
        "run <- function() c(.External2('external_tick', PACKAGE='nativedep'), .External2(C_external_tick))\n",
    );
    fixture.compare("stopifnot(identical(nativeroot::run(), 1:2))", "stopifnot(identical(nativeroot::run(), 1:2)); loadNamespace('nativedep'); for (i in 1:5) nativedep::run(); stopifnot(identical(nativeroot::run(), 3:4))");
}

#[test]
fn private_native_library_keeps_its_companion_files() {
    let fixture = Fixture::new("run <- function() .Call('asset', PACKAGE='nativedep')\n");
    fixture.compare("stopifnot(identical(nativeroot::run(), 42L))", "");
}

#[test]
fn compiled_native_pointer_arity_that_source_restoration_cannot_preserve_blocks() {
    let fixture = Fixture::new("run <- function() .Call(C_zero, 1L)\n");
    run_r(
        &fixture.r,
        &fixture.library,
        "stopifnot(identical(nativeroot::run(), 42L))",
    );
    let result = fixture.audited_build();
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(
        !result.status.success(),
        "unpreserved compiled pointer call was accepted"
    );
    assert!(stderr.contains("UnknownNativeLookup"), "{stderr}");
    assert!(!stderr.contains("UnknownNativeEffects"), "{stderr}");
    assert!(!fixture.output.exists());
}
