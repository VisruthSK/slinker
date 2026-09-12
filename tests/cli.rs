use std::process::Command;

#[test]
fn version_reports_package_version() {
    let output = Command::new(env!("CARGO_BIN_EXE_slinker"))
        .arg("--version")
        .output()
        .expect("run slinker binary");
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("utf-8 stdout");
    assert_eq!(
        stdout.trim(),
        format!("slinker {}", env!("CARGO_PKG_VERSION"))
    );
}

#[test]
fn help_documents_analyze_command() {
    let output = Command::new(env!("CARGO_BIN_EXE_slinker"))
        .arg("--help")
        .output()
        .expect("run slinker binary");
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("utf-8 stdout");
    assert!(stdout.contains("slinker analyze PACKAGE"));
    assert!(stdout.contains("SLINKER_R"));
    assert!(stdout.contains("--lib PATH"));
    assert!(stdout.contains("--graph"));
    assert!(!stdout.contains("--graph-format"));
    assert!(stdout.contains("never installs, rebuilds, or downloads"));
}

#[test]
fn removed_graph_format_is_rejected_before_analysis() {
    let output = Command::new(env!("CARGO_BIN_EXE_slinker"))
        .args(["analyze", "glue", "--graph-format", "yaml"])
        .output()
        .expect("run slinker binary");
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8(output.stderr).expect("utf-8 stderr");
    assert!(stderr.contains("unknown analyze option"));
}
