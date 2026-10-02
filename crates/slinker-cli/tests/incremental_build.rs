mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::SystemTime;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

fn copy_directory(from: &Path, to: &Path) {
    fs::create_dir_all(to).expect("create directory");
    for entry in fs::read_dir(from).expect("list directory").flatten() {
        let target = to.join(entry.file_name());
        if entry.path().is_dir() {
            copy_directory(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).expect("copy file");
        }
    }
}

fn build(source: &Path, output: &Path, cache: &Path) -> Output {
    let result = Command::new(env!("CARGO_BIN_EXE_slinker"))
        .arg("build")
        .arg(source)
        .arg("--output")
        .arg(output)
        .env("SLINKER_CACHE_DIR", cache)
        .output()
        .expect("run slinker build");
    common::assert_success(&result, "slinker build");
    result
}

fn up_to_date(result: &Output) -> bool {
    String::from_utf8_lossy(&result.stderr).contains("up to date")
}

fn modified(path: &Path) -> SystemTime {
    fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .expect("modification time")
}

fn tree(root: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut files = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory).expect("list").flatten() {
            if entry.path().is_dir() {
                pending.push(entry.path());
            } else {
                files.push((
                    entry
                        .path()
                        .strip_prefix(root)
                        .expect("relative")
                        .to_path_buf(),
                    fs::read(entry.path()).expect("read"),
                ));
            }
        }
    }
    files.sort();
    files
}

#[test]
fn unchanged_rebuild_is_skipped_and_an_edit_rebuilds_only_what_changed() {
    let work = tempfile::tempdir().expect("work directory");
    let source = work.path().join("pkgconfig");
    copy_directory(&fixture("pkgconfig"), &source);
    let output = work.path().join("out");
    let cache = work.path().join("cache");

    let first = build(&source, &output, &cache);
    assert!(!up_to_date(&first));
    let built = tree(&output);
    let description = modified(&output.join("DESCRIPTION"));

    std::thread::sleep(std::time::Duration::from_millis(50));
    let second = build(&source, &output, &cache);
    assert!(
        up_to_date(&second),
        "{}",
        String::from_utf8_lossy(&second.stderr)
    );
    assert_eq!(tree(&output), built);
    assert_eq!(modified(&output.join("DESCRIPTION")), description);

    let entry = fs::read_dir(source.join("R"))
        .expect("R directory")
        .flatten()
        .map(|entry| entry.path())
        .min()
        .expect("a source file");
    let mut text = fs::read_to_string(&entry).expect("read source");
    text.push_str("\n.slinker_edit <- function() 1\n");
    fs::write(&entry, text).expect("edit source");

    let third = build(&source, &output, &cache);
    assert!(!up_to_date(&third));
    assert_ne!(tree(&output), built);
    assert_eq!(
        modified(&output.join("DESCRIPTION")),
        description,
        "files the edit did not change keep their modification time"
    );

    let fourth = build(&source, &output, &cache);
    assert!(up_to_date(&fourth));
}

#[test]
fn a_foreign_directory_is_never_replaced_by_a_build() {
    let work = tempfile::tempdir().expect("work directory");
    let source = work.path().join("pkgconfig");
    copy_directory(&fixture("pkgconfig"), &source);
    let output = work.path().join("out");
    fs::create_dir_all(&output).expect("create foreign directory");
    fs::write(output.join("keep.txt"), "mine").expect("write file");

    let result = Command::new(env!("CARGO_BIN_EXE_slinker"))
        .arg("build")
        .arg(&source)
        .arg("--output")
        .arg(&output)
        .env("SLINKER_CACHE_DIR", work.path().join("cache"))
        .output()
        .expect("run slinker build");
    assert!(!result.status.success());
    assert_eq!(
        fs::read_to_string(output.join("keep.txt")).expect("kept"),
        "mine"
    );
}
