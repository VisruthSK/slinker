use slinker_core::analysis::{ANALYSIS_STACK_BYTES, Linker};
use slinker_core::cache::CacheLocation;
use slinker_core::package::PackageStore;
use slinker_core::{TargetEnvironmentRequest, profile};
use std::path::{Path, PathBuf};
use std::process::Command;

fn discover_r_home() -> PathBuf {
    if let Some(home) = std::env::var_os("R_HOME") {
        return home.into();
    }
    let output = if cfg!(windows) {
        Command::new("cmd").args(["/c", "R RHOME"]).output()
    } else {
        Command::new("R").arg("RHOME").output()
    }
    .expect("R RHOME");
    let home = String::from_utf8(output.stdout).expect("R home UTF-8");
    let line = home
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .expect("R home line");
    dunce::canonicalize(line.trim()).expect("canonical R home")
}

fn analyze(root: &str) {
    let r_home = discover_r_home();
    let mut request = TargetEnvironmentRequest::new(r_home.clone());
    if let Some(libraries) = std::env::var_os("R_LIBS_USER") {
        request.libraries = std::env::split_paths(&libraries).collect();
    }
    let target = request
        .capture()
        .expect("capture the target R library universe");
    let jobs = std::thread::available_parallelism().map_or(1, usize::from);
    let plan = PackageStore::new(r_home, target, CacheLocation::Disabled)
        .and_then(|store| Linker::new(store, jobs).analyze(root))
        .unwrap_or_else(|error| panic!("analyze {root}: {error}"));
    println!(
        "bindings {}  construction evaluations {}  blockers {}",
        plan.program().bindings().len(),
        plan.construction_evaluations(),
        plan.blockers().len()
    );
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let arguments = std::env::args_os().collect::<Vec<_>>();
    if arguments
        .get(1)
        .is_some_and(|argument| argument == "__r-worker")
    {
        let protocol = arguments.get(2).ok_or("missing worker protocol path")?;
        return Ok(slinker_r_worker::run(Path::new(protocol))?);
    }
    let root = arguments.get(1).map_or_else(
        || "rlang".into(),
        |root| root.to_string_lossy().into_owned(),
    );
    profile::enable();
    std::thread::Builder::new()
        .stack_size(ANALYSIS_STACK_BYTES)
        .spawn(move || {
            analyze(&root);
            if let Some(report) = profile::report() {
                print!("{report}");
            }
        })?
        .join()
        .map_err(|_| "profile run failed")?;
    Ok(())
}
