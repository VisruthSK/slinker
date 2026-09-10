use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use hrm::analysis::{Analyzer, AnalyzerConfig, TargetPackage};
use hrm::{
    MaterializationRequest, PackageMaterialization, RToolchain, TargetEnvironmentRequest,
};

fn scratch() -> PathBuf {
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    env::temp_dir().join(format!("hrm-{}-{nonce}", std::process::id()))
}

#[test]
#[ignore = "requires HRM_R and voucher installed in the selected target library"]
fn inspects_and_materializes_installed_voucher_image() -> Result<(), Box<dyn std::error::Error>> {
    let r = PathBuf::from(env::var("HRM_R")?);
    let toolchain = RToolchain::from_r(&r);
    let work = scratch();
    fs::create_dir_all(&work)?;

    let target = toolchain.capture_target_environment(&TargetEnvironmentRequest::new(work.join("target")))?;
    let voucher = target.package("voucher").ok_or("voucher must be installed in the target R library")?;
    for dependency in ["cli", "fs"] {
        if target.package(dependency).is_none() {
            return Err(format!(
                "voucher imports {dependency}; install it into the target R library before running this integration test"
            )
            .into());
        }
    }

    let output = work.join("voucher-image.hrm");
    let snapshot = toolchain.inspect_package_snapshot_with_libraries(
        &voucher.library,
        "voucher",
        &output,
        &target.libraries,
    )?;

    assert_eq!(snapshot.state.package, "voucher");
    assert!(!snapshot.state.has_on_load);

    let analysis = Analyzer::default().analyze(&AnalyzerConfig {
        root: snapshot.analysis_root.clone(),
        dependencies: Vec::new(),
        target_packages: target
            .packages
            .iter()
            .map(|package| TargetPackage {
                name: package.name.clone(),
                version: package.version.clone(),
            })
            .collect(),
    })?;
    assert!(!analysis.graph.nodes.is_empty());
    assert!(analysis.reachable.iter().any(|reachable| *reachable));
    assert!(analysis.diagnostics.iter().all(|diagnostic| !diagnostic.reachable));

    for binding in ["vouch_split_handle", "vouch_parse_line", "vouch_entry_matches_target"] {
        assert!(
            snapshot
                .state
                .bindings
                .iter()
                .any(|candidate| candidate.name == binding && candidate.supported),
            "expected real voucher binding {binding} to be materializable"
        );
    }

    // Materialization is still exercised on a small explicit slice here. The
    // analyzer above now builds real reachability; wiring the retained analysis graph directly
    // into materialization is the next layer.
    let package = PackageMaterialization::from_snapshot(
        &snapshot,
        ["vouch_split_handle", "vouch_parse_line", "vouch_entry_matches_target"],
    )?;
    let mut materialization = MaterializationRequest::new(work.join("baseline.rds"));
    materialization.packages.push(package);
    materialization.target = Some(target.target.clone());
    let artifact = toolchain.materialize(&materialization)?;
    assert!(artifact.baseline_rds.is_file());
    assert!(artifact.lazy_rdb.as_ref().is_some_and(|path| path.is_file()));
    assert!(artifact.lazy_rdx.as_ref().is_some_and(|path| path.is_file()));

    let check = r#"
check_voucher <- function(ns) {
  split <- get("vouch_split_handle", envir = ns, inherits = FALSE)
  parse <- get("vouch_parse_line", envir = ns, inherits = FALSE)
  matches <- get("vouch_entry_matches_target", envir = ns, inherits = FALSE)

  target <- split("GitHub:Alice")
  stopifnot(identical(target$username, "alice"))
  stopifnot(identical(target$raw_platform, "github"))
  stopifnot(identical(target$platform, "github"))

  entry <- parse("-github:Alice spam")
  stopifnot(identical(entry$type, "denounce"))
  stopifnot(identical(entry$username, "alice"))
  stopifnot(identical(entry$platform, "github"))
  stopifnot(isTRUE(matches(entry, target)))

  stopifnot(identical(parent.env(parent.env(ns)), .BaseNamespaceEnv))
  stopifnot(environmentIsLocked(ns), environmentIsLocked(parent.env(ns)))
  stopifnot(!exists(".__NAMESPACE__.", envir = ns, inherits = FALSE))
  stopifnot(!("voucher" %in% loadedNamespaces()))
}

x <- readRDS(commandArgs(TRUE)[[1L]])
check_voucher(x$voucher_ns)

lazy <- new.env(parent = emptyenv())
lazyLoad(commandArgs(TRUE)[[2L]], lazy)
check_voucher(lazy$.hermetic$voucher_ns)
"#;
    let check_path = work.join("check-runtime.R");
    fs::write(&check_path, check)?;
    let status = Command::new(toolchain.rscript())
        .env("R_ENVIRON_USER", "")
        .env("R_PROFILE_USER", "")
        .env("R_LIBS_USER", "")
        .env("R_DEFAULT_PACKAGES", "NULL")
        .arg("--vanilla")
        .arg(&check_path)
        .arg(&artifact.baseline_rds)
        .arg(artifact.lazy_db_base.as_ref().expect("lazy db base"))
        .status()?;
    assert!(status.success());

    fs::remove_dir_all(work)?;
    Ok(())
}
