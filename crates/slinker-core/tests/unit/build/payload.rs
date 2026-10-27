use super::*;
use crate::ir::{
    MaterializedRole, MaterializedSlot, MaterializedSlotSource, RootArtifactIr, TargetContract,
};
use crate::package::{Digest, PackageId, PackageIdentity};

fn program() -> ProgramIr {
    let root = PackageId::from_index(0);
    let mut builder = ProgramIr::builder(
        TargetContract {
            r_version: "4.6.1".into(),
            platform: "windows".into(),
            arch: "x86_64".into(),
        },
        root,
        PackageIdentity {
            name: "root".into(),
            version: "1.0.0".parse().unwrap(),
            image_fingerprint: Digest::of(b"fixture"),
        },
    );
    let namespace = builder
        .finish_materialized_namespace(
            root,
            MaterializedRole::Root,
            [MaterializedSlot {
                name: "value".into(),
                source: MaterializedSlotSource::Payload,
            }],
        )
        .namespace;
    let load = builder.root_load(namespace);
    builder.finish(RootArtifactIr {
        description: None,
        exports: Default::default(),
        native_components: Vec::new(),
        on_load: None,
        load,
    })
}

#[test]
fn missing_or_extra_worker_payloads_cannot_pass_preflight() {
    let program = program();
    for count in [0, 2] {
        let bundles = (0..count)
            .map(|_| SerializedPayload {
                bytes: Vec::new(),
                namespaces: Vec::new(),
            })
            .collect();
        assert!(
            check_payload_bundles(&program, PayloadSerialization::Serialized { bundles }).is_err()
        );
    }
}

#[test]
fn malformed_worker_identity_indices_are_errors_instead_of_panics() {
    let program = program();
    let invalid = PayloadSite {
        payload: usize::MAX,
        binding: "value".into(),
    };
    assert!(
        check_payload_bundles(
            &program,
            PayloadSerialization::SharedIdentity {
                first: invalid.clone(),
                second: invalid
            }
        )
        .is_err()
    );
}
