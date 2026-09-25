use super::arguments::{matched_call_arg_index, native_call_argument_index};
use super::resolution::{BindingTarget, OpenReason, Resolution};
use super::state::{AnalyzerState, NativeCallTarget, NativeCallbackContext};
use crate::Result;
use crate::analysis::{EdgeKind, Need, NodeKind, RejectCode};
use crate::package::{
    NativeRoutineSummary, NativeSafety, PackageId, PackageImage, PackageIndex, PackageProvider,
};
use crate::syntax::{CallSite, StaticArg};

impl<P: PackageProvider> AnalyzerState<P> {
    pub(super) fn native_selector(call: &CallSite) -> Option<&str> {
        let index = matched_call_arg_index(call, &[".NAME"], ".NAME")?;
        match call.args.get(index)?.as_ref()? {
            StaticArg::Symbol(name) | StaticArg::String(name) => Some(name.as_str()),
        }
    }

    pub(super) fn native_summary_for_selector<'a>(
        native: &'a crate::package::NativeComponent,
        selector: &str,
        summaries: &'a [NativeRoutineSummary],
    ) -> Option<&'a NativeRoutineSummary> {
        summaries.iter().find(|summary| {
            summary.selector == selector
                || native
                    .symbols
                    .iter()
                    .any(|symbol| symbol.binding == selector && symbol.symbol == summary.selector)
        })
    }

    pub(super) fn process_native_routine_callbacks(
        &mut self,
        context: NativeCallbackContext<'_>,
    ) -> Result<()> {
        let NativeCallbackContext {
            owner: callback_owner,
            package: current,
            image,
            binding,
            lexical_environment,
            component,
            call,
        } = context;
        let Some(native) = image
            .index
            .dynlibs
            .iter()
            .find(|native| native.name == component)
        else {
            return Ok(());
        };
        let NativeSafety::Summarized(summaries) = &native.safety else {
            return Ok(());
        };
        let Some(selector) = Self::native_selector(call) else {
            return Ok(());
        };
        let native_node = self.need_node(&Need::Native {
            package: current,
            component: component.to_owned(),
        });
        let Some(summary) = Self::native_summary_for_selector(native, selector, summaries) else {
            self.diagnostic(
                native_node,
                current,
                Some(binding),
                RejectCode::UnknownNativeEffects,
                format!("native routine `{selector}` in `{component}` has no semantic summary"),
                Some(call.span.clone()),
            );
            return Ok(());
        };

        for &position in &summary.callback_arguments {
            if position == 0 {
                self.diagnostic(
                    native_node,
                    current,
                    Some(binding),
                    RejectCode::UnknownNativeEffects,
                    format!("native routine `{selector}` has invalid callback argument position 0"),
                    Some(call.span.clone()),
                );
                continue;
            }
            let callback_index = native_call_argument_index(call, position);
            let callback = callback_index.and_then(|index| call.args.get(index)?.as_ref());
            let Some(StaticArg::Symbol(callback_name)) = callback else {
                self.diagnostic(
                    native_node,
                    current,
                    Some(binding),
                    RejectCode::UnknownNativeEffects,
                    format!(
                        "native routine `{selector}` invokes callback argument #{position}, but the call site does not supply a statically known R callable"
                    ),
                    Some(call.span.clone()),
                );
                continue;
            };
            if callback_index
                .and_then(|index| call.local_closure_args.get(index))
                .copied()
                .unwrap_or(false)
            {
                self.graph.add_edge_at(
                    native_node,
                    callback_owner,
                    EdgeKind::Callback,
                    format!("native routine `{selector}` invokes locally defined callback argument #{position} `{callback_name}`"),
                    Some(call.span.clone()),
                );
                continue;
            }

            match self.resolve_lexical_name(current, image, lexical_environment, callback_name)? {
                Resolution::Static(BindingTarget::Namespace { package, binding: callback }) => self.require_at(
                    native_node,
                    Need::Binding { package, binding: callback.clone() },
                    EdgeKind::Callback,
                    format!("native routine `{selector}` invokes argument #{position} as R binding `{callback}`"),
                    Some(call.span.clone()),
                ),
                Resolution::Static(BindingTarget::Private { package, environment, binding: callback }) => self.require_at(
                    native_node,
                    Need::PrivateBinding { package, environment: environment.clone(), binding: callback.clone() },
                    EdgeKind::Callback,
                    format!("native routine `{selector}` invokes argument #{position} as private R binding `{callback}` in {environment}"),
                    Some(call.span.clone()),
                ),
                Resolution::Static(BindingTarget::Closure { package, closure }) => self.require_at(
                    native_node,
                    Need::ClosureExecution { package, closure },
                    EdgeKind::Callback,
                    format!("native routine `{selector}` invokes argument #{position} as a retained closure"),
                    Some(call.span.clone()),
                ),
                Resolution::Static(BindingTarget::Imported { package, binding: callback }) => {
                    self.require_at(
                        native_node,
                        Need::Activation { package },
                        EdgeKind::Callback,
                        format!("native callback `{callback}` requires imported namespace activation"),
                        Some(call.span.clone()),
                    );
                    self.require_at(
                        native_node,
                        Need::Binding { package, binding: callback.clone() },
                        EdgeKind::Callback,
                        format!("native routine `{selector}` invokes imported callback argument #{position} `{callback}`"),
                        Some(call.span.clone()),
                    );
                }
                Resolution::Static(BindingTarget::External { package, binding: callback }) => {
                    let target = self.graph.add_node(
                        self.packages.name(package).to_owned(),
                        NodeKind::ExternalBinding { name: callback.clone() },
                        Some(call.span.clone()),
                    );
                    self.graph.add_edge_at(
                        native_node,
                        target,
                        EdgeKind::Callback,
                        format!("native routine `{selector}` invokes External callback argument #{position} `{callback}`"),
                        Some(call.span.clone()),
                    );
                }
                Resolution::Static(BindingTarget::Base) => {}
                Resolution::Static(BindingTarget::Local)
                | Resolution::Static(BindingTarget::Native { .. })
                | Resolution::Static(BindingTarget::Metadata { .. })
                | Resolution::OpenDynamic(OpenReason::MissingPackage { .. })
                | Resolution::OpenDynamic(OpenReason::Unresolved(_)) => self.diagnostic(
                    native_node,
                    current,
                    Some(binding),
                    RejectCode::UnknownNativeEffects,
                    format!(
                        "native routine `{selector}` invokes callback argument #{position} `{callback_name}`, but its R callable identity is not statically linkable"
                    ),
                    Some(call.span.clone()),
                ),
            }
        }
        Ok(())
    }

    pub(super) fn native_component_for_binding<'a>(
        index: &'a PackageIndex,
        name: &str,
    ) -> Option<&'a str> {
        // Only explicit routine bindings are evidence. registrationFixes tells
        // us how R names registered routines, but not which routines exist.
        // Treating an arbitrary prefix/suffix match as native would hide real
        // unresolved R names.
        let mut matches = index
            .dynlibs
            .iter()
            .filter(|native| native.symbols.iter().any(|symbol| symbol.binding == name));
        let first = matches.next()?;
        if matches.next().is_some() {
            None
        } else {
            Some(first.name.as_str())
        }
    }

    pub(super) fn native_component_for_call(
        &mut self,
        current: PackageId,
        image: &PackageImage,
        lexical_environment: &str,
        call: &CallSite,
    ) -> Result<Option<NativeCallTarget>> {
        let Some(selector_index) = matched_call_arg_index(call, &[".NAME"], ".NAME") else {
            return Ok(None);
        };
        let Some(selector) = call.args.get(selector_index).and_then(Option::as_ref) else {
            return Ok(None);
        };
        if let StaticArg::String(symbol) = selector {
            let mut components = image.index.dynlibs.iter().filter(|native| {
                native
                    .symbols
                    .iter()
                    .any(|binding| binding.symbol == *symbol)
            });
            let Some(component) = components.next() else {
                return Ok(None);
            };
            if components.next().is_some() {
                return Ok(None);
            }
            return Ok(Some(NativeCallTarget {
                component: component.name.clone(),
                consumes_selector: false,
            }));
        }

        let StaticArg::Symbol(name) = selector else {
            return Ok(None);
        };
        match self.resolve_lexical_name(current, image, lexical_environment, name)? {
            Resolution::Static(BindingTarget::Native { component, .. }) => {
                return Ok(Some(NativeCallTarget {
                    component,
                    consumes_selector: false,
                }));
            }
            Resolution::OpenDynamic(OpenReason::Unresolved(_)) => {}
            Resolution::Static(BindingTarget::Local)
            | Resolution::Static(BindingTarget::Closure { .. })
            | Resolution::Static(BindingTarget::Namespace { .. })
            | Resolution::Static(BindingTarget::Private { .. })
            | Resolution::Static(BindingTarget::Imported { .. })
            | Resolution::Static(BindingTarget::External { .. })
            | Resolution::Static(BindingTarget::Metadata { .. })
            | Resolution::OpenDynamic(OpenReason::MissingPackage { .. })
            | Resolution::Static(BindingTarget::Base) => return Ok(None),
        }

        // With .registration=TRUE, R creates RegisteredNativeSymbol variables
        // for every routine reported by the loaded DLL. nsInfo.rds records the
        // registration policy but not that runtime routine table. A static
        // symbol selector can therefore be associated with the sole registered
        // package DLL even when its exact routine name is unavailable until DLL
        // load. String selectors do not have that lexical binding guarantee.
        let mut registered = image.index.dynlibs.iter().filter(|native| {
            native.registration.is_some() && !matches!(native.safety, NativeSafety::Unsupported(_))
        });
        let Some(first) = registered.next() else {
            return Ok(None);
        };
        if registered.next().is_some() {
            Ok(None)
        } else {
            Ok(Some(NativeCallTarget {
                component: first.name.clone(),
                consumes_selector: true,
            }))
        }
    }

    pub(super) fn sole_opaque_registered_native_component(index: &PackageIndex) -> Option<&str> {
        let mut registered = index.dynlibs.iter().filter(|native| {
            native.registration.is_some() && matches!(native.safety, NativeSafety::Unanalyzed)
        });
        let first = registered.next()?;
        if registered.next().is_some() {
            None
        } else {
            Some(first.name.as_str())
        }
    }
}
