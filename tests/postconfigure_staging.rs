use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use hrm::{RToolchain, StageRequest, TargetEnvironmentRequest};

fn scratch() -> PathBuf {
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    env::temp_dir().join(format!("hrm-config-{}-{nonce}", std::process::id()))
}

#[test]
#[ignore = "requires HRM_R pointing at a real target R executable"]
fn captures_real_package_post_configure_source() -> Result<(), Box<dyn std::error::Error>> {
    let r = PathBuf::from(env::var("HRM_R")?);
    let toolchain = RToolchain::from_r(&r);
    let work = scratch();
    fs::create_dir_all(&work)?;

    let target = toolchain.capture_target_environment(&TargetEnvironmentRequest::new(work.join("target")))?;
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/postconfigurepkg");
    let mut request = StageRequest::new(fixture, work.join("stage"));
    request.use_target_environment(&target);
    let staged = toolchain.stage(&request)?;

    assert!(staged.configured.effective.description.contains("Config/hrm: configured"));
    assert!(staged.configured.effective.namespace.contains("export(public)"));
    assert!(staged.configured.files.iter().any(|path| path == Path::new("inst/hrm-configured.txt")));

    fs::remove_dir_all(work)?;
    Ok(())
}
