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

fn slinker_with_cache(args: &[&str], cache: &std::path::Path) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_slinker"))
        .args(args)
        .env("SLINKER_CACHE_DIR", cache)
        .output()
        .expect("run slinker binary")
}

#[test]
fn cache_commands_report_and_clear_the_persistent_cache() {
    let cache = tempfile::tempdir().expect("cache directory");
    let empty = slinker_with_cache(&["cache", "--json"], cache.path());
    assert!(empty.status.success());
    let document: serde_json::Value = serde_json::from_slice(&empty.stdout).expect("cache JSON");
    assert_eq!(document["schemas"], serde_json::json!([]));

    let analysis = slinker_with_cache(&["analyze", "stats4", "--json"], cache.path());
    assert!(analysis.status.success() || !analysis.stdout.is_empty());
    let report = slinker_with_cache(&["cache", "--json"], cache.path());
    let document: serde_json::Value = serde_json::from_slice(&report.stdout).expect("cache JSON");
    let packages = document["schemas"][0]["packages"]
        .as_array()
        .expect("package list");
    assert!(packages.iter().any(|package| package["name"] == "stats4"));
    assert!(packages.iter().all(|package| {
        package["cache_key"]
            .as_str()
            .is_some_and(|key| key.len() == 64)
    }));

    let cleared = slinker_with_cache(&["cache", "clear", "stats4", "--json"], cache.path());
    assert!(cleared.status.success());
    let outcome: serde_json::Value = serde_json::from_slice(&cleared.stdout).expect("clear JSON");
    assert!(outcome["entries_removed"].as_u64().unwrap_or(0) > 0);
    let report = slinker_with_cache(&["cache", "--json"], cache.path());
    let document: serde_json::Value = serde_json::from_slice(&report.stdout).expect("cache JSON");
    assert!(
        document["schemas"]
            .as_array()
            .is_none_or(|schemas| schemas.iter().all(|schema| schema["packages"]
                .as_array()
                .is_none_or(|packages| packages
                    .iter()
                    .all(|package| package["name"] != "stats4"))))
    );

    assert!(
        slinker_with_cache(&["cache", "clear"], cache.path())
            .status
            .success()
    );
    let path = slinker_with_cache(&["cache", "path"], cache.path());
    assert!(
        String::from_utf8_lossy(&path.stdout).contains(cache.path().to_string_lossy().as_ref())
    );
}
