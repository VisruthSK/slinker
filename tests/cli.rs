use std::process::Command;

#[test]
fn version_reports_package_version() {
    let output = Command::new(env!("CARGO_BIN_EXE_hrm"))
        .arg("--version")
        .output()
        .expect("run hrm binary");
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("utf-8 stdout");
    assert_eq!(stdout.trim(), format!("hrm {}", env!("CARGO_PKG_VERSION")));
}

#[test]
fn help_documents_analyze_command() {
    let output = Command::new(env!("CARGO_BIN_EXE_hrm"))
        .arg("--help")
        .output()
        .expect("run hrm binary");
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("utf-8 stdout");
    assert!(stdout.contains("hrm analyze PACKAGE"));
    assert!(stdout.contains("HRM_R"));
    assert!(stdout.contains("--lib PATH"));
    assert!(stdout.contains("never installs, rebuilds, or downloads"));
}
