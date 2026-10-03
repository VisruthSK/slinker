mod common;

use common::{assert_success, user_library_arguments};
use std::path::Path;
use std::process::Command;

const JOB_COUNTS: [&str; 4] = ["1", "2", "5", "16"];

fn analyze(package: &str, jobs: &str, cache: &Path) -> Vec<u8> {
    let output = Command::new(env!("CARGO_BIN_EXE_slinker"))
        .args(["analyze", package, "--json", "--jobs", jobs])
        .args(user_library_arguments())
        .env("SLINKER_CACHE_DIR", cache)
        .output()
        .expect("run slinker");
    assert_success(&output, &format!("analyze {package} --jobs {jobs}"));
    output.stdout
}

fn assert_independent_of_jobs_and_cache(package: &str) {
    let cache = tempfile::tempdir().expect("cache directory");
    let reference = analyze(package, JOB_COUNTS[0], cache.path());
    assert!(!reference.is_empty());
    for jobs in &JOB_COUNTS[1..] {
        let fresh = tempfile::tempdir().expect("cache directory");
        assert!(
            reference == analyze(package, jobs, fresh.path()),
            "{package} cold analysis differs between --jobs 1 and --jobs {jobs}"
        );
    }
    for jobs in JOB_COUNTS {
        assert!(
            reference == analyze(package, jobs, cache.path()),
            "{package} warm-cache analysis with --jobs {jobs} differs from the cold --jobs 1 analysis"
        );
    }
}

#[test]
fn compiler_analysis_is_independent_of_the_job_count_and_the_cache() {
    assert_independent_of_jobs_and_cache("compiler");
}

#[test]
fn grid_analysis_is_independent_of_the_job_count_and_the_cache() {
    assert_independent_of_jobs_and_cache("grid");
}

#[test]
fn rlang_analysis_is_independent_of_the_job_count_and_the_cache() {
    assert_independent_of_jobs_and_cache("rlang");
}

#[test]
fn testthat_analysis_is_independent_of_the_job_count_and_the_cache() {
    assert_independent_of_jobs_and_cache("testthat");
}
