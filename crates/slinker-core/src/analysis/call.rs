use super::resolution::{BindingTarget, OpenReason, Resolution};
use super::state::AnalyzerState;
use crate::Result;
use crate::package::{BindingName, EnvironmentLabel, PackageId, PackageImage, PackageProvider};
use crate::syntax::{CallSite, CalleeKind};
use std::sync::Arc;

#[derive(Clone, Debug)]
pub(super) enum ResolvedCallee {
    Local,
    Conditional(Resolution),
    Established {
        target: Resolution,
        name: BindingName,
    },
}

pub(super) struct ResolvedCall {
    callee: ResolvedCallee,
    call: CallSite,
}

impl ResolvedCall {
    pub(super) fn callee(&self) -> &ResolvedCallee {
        &self.callee
    }
    pub(super) fn syntax(&self) -> &CallSite {
        &self.call
    }
    pub(super) fn target(&self) -> Option<&Resolution> {
        match &self.callee {
            ResolvedCallee::Local => None,
            ResolvedCallee::Conditional(target) | ResolvedCallee::Established { target, .. } => {
                Some(target)
            }
        }
    }

    pub(super) fn base(&self) -> bool {
        matches!(
            self.callee,
            ResolvedCallee::Established {
                target: Resolution::Static(BindingTarget::Base),
                ..
            }
        )
    }

    pub(super) fn name(&self) -> Option<&BindingName> {
        match &self.callee {
            ResolvedCallee::Established { name, .. } => Some(name),
            ResolvedCallee::Local | ResolvedCallee::Conditional(_) => None,
        }
    }
}

impl<P: PackageProvider> AnalyzerState<P> {
    pub(super) fn resolved_call(
        &self,
        current: PackageId,
        image: &PackageImage,
        environment: &EnvironmentLabel,
        call: &CallSite,
    ) -> Result<Arc<ResolvedCall>> {
        if let Some(known) = self.calls.lock().get(&call.id()).cloned() {
            return Ok(known);
        }
        let resolved = if call.callee_kind == CalleeKind::DefinitelyLexical
            || environment.is_unsupported() && call.qualification.is_none()
        {
            ResolvedCallee::Local
        } else {
            let target = match call.qualified_package() {
                Some("base") => Resolution::Static(BindingTarget::Base),
                Some(name) => match self.known_package(name) {
                    Some(package) => {
                        let index = self.packages.index(package)?;
                        let binding = match &call.qualification {
                            Some(crate::syntax::QualifiedCall::Exported(_)) => index
                                .exports
                                .get(call.callee.as_str())
                                .cloned()
                                .unwrap_or_else(|| call.callee.clone().into()),
                            _ => call.callee.clone().into(),
                        };
                        if self.packages.is_external(package) {
                            Resolution::Static(BindingTarget::External { package, binding })
                        } else {
                            Resolution::Static(BindingTarget::Namespace { package, binding })
                        }
                    }
                    None => Resolution::OpenDynamic(OpenReason::MissingPackage {
                        package: name.into(),
                        binding: Some(call.callee.clone().into()),
                    }),
                },
                None => self.resolve_lexical_name(current, image, environment, &call.callee)?,
            };
            if call.callee_kind == CalleeKind::ConditionalFallthrough {
                ResolvedCallee::Conditional(target)
            } else {
                let name = match &target {
                    Resolution::Static(
                        BindingTarget::Namespace { binding, .. }
                        | BindingTarget::Imported { binding, .. }
                        | BindingTarget::Private { binding, .. }
                        | BindingTarget::Native { binding, .. }
                        | BindingTarget::External { binding, .. },
                    ) => binding.clone(),
                    _ => call.callee.clone().into(),
                };
                let target = match target {
                    Resolution::Static(BindingTarget::External { package, .. })
                        if self.packages.name(package) == "base" =>
                    {
                        Resolution::Static(BindingTarget::Base)
                    }
                    target => target,
                };
                ResolvedCallee::Established { target, name }
            }
        };
        Ok(Arc::clone(
            self.calls.lock().entry(call.id()).or_insert_with(|| {
                Arc::new(ResolvedCall {
                    callee: resolved,
                    call: call.clone(),
                })
            }),
        ))
    }
}
