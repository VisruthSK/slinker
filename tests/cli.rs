use std::process::Command;

fn slinker(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_slinker"))
        .args(args)
        .output()
        .expect("run slinker binary")
}

#[test]
fn version_reports_package_version() {
    let output = slinker(&["--version"]);
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("utf-8 stdout");
    assert_eq!(
        stdout.trim(),
        format!("slinker {}", env!("CARGO_PKG_VERSION"))
    );
}

#[test]
fn help_lists_public_commands_only() {
    let output = slinker(&["--help"]);
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("utf-8 stdout");
    for command in ["build", "check", "analyze", "why", "path", "R_HOME"] {
        assert!(stdout.contains(command), "{command} missing from help");
    }
    assert!(!stdout.contains("__r-worker"));
}

#[test]
fn mistyped_command_suggests_the_closest_one() {
    let output = slinker(&["biuld"]);
    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).expect("utf-8 stderr");
    assert!(stderr.contains("'build'"), "{stderr}");
}

#[test]
fn unknown_option_is_rejected_before_analysis() {
    let output = slinker(&["analyze", "glue", "--bogus", "yaml"]);
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8(output.stderr).expect("utf-8 stderr");
    assert!(stderr.contains("--bogus"));
}
