use super::*;

#[test]
fn snapshot_is_immutable_after_source_changes() {
    let source = tempfile::tempdir().expect("source tempdir");
    fs::create_dir(source.path().join("R")).expect("R directory");
    fs::write(
        source.path().join("DESCRIPTION"),
        "Package: fixture\nVersion: 1.0.0\n",
    )
    .expect("DESCRIPTION");
    fs::write(source.path().join("NAMESPACE"), "export(f)\n").expect("NAMESPACE");
    fs::write(source.path().join("R/f.R"), "f <- function() 1L\n").expect("R source");
    let snapshot = SourcePackageSnapshot::capture(source.path()).expect("snapshot");

    fs::write(source.path().join("R/f.R"), "f <- function() 2L\n").expect("mutate source");

    assert_eq!(
        fs::read_to_string(snapshot.files().root().join("R/f.R")).expect("frozen source"),
        "f <- function() 1L\n"
    );
}

fn write_package(root: &Path) {
    fs::create_dir(root.join("R")).expect("R directory");
    fs::write(
        root.join("DESCRIPTION"),
        "Package: fixture\nVersion: 1.0.0\n",
    )
    .expect("DESCRIPTION");
    fs::write(root.join("NAMESPACE"), "export(f)\n").expect("NAMESPACE");
    fs::write(root.join("R/f.R"), "f <- function() 1L\n").expect("R source");
}

#[test]
fn snapshot_skips_vcs_build_output_and_rbuildignore_entries() {
    let source = tempfile::tempdir().expect("source tempdir");
    write_package(source.path());
    for directory in [".git", "target", "renv", "notes"] {
        fs::create_dir(source.path().join(directory)).expect("excluded directory");
        fs::write(source.path().join(directory).join("file"), "x").expect("excluded file");
    }
    fs::write(source.path().join("scratch.R"), "x").expect("ignored file");
    fs::write(source.path().join("R/keep_scratch.R"), "y <- 1\n").expect("kept file");
    fs::write(
        source.path().join(".Rbuildignore"),
        "^notes$\n\n^SCRATCH\\.R$\n",
    )
    .expect(".Rbuildignore");

    let snapshot = SourcePackageSnapshot::capture(source.path()).expect("snapshot");

    let root = snapshot.files().root();
    for excluded in [".git", "target", "renv", "notes", "scratch.R"] {
        assert!(!root.join(excluded).exists(), "{excluded} was copied");
    }
    assert!(root.join("R/keep_scratch.R").is_file());
    assert!(root.join(".Rbuildignore").is_file());
}

#[test]
fn snapshot_rejects_unparseable_rbuildignore_pattern() {
    let source = tempfile::tempdir().expect("source tempdir");
    write_package(source.path());
    fs::write(source.path().join(".Rbuildignore"), "(unclosed\n").expect(".Rbuildignore");

    let error = SourcePackageSnapshot::capture(source.path()).expect_err("invalid pattern");

    assert!(matches!(
        error,
        SourcePackageError::InvalidBuildIgnore { .. }
    ));
}
