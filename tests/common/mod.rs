#![allow(dead_code)]

use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

pub fn slinker(arguments: &[&OsStr]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_slinker"))
        .args(arguments)
        .output()
        .expect("run slinker")
}

pub fn install_package(r_home: &Path, package: &Path, library: &Path) {
    let output = Command::new(r_executable(r_home))
        .args(["CMD", "INSTALL", "--no-test-load"])
        .arg(format!("--library={}", library.display()))
        .arg(package)
        .output()
        .expect("R CMD INSTALL");
    assert_success(&output, "R CMD INSTALL");
}

pub fn run_r(r_home: &Path, libraries: impl AsRef<OsStr>, expression: &str) {
    let output = run_r_output(r_home, libraries, expression);
    assert_success(&output, "target R expression");
}

pub fn run_r_output(r_home: &Path, libraries: impl AsRef<OsStr>, expression: &str) -> Output {
    let libraries = libraries.as_ref();
    r_script_output(r_home, expression, |command| {
        command
            .arg("--vanilla")
            .env("R_LIBS", libraries)
            .env("R_LIBS_USER", libraries)
            .env_remove("R_LIBS_SITE")
    })
}

pub fn run_r_with_site_profile(r_home: &Path, expression: &str) {
    let output = r_script_output(r_home, expression, |command| command.arg("--no-init-file"));
    assert_success(&output, "target R expression with site profile");
}

fn r_script_output(
    r_home: &Path,
    expression: &str,
    configure: impl FnOnce(&mut Command) -> &mut Command,
) -> Output {
    let script = tempfile::Builder::new()
        .suffix(".R")
        .tempfile()
        .expect("R script");
    fs::write(script.path(), expression).expect("write R script");
    let mut command = Command::new(r_executable(r_home));
    command.args(["--slave", "--no-save", "--no-restore"]);
    configure(&mut command)
        .arg("-f")
        .arg(script.path())
        .output()
        .expect("run target R")
}

pub fn discover_r_home() -> PathBuf {
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

pub fn r_executable(r_home: &Path) -> PathBuf {
    [
        r_home.join("bin/x64/R.exe"),
        r_home.join("bin/R.exe"),
        r_home.join("bin/R"),
    ]
    .into_iter()
    .find(|path| path.is_file())
    .expect("target R executable")
}

pub fn assert_success(output: &Output, operation: &str) {
    assert!(
        output.status.success(),
        "{operation} failed ({})\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

pub fn install_package_using(
    r_home: &Path,
    package: &Path,
    library: &Path,
    libraries: impl AsRef<OsStr>,
) {
    let output = Command::new(r_executable(r_home))
        .args(["CMD", "INSTALL", "--no-test-load"])
        .arg(format!("--library={}", library.display()))
        .arg(package)
        .env("R_LIBS", libraries.as_ref())
        .output()
        .expect("R CMD INSTALL");
    assert_success(&output, "R CMD INSTALL");
}
