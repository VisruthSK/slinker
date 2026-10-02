mod common;

use slinker_core::TargetEnvironmentRequest;
use slinker_core::analysis::{ANALYSIS_STACK_BYTES, ExplanationDag, Linker, Schedule};
use slinker_core::cache::CacheLocation;
use slinker_core::package::PackageStore;
use std::path::Path;

const PACKAGES: [&str; 2] = ["compiler", "grid"];
const JOB_COUNTS: [usize; 3] = [1, 3, 16];
const SCHEDULES: [Schedule; 6] = [
    Schedule::Fifo,
    Schedule::Lifo,
    Schedule::Seeded(1),
    Schedule::Seeded(2),
    Schedule::Seeded(3),
    Schedule::Seeded(0xdead_beef),
];

fn rendered(r_home: &Path, package: &str, jobs: usize, schedule: Schedule) -> String {
    let mut request = TargetEnvironmentRequest::new(r_home.to_path_buf());
    if let Some(libraries) = std::env::var_os("R_LIBS_USER") {
        request.libraries = std::env::split_paths(&libraries).collect();
    }
    let target = request.capture().expect("capture the target R libraries");
    let store = PackageStore::new(
        r_home.to_path_buf(),
        target.clone(),
        CacheLocation::Disabled,
    )
    .expect("package store");
    let plan = Linker::new(store, jobs)
        .with_schedule(schedule)
        .analyze(package)
        .unwrap_or_else(|error| panic!("analyze {package}: {error}"));
    let graph = ExplanationDag::from_plan(&plan, &target, package).expect("explanation graph");
    serde_json::to_string(&graph).expect("serialize explanation graph")
}

fn run() {
    let r_home = common::discover_r_home();
    for package in PACKAGES {
        let reference = rendered(&r_home, package, JOB_COUNTS[0], SCHEDULES[0]);
        for jobs in JOB_COUNTS {
            for schedule in SCHEDULES {
                assert!(
                    reference == rendered(&r_home, package, jobs, schedule),
                    "{package} analysis at quiescence differs with --jobs {jobs} under {schedule:?}"
                );
            }
        }
        println!(
            "{package}: identical for {} job counts x {} schedules",
            JOB_COUNTS.len(),
            SCHEDULES.len()
        );
    }
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
    std::thread::Builder::new()
        .stack_size(ANALYSIS_STACK_BYTES)
        .spawn(run)?
        .join()
        .map_err(|_| "schedule determinism check failed")?;
    Ok(())
}
