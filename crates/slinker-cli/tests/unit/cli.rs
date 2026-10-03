use super::{Cli, Command, RootSpec, UserCommand, parse_r_home};
use clap::{CommandFactory, Parser};
use std::path::Path;

#[test]
fn cli_definition_is_consistent() {
    Cli::command().debug_assert();
}

#[test]
fn build_defaults_to_current_directory() {
    let Command::User(UserCommand::Build(args)) = Cli::parse_from(["slinker", "build"]).command
    else {
        panic!("expected build command");
    };
    assert_eq!(args.path, Path::new("."));
    assert_eq!(args.output, None);
}

#[test]
fn analyze_accepts_ordered_libraries_and_package_lists() {
    let Command::User(UserCommand::Analyze(args)) = Cli::parse_from([
        "slinker",
        "analyze",
        "voucher",
        "--lib",
        "one",
        "--lib=two",
        "--external",
        "cli,glue",
        "--link=foo,bar",
        "--threads",
        "3",
        "--json",
    ])
    .command
    else {
        panic!("expected analyze command");
    };
    assert_eq!(args.analysis.root, RootSpec::Installed("voucher".into()));
    assert_eq!(
        args.analysis.universe.libraries,
        [Path::new("one"), Path::new("two")]
    );
    assert_eq!(args.analysis.universe.external, ["cli", "glue"]);
    assert_eq!(args.analysis.universe.linked, ["foo", "bar"]);
    assert_eq!(args.analysis.universe.threads.get(), 3);
    assert!(args.json.json);
}

#[test]
fn query_takes_root_then_target() {
    let Command::User(UserCommand::Why(args)) =
        Cli::parse_from(["slinker", "why", "voucher", "cli::cli_abort"]).command
    else {
        panic!("expected why command");
    };
    assert_eq!(args.analysis.root, RootSpec::Installed("voucher".into()));
    assert_eq!(args.target.to_string(), "cli::cli_abort");
}

#[test]
fn root_is_a_package_name_or_a_path() {
    assert_eq!(
        RootSpec::parse("voucher"),
        Ok(RootSpec::Installed("voucher".into()))
    );
    for path in ["./voucher", "..", ".", "C:\\src\\voucher"] {
        assert_eq!(
            RootSpec::parse(path),
            Ok(RootSpec::Source(path.into())),
            "{path}"
        );
    }
    assert!(RootSpec::parse("").is_err());
}

#[test]
fn check_takes_a_path_defaulting_to_the_current_directory() {
    let Command::User(UserCommand::Check(args)) =
        Cli::parse_from(["slinker", "check", "--json"]).command
    else {
        panic!("expected check command");
    };
    assert_eq!(args.path, Path::new("."));
    assert!(args.json.json);
}

#[test]
fn rejects_invalid_arguments() {
    for args in [
        &["slinker", "analyze"][..],
        &["slinker", "analyze", "voucher", "--threads=0"],
        &["slinker", "analyze", "voucher", "--external", "cli,,glue"],
        &["slinker", "analyze", "voucher", "--bogus", "json"],
        &["slinker", "why", "voucher"],
    ] {
        assert!(Cli::try_parse_from(args).is_err(), "{args:?}");
    }
}

#[test]
fn r_home_uses_last_non_warning_line() {
    let stdout = "WARNING: ignoring environment value of R_HOME\nC:/Program Files/R/R-4.6.1\n\n";
    assert_eq!(parse_r_home(stdout), Some("C:/Program Files/R/R-4.6.1"));
}
