use super::runtime::WorkerRuntime;
use super::{InspectionError, WorkerOperationError, protocol};
use super::{InspectionResult, OperationResult};
use harp::{RFunctionExt, RObjectExt};
use protocol::WorkerErrorCode;
use std::collections::{HashMap, HashSet};

impl WorkerRuntime {
    pub(super) fn serialize_payloads(
        &mut self,
        namespaces: &[protocol::NamespaceImageSpec],
        payloads: &[protocol::PayloadSpec],
    ) -> OperationResult<protocol::PayloadSerialization> {
        let mut images = harp::RFunction::new("base", "list");
        let mut package_names = Vec::with_capacity(namespaces.len());
        let mut registered_names = Vec::with_capacity(namespaces.len());
        for namespace in namespaces {
            images.add(self.image_environment(&namespace.package)?);
            package_names.push(namespace.package.name.clone());
            registered_names.push(namespace.registered_name.clone());
        }
        let mut replaced = Vec::new();
        for payload in payloads {
            let image = self.image_environment(&payload.package)?;
            for patch in &payload.patches {
                replaced.push(patch_closure(&image, patch).map_err(|error| {
                    WorkerOperationError::with(WorkerErrorCode::BindingForce)(
                        format!(
                            "failed to rewrite payload closure {}::{}: {error}",
                            payload.package.name, patch.binding
                        )
                        .into(),
                    )
                })?);
            }
        }
        if !replaced.is_empty() {
            let mut roots = Vec::new();
            for payload in payloads {
                let image = self.image_environment(&payload.package)?;
                for name in &payload.names {
                    roots.push(
                        harp::RFunction::new("base", "get")
                            .add(name.clone())
                            .param("envir", image.clone())
                            .param("inherits", false)
                            .call()
                            .map_err(|error| {
                                WorkerOperationError::with(WorkerErrorCode::BindingForce)(
                                    InspectionError::from(error),
                                )
                            })?,
                    );
                }
            }
            let replaced = replaced
                .iter()
                .map(|closure| closure.sexp)
                .collect::<HashSet<_>>();
            if ReferenceWalk::default().reaches(&roots, &replaced) {
                return Err(WorkerOperationError::with(WorkerErrorCode::BindingForce)(
                    "a rewritten payload closure is still referenced from another payload location"
                        .to_owned()
                        .into(),
                ));
            }
        }
        let mut sources = harp::RFunction::new("base", "list");
        let mut names = harp::RFunction::new("base", "list");
        for payload in payloads {
            if !namespaces
                .iter()
                .any(|namespace| namespace.package.root == payload.package.root)
            {
                return Err(WorkerOperationError::with(WorkerErrorCode::Protocol)(
                    format!(
                        "payload of {} is not a materialized namespace image",
                        payload.package.name
                    )
                    .into(),
                ));
            }
            sources.add(self.image_environment(&payload.package)?);
            names.add(payload.names.clone());
        }
        let failed = |error: harp::Error| {
            WorkerOperationError::with(WorkerErrorCode::BindingForce)(
                format!("failed to serialize payload bundles: {error}").into(),
            )
        };
        let bundles = harp::RFunction::new("", ".slinker_payloads")
            .add(images.call().map_err(failed)?)
            .add(package_names)
            .add(registered_names)
            .add(sources.call().map_err(failed)?)
            .add(names.call().map_err(failed)?)
            .call()
            .and_then(Vec::<harp::object::RObject>::try_from)
            .map_err(failed)?;
        let mut owners = HashMap::<libr::SEXP, usize>::new();
        for (index, bundle) in bundles.iter().enumerate() {
            let references = bundle
                .elt("references")
                .and_then(Vec::<harp::object::RObject>::try_from)
                .map_err(failed)?;
            for reference in references {
                let owner = *owners.entry(reference.sexp).or_insert(index);
                if owner != index {
                    return Ok(protocol::PayloadSerialization::SharedIdentity {
                        first: self.payload_site(&payloads[owner], owner, reference.sexp)?,
                        second: self.payload_site(&payloads[index], index, reference.sexp)?,
                    });
                }
            }
        }
        let bundles = bundles
            .iter()
            .map(|bundle| {
                Ok(protocol::SerializedPayload {
                    bytes: Vec::<u8>::try_from(&bundle.elt("bytes")?)?,
                    namespaces: Vec::<String>::try_from(&bundle.elt("namespaces")?)?,
                })
            })
            .collect::<harp::Result<_>>()
            .map_err(failed)?;
        Ok(protocol::PayloadSerialization::Serialized { bundles })
    }

    fn payload_site(
        &mut self,
        payload: &protocol::PayloadSpec,
        index: usize,
        reference: libr::SEXP,
    ) -> OperationResult<protocol::PayloadSite> {
        let image = self.image_environment(&payload.package)?;
        let failed = |error: harp::Error| {
            WorkerOperationError::with(WorkerErrorCode::BindingForce)(InspectionError::from(error))
        };
        for name in &payload.names {
            let value = harp::RFunction::new("base", "get")
                .add(name.clone())
                .param("envir", image.clone())
                .param("inherits", false)
                .call()
                .map_err(failed)?;
            let reached = harp::RFunction::new("", ".slinker_serialize")
                .add(value)
                .call()
                .and_then(|serialized| serialized.elt("references"))
                .and_then(Vec::<harp::object::RObject>::try_from)
                .map_err(failed)?
                .iter()
                .any(|candidate| candidate.sexp == reference);
            if reached {
                return Ok(protocol::PayloadSite {
                    payload: index,
                    binding: name.clone(),
                });
            }
        }
        Err(WorkerOperationError::with(WorkerErrorCode::Protocol)(
            format!(
                "no payload binding of {} reaches its shared reference object",
                payload.package.name
            )
            .into(),
        ))
    }
}

fn patch_closure(
    image: &harp::object::RObject,
    patch: &protocol::ClosurePatchSpec,
) -> InspectionResult<harp::object::RObject> {
    let (kinds, names): (Vec<String>, Vec<String>) = patch
        .steps
        .iter()
        .map(|step| match step {
            protocol::ObjectStepSpec::Environment => ("environment".to_owned(), String::new()),
            protocol::ObjectStepSpec::Parent => ("parent".to_owned(), String::new()),
            protocol::ObjectStepSpec::Binding(name) => ("binding".to_owned(), name.clone()),
        })
        .unzip();
    let home = harp::RFunction::new("", ".slinker_closure_home")
        .add(image.clone())
        .add(patch.root.iter().cloned().collect::<Vec<_>>())
        .add(kinds)
        .add(names)
        .call()?;
    let closure = harp::RFunction::new("", ".slinker_closure_at")
        .add(home.clone())
        .add(patch.binding.clone())
        .call()?;
    let deparsed = harp::RFunction::new("", ".slinker_deparse_binding")
        .add(patch.binding.clone())
        .add(closure.clone())
        .call()?;
    let normalized = String::try_from(
        harp::RFunction::new("", ".slinker_normalize_source")
            .add(deparsed)
            .call()?,
    )?;
    if slinker_core::package::Digest::of(&normalized).0 != patch.expected_shape {
        return Err("the installed closure differs from the analyzed one"
            .to_owned()
            .into());
    }
    harp::RFunction::new("", ".slinker_patch_closure")
        .add(home)
        .add(patch.binding.clone())
        .add(closure.clone())
        .add(patch.source.clone())
        .call()?;
    Ok(closure)
}

#[derive(Default)]
struct ReferenceWalk {
    visited: HashSet<libr::SEXP>,
}

impl ReferenceWalk {
    fn reaches(&mut self, roots: &[harp::object::RObject], targets: &HashSet<libr::SEXP>) -> bool {
        let mut stack = roots.iter().map(|root| root.sexp).collect::<Vec<_>>();
        while let Some(value) = stack.pop() {
            if targets.contains(&value) {
                return true;
            }
            if !self.visited.insert(value) {
                continue;
            }
            harp::r::attrib_for_each(value, |_, attribute| stack.push(attribute));
            match harp::utils::r_typeof(value) {
                libr::CLOSXP => stack.extend([
                    harp::r::fn_formals(value),
                    harp::r::fn_body(value),
                    harp::r::fn_env(value),
                ]),
                libr::ENVSXP => {
                    let special = [
                        unsafe { libr::R_GlobalEnv },
                        unsafe { libr::R_BaseEnv },
                        unsafe { libr::R_EmptyEnv },
                        unsafe { libr::R_BaseNamespace },
                    ];
                    if special.contains(&value)
                        || harp::utils::r_env_is_ns_env(value)
                        || harp::utils::r_env_is_pkg_env(value)
                    {
                        continue;
                    }
                    stack.push(harp::r::env_parent(value));
                    for binding in harp::environment::Environment::view(value).iter().flatten() {
                        match binding.value {
                            harp::environment_iter::BindingValue::Standard { object }
                            | harp::environment_iter::BindingValue::Altrep { object, .. } => {
                                stack.push(object.sexp);
                            }
                            harp::environment_iter::BindingValue::Promise { promise } => {
                                if harp::utils::r_promise_is_forced(promise.sexp) {
                                    stack.push(harp::utils::r_promise_value(promise.sexp));
                                }
                            }
                            harp::environment_iter::BindingValue::Active { .. } => {}
                        }
                    }
                }
                libr::VECSXP | libr::EXPRSXP => {
                    for index in 0..harp::object::r_length(value) {
                        stack.push(harp::object::list_get(value, index));
                    }
                }
                libr::LISTSXP | libr::LANGSXP => {
                    let mut node = value;
                    while node != unsafe { libr::R_NilValue } {
                        stack.push(unsafe { libr::CAR(node) });
                        node = unsafe { libr::CDR(node) };
                    }
                }
                _ => {}
            }
        }
        false
    }
}
