use super::*;
use crate::Description;
use crate::package::{BindingNames, Digest, LifecycleMetadata, NativeComponent, NativeLibrary};

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
