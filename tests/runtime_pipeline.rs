use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use hrm::{
    MaterializationRequest, PackageMaterialization, RToolchain, StageRequest,
    TargetEnvironmentRequest,
};

fn scratch() -> PathBuf {
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    env::temp_dir().join(format!("hrm-{}-{nonce}", std::process::id()))
}

#[test]
#[ignore = "requires HRM_R and HRM_VOUCHER_SOURCE pointing at a real voucher checkout"]
fn stages_inspects_and_materializes_real_voucher_source() -> Result<(), Box<dyn std::error::Error>> {
    let r = PathBuf::from(env::var("HRM_R")?);
    let voucher_source = PathBuf::from(env::var("HRM_VOUCHER_SOURCE")?);
    let toolchain = RToolchain::from_r(&r);
    let work = scratch();
    fs::create_dir_all(&work)?;

    if !voucher_source.join("DESCRIPTION").is_file() || !voucher_source.join("NAMESPACE").is_file() {
        return Err(format!(
            "HRM_VOUCHER_SOURCE must point at an unpacked voucher source tree; missing DESCRIPTION/NAMESPACE under {}",
            voucher_source.display()
        )
        .into());
    }

    let target = toolchain.capture_target_environment(&TargetEnvironmentRequest::new(work.join("target")))?;
    for dependency in ["cli", "fs"] {
        if target.package(dependency).is_none() {
            return Err(format!(
                "voucher imports {dependency}; install it into the target R library before running this integration test"
            )
            .into());
        }
    }

    let mut stage_request = StageRequest::new(&voucher_source, work.join("stage"));
    stage_request.use_target_environment(&target);
    let prepared = toolchain.prepare_package(&stage_request)?;
    let staged = &prepared.staged;

    assert_eq!(staged.name, "voucher");
    assert!(staged.configured.effective.description.contains("Package: voucher"));
    assert!(staged.configured.effective.namespace.contains("export(check)"));
    assert!(prepared.source.iter().any(|file| !file.facts.is_empty()));

    let snapshot = &prepared.semantic;
    assert_eq!(snapshot.state.package, "voucher");
    assert!(snapshot.state.activation_clean);
    assert!(!snapshot.state.has_on_load);

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

    // This is intentionally a small, real closed-world slice of voucher. The
    // automatic reachability builder is not implemented yet, so the retained
    // set is stated explicitly for this vertical-slice integration test.
    let package = PackageMaterialization::from_snapshot(
        snapshot,
        ["vouch_split_handle", "vouch_parse_line", "vouch_entry_matches_target"],
    )?;
    let mut materialization = MaterializationRequest::new(work.join("baseline.rds"));
    materialization.packages.push(package);
    materialization.use_target_environment(&target);
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
