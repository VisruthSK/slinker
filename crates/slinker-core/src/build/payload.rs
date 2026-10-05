use super::BuildReport;
use super::relocated::RelocatedCode;
use crate::ir::{ClosureHome, NamespaceId, PayloadBundleId, PayloadBundleIr, ProgramIr};
use crate::worker::protocol::{
    ClosurePatchSpec, PayloadSerialization, PayloadSite, SerializedPayload,
};
use std::collections::BTreeSet;

#[derive(Debug)]
pub(super) struct CheckedPayloadBundle {
    pub(super) bundle: PayloadBundleId,
    pub(super) bytes: Vec<u8>,
}

fn registered_name(program: &ProgramIr, namespace: NamespaceId) -> &str {
    program
        .package(program.namespace(namespace).package)
        .registered_namespace()
        .as_str()
}

#[cfg(test)]
#[path = "../../tests/unit/build/payload.rs"]
mod tests;

pub(super) fn check_payload_bundles(
    program: &ProgramIr,
    serialization: PayloadSerialization,
) -> Result<Vec<CheckedPayloadBundle>, BuildReport> {
    let bundles = program.payload_bundles();
    let package_name = |bundle: &PayloadBundleIr| {
        &program
            .package(program.namespace(bundle.namespace()).package)
            .identity()
            .name
    };
    let site = |site: &PayloadSite| {
        bundles
            .get(site.payload)
            .map(|bundle| format!("{}::{}", package_name(bundle), site.binding))
            .ok_or_else(|| {
                BuildReport::preflight(vec![
                    "worker returned an invalid payload identity index".into(),
                ])
            })
    };
    let serialized = match serialization {
        PayloadSerialization::SharedIdentity { first, second } => {
            return Err(BuildReport::preflight(vec![format!(
                "payload `{}` and payload `{}` reach one environment or reference object, which separate namespace bundles would split into two",
                site(&first)?,
                site(&second)?
            )]));
        }
        PayloadSerialization::Serialized { bundles } => bundles,
    };
    if serialized.len() != bundles.len() {
        return Err(BuildReport::preflight(vec![format!(
            "worker returned {} payload bundles, expected {}",
            serialized.len(),
            bundles.len()
        )]));
    }
    let mut blockers = Vec::new();
    let mut checked = Vec::new();
    for ((id, bundle), SerializedPayload { bytes, namespaces }) in
        program.indexed_payload_bundles().zip(serialized)
    {
        let owner = registered_name(program, bundle.namespace());
        let package = package_name(bundle);
        let established = bundle
            .dependencies()
            .iter()
            .map(|dependency| registered_name(program, dependency.namespace()))
            .collect::<BTreeSet<_>>();
        let observed = namespaces
            .iter()
            .map(String::as_str)
            .filter(|namespace| *namespace != owner)
            .collect::<BTreeSet<_>>();
        blockers.extend(observed.difference(&established).map(|namespace| {
            format!(
                "payload bundle of `{package}` refers to namespace `{namespace}`, which analysis did not establish as a dependency"
            )
        }));
        blockers.extend(established.difference(&observed).map(|namespace| {
            format!(
                "payload bundle of `{package}` does not refer to namespace `{namespace}`, which analysis established as a dependency"
            )
        }));
        checked.push(CheckedPayloadBundle { bundle: id, bytes });
    }
    if blockers.is_empty() {
        Ok(checked)
    } else {
        Err(BuildReport::preflight(blockers))
    }
}

pub(super) fn closure_patches(
    program: &ProgramIr,
    bundle: &PayloadBundleIr,
    relocated: &RelocatedCode,
) -> Vec<ClosurePatchSpec> {
    bundle
        .closure_patches()
        .iter()
        .map(|closure| {
            let code = program.code(closure.code);
            let source = relocated.source(closure.code);
            let (root, steps) = match &closure.home {
                ClosureHome::Namespace => (None, Vec::new()),
                ClosureHome::Reached { root, steps } => (Some(root.clone()), steps.clone()),
            };
            ClosurePatchSpec {
                root,
                steps,
                binding: closure.binding.clone(),
                expected_shape: code.normalized_shape().clone(),
                source: code
                    .assigned_value_start()
                    .map_or(source, |start| &source[start..])
                    .to_owned(),
            }
        })
        .collect()
}
