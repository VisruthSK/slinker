use super::*;

#[test]
fn image_fingerprint_reads_current_bytes_even_when_length_is_unchanged() {
    let root = tempfile::tempdir().expect("fixture root");
    let path = root.path().join("object.rdb");
    fs::write(&path, b"before").expect("write first image");
    let before = fingerprint_image(root.path()).expect("fingerprint first image");
    fs::write(&path, b"after!").expect("rewrite same-length image");
    let after = fingerprint_image(root.path()).expect("fingerprint changed image");

    assert_ne!(before, after);
}

#[test]
fn fingerprint_fields_are_unambiguous() {
    let split = |left: &str, right: &str| Fingerprint::new("t").field(left).field(right).finish();
    assert_ne!(split("ab", "c"), split("a", "bc"));
    let listed = |items: &[&str]| Fingerprint::new("t").list(items.iter().copied()).finish();
    assert_ne!(listed(&["a", "b"]), listed(&["b", "a"]));
    assert_ne!(listed(&["a"]), listed(&["a", ""]));
    assert_eq!(listed(&["a", "b"]), listed(&["a", "b"]));
}

#[test]
fn identity_excludes_physical_location() {
    let first = tempfile::tempdir().expect("first library");
    let second = tempfile::tempdir().expect("second library");
    for library in [first.path(), second.path()] {
        let root = library.join("fixture");
        fs::create_dir(&root).expect("package root");
        fs::write(
            root.join("DESCRIPTION"),
            "Package: fixture\nVersion: 1.0.0\n",
        )
        .expect("DESCRIPTION");
    }
    let locate = |library: &Path| {
        PackageLocator::new(TargetEnvironment {
            r_home: PathBuf::new(),
            target: crate::Target {
                r_version: String::new(),
                os: String::new(),
                arch: String::new(),
            },
            libraries: vec![library.to_path_buf()],
            base_bindings: Default::default(),
        })
        .locate("fixture")
        .expect("locate fixture")
        .expect("fixture present")
    };

    let (first, second) = (locate(first.path()), locate(second.path()));

    assert_eq!(first.identity, second.identity);
    assert_ne!(first.location, second.location);
}
