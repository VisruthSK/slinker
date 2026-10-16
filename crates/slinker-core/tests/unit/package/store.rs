use super::*;
use crate::Description;
use crate::package::{BindingNames, Digest, LifecycleMetadata, NativeComponent, NativeLibrary};

#[test]
fn malformed_object_observations_do_not_become_package_images() {
    use crate::package::{
        BindingImage, BindingOrigin, BindingRepresentation, EmbeddedClosureSource, MemberPath,
        MemberStep, ObjectKind,
    };
    let mut index = package_index();
    index.binding_names = vec!["value".into()].into();
    let identity = index.identity.clone();
    let index = Arc::new(index);
    let mut worker = WorkerBinding {
        package_name: identity.name.clone(),
        package_version: identity.version.to_string(),
        image_fingerprint: identity.image_fingerprint.clone(),
        binding: BindingImage {
            name: "value".into(),
            origin: BindingOrigin::Code,
            object: ObjectImage::of_kind(BindingRepresentation::Value, ObjectKind::Environment),
        },
        private_environments: HashMap::new(),
        normalizations: Vec::new(),
    };
    let check =
        |worker| PackageStore::package_image(&identity, Arc::clone(&index), worker, HashMap::new());
    assert!(
        check(worker.clone()).is_err(),
        "environment has no identity"
    );
    worker.binding.object.environment = Some(EnvironmentLabel::private("missing"));
    assert!(
        check(worker.clone()).is_err(),
        "private environment was not delivered"
    );
    worker.binding.object = ObjectImage::of_kind(BindingRepresentation::Value, ObjectKind::List);
    let closure = EmbeddedClosureSource {
        path: MemberPath::root().child(MemberStep::Element(1.try_into().unwrap())),
        source: "f <- function() 1L".into(),
        environment: EnvironmentLabel::namespace("fixture"),
    };
    worker.binding.object.embedded_closures = vec![closure.clone(), closure];
    assert!(
        check(worker).is_err(),
        "two closures occupy the same structural location"
    );
    assert!(serde_json::from_str::<EnvironmentLabel>("\"private:\"").is_err());
    assert!(serde_json::from_str::<EnvironmentLabel>("\"worker:1\"").is_err());
    assert!(serde_json::from_str::<MemberPath>("[{\"Element\":0}]").is_err());
    assert!(serde_json::from_str::<MemberPath>("\"$[[1]]\"").is_err());
}

fn package_index() -> PackageIndex {
    PackageIndex {
        identity: PackageIdentity {
            name: "fixture".into(),
            version: "1.0.0".parse().expect("version"),
            image_fingerprint: Digest::from("exact-image"),
        },
        description: Description::parse("Package: fixture\nVersion: 1.0.0\n"),
        exports: Default::default(),
        imports: Vec::new(),
        s3: Vec::new(),
        dynlibs: vec![NativeComponent {
            name: "fixture".into(),
            alias: String::new(),
            registration: None,
            symbols: Vec::new(),
            library: NativeLibrary::Missing,
            safety: NativeSafety::Unanalyzed,
        }],
        lifecycle: LifecycleMetadata::default(),
        binding_names: BindingNames::default(),
        data: PackageData::default(),
        files: Vec::new(),
        has_sysdata: false,
    }
}

#[test]
fn exact_image_native_manifest_attaches_routine_callbacks() {
    let manifest: NativeSummaryManifest = serde_json::from_str(
        r#"{
            "schema": 1,
            "packages": [{
                "package": "fixture",
                "version": "1.0.0",
                "origin": "installed",
                "image_fingerprint": "exact-image",
                "components": [{
                    "component": "fixture",
                    "safety": "summarized",
                    "routines": [{"selector": "fixture_call", "callback_arguments": [2]}]
                }]
            }]
        }"#,
    )
    .expect("manifest");
    manifest.validate().expect("valid manifest");
    let mut index = package_index();
    manifest.apply(&mut index);

    assert!(matches!(
        &index.dynlibs[0].safety,
        NativeSafety::Summarized(routines)
            if routines == &[NativeRoutineSummary {
                selector: "fixture_call".into(),
                callback_arguments: vec![2],
            }]
    ));
}

#[test]
fn native_manifest_rejects_zero_callback_position() {
    let manifest: NativeSummaryManifest = serde_json::from_str(
        r#"{
            "schema": 1,
            "packages": [{
                "package": "fixture",
                "version": "1.0.0",
                "origin": "installed",
                "image_fingerprint": "exact-image",
                "components": [{
                    "component": "fixture",
                    "safety": "summarized",
                    "routines": [{"selector": "fixture_call", "callback_arguments": [0]}]
                }]
            }]
        }"#,
    )
    .expect("manifest");
    assert!(manifest.validate().is_err());
}
