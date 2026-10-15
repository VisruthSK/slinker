use slinker_core::package::tree_digest;

#[test]
fn different_file_trees_cannot_share_a_fingerprint_encoding() {
    let one = tempfile::tempdir().unwrap();
    let two = tempfile::tempdir().unwrap();
    std::fs::write(one.path().join("a"), b"X\xffb\0Y").unwrap();
    std::fs::write(two.path().join("a"), b"X").unwrap();
    std::fs::write(two.path().join("b"), b"Y").unwrap();
    assert_ne!(
        tree_digest(one.path()).unwrap(),
        tree_digest(two.path()).unwrap()
    );
}
