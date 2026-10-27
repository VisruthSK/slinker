mod common;

use common::{assert_success, user_library_arguments};
use std::path::Path;
use std::process::Command;

const THREAD_COUNTS: [&str; 4] = ["1", "2", "5", "16"];

fn analyze(package: &str, threads: &str, cache: &Path) -> Vec<u8> {
    let output = Command::new(env!("CARGO_BIN_EXE_slinker"))
        .args(["analyze", package, "--json", "--threads", threads])
        .args(user_library_arguments())
        .env("SLINKER_CACHE_DIR", cache)
        .output()
        .expect("run slinker");
    assert_success(&output, &format!("analyze {package} --threads {threads}"));
    output.stdout
}

fn assert_independent_of_threads_and_cache(package: &str) {
    let cache = tempfile::tempdir().expect("cache directory");
    let reference = analyze(package, THREAD_COUNTS[0], cache.path());
    assert!(!reference.is_empty());
    for threads in &THREAD_COUNTS[1..] {
        let fresh = tempfile::tempdir().expect("cache directory");
        assert!(
            reference == analyze(package, threads, fresh.path()),
            "{package} cold analysis differs between --threads 1 and --threads {threads}"
        );
    }
    for threads in THREAD_COUNTS {
        assert!(
            reference == analyze(package, threads, cache.path()),
            "{package} warm-cache analysis with --threads {threads} differs from the cold --threads 1 analysis"
        );
    }
}

#[test]
fn compiler_analysis_is_independent_of_the_thread_count_and_the_cache() {
    assert_independent_of_threads_and_cache("compiler");
}

#[test]
fn grid_analysis_is_independent_of_the_thread_count_and_the_cache() {
    assert_independent_of_threads_and_cache("grid");
}

#[test]
fn rlang_analysis_is_independent_of_the_thread_count_and_the_cache() {
    assert_independent_of_threads_and_cache("rlang");
}

#[test]
fn testthat_analysis_is_independent_of_the_thread_count_and_the_cache() {
    assert_independent_of_threads_and_cache("testthat");
}
