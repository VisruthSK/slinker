use super::*;
use crate::package::PackageLocator;
use crate::{Target, TargetEnvironment};
use std::fs;
use std::path::Path;
use std::sync::Mutex;

struct CountingStore {
    locator: PackageLocator,
    located: Mutex<Vec<String>>,
}

impl PackageResolver for CountingStore {
    fn target_environment(&self) -> &TargetEnvironment {
        self.locator.target()
    }

    fn locate(&self, name: &str) -> Result<Option<InstalledPackage>> {
        self.located.lock().expect("located").push(name.to_owned());
        self.locator.locate(name)
    }
}

fn install(library: &Path, name: &str) {
    let root = library.join(name);
    fs::create_dir_all(&root).expect("package root");
    fs::write(
        root.join("DESCRIPTION"),
        format!("Package: {name}\nVersion: 1.0.0\n"),
    )
    .expect("DESCRIPTION");
}

fn universe(library: &Path) -> TargetUniverse<CountingStore> {
    universe_with_policy(library, "unused-root", HashSet::new())
}

fn universe_with_policy(
    library: &Path,
    root: &str,
    explicit_external: HashSet<PackageName>,
) -> TargetUniverse<CountingStore> {
    TargetUniverse::new(
        CountingStore {
            locator: PackageLocator::new(TargetEnvironment {
                r_home: library.to_path_buf(),
                target: Target {
                    r_version: String::new(),
                    os: String::new(),
                    arch: String::new(),
                },
                libraries: vec![library.to_path_buf()],
                base_bindings: Default::default(),
            }),
            located: Mutex::new(Vec::new()),
        },
        root,
        explicit_external,
    )
}

#[test]
fn absence_is_frozen_for_the_invocation() {
    let library = tempfile::tempdir().expect("library");
    let universe = universe(library.path());

    assert_eq!(universe.resolve("late").expect("first answer"), None);
    install(library.path(), "late");

    assert_eq!(universe.resolve("late").expect("frozen answer"), None);
    assert_eq!(*universe.store.located.lock().unwrap(), ["late"]);
    assert_eq!(
        universe.availability("late"),
        Some(PackageAvailability::Absent)
    );
}

#[test]
fn package_ids_are_allocated_per_invocation_in_resolution_order() {
    let library = tempfile::tempdir().expect("library");
    install(library.path(), "first");
    install(library.path(), "second");
    let forward = universe(library.path());
    let reverse = universe(library.path());

    let forward_first = forward.require("first").expect("first");
    reverse.require("second").expect("second");
    let reverse_first = reverse.require("first").expect("first");

    assert_ne!(forward_first, reverse_first);
    assert_eq!(
        forward.identity(forward_first),
        reverse.identity(reverse_first)
    );
    assert_eq!(forward.require("first").expect("memoized"), forward_first);
}

#[test]
fn concurrent_resolution_of_one_new_package_agrees_on_a_single_id() {
    let library = tempfile::tempdir().expect("library");
    install(library.path(), "shared");
    let universe = universe(library.path());

    let ids = std::thread::scope(|scope| {
        let handles = (0..8)
            .map(|_| scope.spawn(|| universe.require("shared").expect("shared")))
            .collect::<Vec<_>>();
        handles
            .into_iter()
            .map(|handle| handle.join().expect("resolver"))
            .collect::<Vec<_>>()
    });

    assert!(ids.iter().all(|id| *id == ids[0]));
    assert_eq!(universe.require("shared").expect("memoized"), ids[0]);
}

#[test]
fn roles_follow_the_policy_given_at_construction() {
    let library = tempfile::tempdir().expect("library");
    for name in ["root", "dependency", "kept"] {
        install(library.path(), name);
    }
    let universe = universe_with_policy(library.path(), "root", HashSet::from(["kept".into()]));

    let roles = ["root", "dependency", "kept"].map(|name| {
        let package = universe.require(name).expect("installed");
        universe.role(package)
    });

    assert_eq!(
        roles,
        [
            PackageRole::Root,
            PackageRole::Linked,
            PackageRole::External
        ]
    );
}

#[test]
fn changed_selected_image_is_detected() {
    let library = tempfile::tempdir().expect("library");
    install(library.path(), "fixture");
    let universe = universe(library.path());
    let fixture = universe.require("fixture").expect("fixture");
    let sources = universe.sources([fixture]);
    assert_eq!(sources.changed().expect("unchanged"), None);

    fs::write(library.path().join("fixture/R"), "mutated").expect("mutate image");

    assert_eq!(
        sources
            .changed()
            .expect("fingerprint")
            .map(|identity| identity.name.as_str()),
        Some("fixture")
    );
}
