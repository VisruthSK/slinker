use std::env;
use std::io;
use std::path::PathBuf;

use hrm::{
    MaterializationRequest, PackageMaterialization, RToolchain, StageRequest,
    TargetEnvironmentRequest,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let r = env::var_os("HRM_R").ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "set HRM_R to the target R executable"))?;
    let toolchain = RToolchain::from_r(r);

    let target = toolchain.capture_target_environment(&TargetEnvironmentRequest::new(
        "target/hrm/target",
    ))?;

    let mut stage_request = StageRequest::new("vendor/foo", "target/hrm/stage");
    stage_request.expected_target = Some(target.target.clone());
    stage_request.external_libraries = target.libraries.clone();
    let staged = toolchain.stage(&stage_request)?;
    let snapshot = toolchain.inspect_staged_snapshot(&staged)?;

    let package = PackageMaterialization::from_snapshot(&snapshot, ["needed", "helper"])?;

    let mut request = MaterializationRequest::new(PathBuf::from("target/hrm/baseline.rds"));
    request.packages.push(package);
    request.target = Some(target.target.clone());
    let artifact = toolchain.materialize(&request)?;

    println!("{}", artifact.baseline_rds.display());
    if let Some(rdb) = artifact.lazy_rdb { println!("{}", rdb.display()); }
    Ok(())
}
