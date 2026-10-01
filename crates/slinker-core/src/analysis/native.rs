use super::NodeId;
use super::arguments::{
    declared_callables, matched_arg_index, matched_static_arg, native_call_argument_index,
};
use super::relocation::PendingRelocation;
use super::resolution::{BindingTarget, OpenReason, Resolution};
use super::state::{AnalyzerState, NativeCallTarget, NativeCallbackContext};
use crate::Result;
use crate::analysis::{EdgeKind, Need, RejectCode};
use crate::ir::ExternalBindingAccess;
use crate::package::NativeComponent;
use crate::package::{
    NameLookup, NativeInterface, NativeLibrary, NativeRoutineSummary, NativeSafety, PackageId,
    PackageImage, PackageIndex, PackageProvider,
};
use crate::syntax::{CallSite, DeclaredCallable, Span, StaticArg};

struct CallbackSite<'a> {
    node: NodeId,
    package: PackageId,
    binding: &'a str,
    selector: &'a str,
    position: usize,
    span: &'a Span,
}

impl<P: PackageProvider> AnalyzerState<P> {
    pub(super) fn native_selector(call: &CallSite) -> Option<&str> {
        let index = matched_arg_index(call, &[".NAME"], ".NAME")?;
        match call.args.get(index)?.as_ref()? {
            StaticArg::Symbol(name) | StaticArg::String(name) => Some(name.as_str()),
        }
    }

    pub(super) fn native_summary_for_selector<'a>(
        native: &'a NativeComponent,
        selector: &str,
        summaries: &'a [NativeRoutineSummary],
    ) -> Option<&'a NativeRoutineSummary> {
        summaries.iter().find(|summary| {
            summary.selector == selector
                || native
                    .bindings()
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
            parsed,
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
            component: component.to_owned().into(),
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
                self.depend(
                    native_node,
                    callback_owner,
                    EdgeKind::Callback,
                    format!("native routine `{selector}` invokes locally defined callback argument #{position} `{callback_name}`"),
                    Some(call.span.clone()),
                );
                continue;
            }

            let resolution =
                self.resolve_lexical_name(current, image, lexical_environment, callback_name)?;
            let declared = callback_index.and_then(|index| declared_callables(parsed, call, index));
            let site = CallbackSite {
                node: native_node,
                package: current,
                binding,
                selector,
                position,
                span: &call.span,
            };
            match declared {
                Some(callables) => {
                    for callable in callables {
                        let resolution = self.declared_callable_resolution(
                            current,
                            image,
                            lexical_environment,
                            &callable,
                        )?;
                        self.require_native_callback(&site, &callable.name, resolution);
                    }
                }
                None => self.require_native_callback(&site, callback_name, resolution),
            }
        }
        Ok(())
    }

    pub(super) fn declared_callable_resolution(
        &mut self,
        current: PackageId,
        image: &PackageImage,
        lexical_environment: &str,
        callable: &DeclaredCallable,
    ) -> Result<Resolution> {
        let Some(name) = &callable.package else {
            return self.resolve_lexical_name(current, image, lexical_environment, &callable.name);
        };
        let binding = callable.name.clone().into();
        Ok(match self.packages.resolve(name)? {
            None => Resolution::OpenDynamic(OpenReason::MissingPackage {
                package: name.clone(),
                binding: Some(callable.name.clone()),
            }),
            Some(package) if self.packages.is_external(package) => {
                self.external.insert(package);
                Resolution::Static(BindingTarget::External { package, binding })
            }
            Some(package) => Resolution::Static(BindingTarget::Imported { package, binding }),
        })
    }

    fn require_native_callback(
        &mut self,
        site: &CallbackSite<'_>,
        callback_name: &str,
        resolution: Resolution,
    ) {
        let &CallbackSite {
            node: native_node,
            package: current,
            binding,
            selector,
            position,
            span: call_span,
        } = site;
        match resolution {
                Resolution::Static(BindingTarget::Namespace { package, binding: callback }) => self.require_at(
                    native_node,
                    Need::Binding { package, binding: callback.clone() },
                    EdgeKind::Callback,
                    format!("native routine `{selector}` invokes argument #{position} as R binding `{callback}`"),
                    Some(call_span.clone()),
                ),
                Resolution::Static(BindingTarget::Private { package, environment, binding: callback }) => self.require_at(
                    native_node,
                    Need::PrivateBinding { package, environment: environment.clone(), binding: callback.clone() },
                    EdgeKind::Callback,
                    format!("native routine `{selector}` invokes argument #{position} as private R binding `{callback}` in {environment}"),
                    Some(call_span.clone()),
                ),
                Resolution::Static(BindingTarget::Closure { package, closure }) => self.require_at(
                    native_node,
                    Need::ClosureExecution { package, closure },
                    EdgeKind::Callback,
                    format!("native routine `{selector}` invokes argument #{position} as a retained closure"),
                    Some(call_span.clone()),
                ),
                Resolution::Static(BindingTarget::Imported { package, binding: callback }) => {
                    self.require_at(
                        native_node,
                        Need::Activation { package },
                        EdgeKind::Callback,
                        format!("native callback `{callback}` requires imported namespace activation"),
                        Some(call_span.clone()),
                    );
                    self.require_at(
                        native_node,
                        Need::Binding { package, binding: callback.clone() },
                        EdgeKind::Callback,
                        format!("native routine `{selector}` invokes imported callback argument #{position} `{callback}`"),
                        Some(call_span.clone()),
                    );
                }
                Resolution::Static(BindingTarget::External { package, binding: callback }) => {
                    let target = self.external_binding(
                        package,
                        &callback,
                        ExternalBindingAccess::Exported,
                        Some(call_span.clone()),
                    );
                    self.depend(
                        native_node,
                        target,
                        EdgeKind::Callback,
                        format!("native routine `{selector}` invokes External callback argument #{position} `{callback}`"),
                        Some(call_span.clone()),
                    );
                }
                Resolution::Static(BindingTarget::Base) => {}
                Resolution::Static(BindingTarget::Local | BindingTarget::Native { .. } |
BindingTarget::Metadata { .. }) |
Resolution::OpenDynamic(OpenReason::MissingPackage { .. } |
OpenReason::Unresolved(_)) => self.diagnostic(
                    native_node,
                    current,
                    Some(binding),
                    RejectCode::UnknownNativeEffects,
                    format!(
                        "native routine `{selector}` invokes callback argument #{position} `{callback_name}`, but its R callable identity is not statically linkable"
                    ),
                    Some(call_span.clone()),
                ),
            }
    }

    pub(super) fn linked_native_selector(
        &mut self,
        from: NodeId,
        current: PackageId,
        image: &PackageImage,
        call: &CallSite,
        component: &str,
    ) {
        let Some(index) = matched_arg_index(call, &[".NAME"], ".NAME") else {
            return;
        };
        let Some(StaticArg::String(symbol)) = call.args.get(index).and_then(Option::as_ref) else {
            return;
        };
        let Some(native) = image
            .index
            .dynlibs
            .iter()
            .find(|native| native.name == component)
        else {
            return;
        };
        let names_other_library = call
            .arg_names
            .iter()
            .zip(&call.args)
            .any(|(name, argument)| {
                name.as_deref() == Some("PACKAGE")
                    && !matches!(argument, Some(StaticArg::String(library)) if library == component)
            });
        let registered = NativeInterface::of_callee(&call.callee)
            .zip(native.library.routines())
            .is_some_and(|(interface, routines)| routines.of(interface).contains(symbol));
        let source = call.arg_spans.get(index).cloned().flatten();
        match (native.library.name_lookup(), source) {
            (Some(NameLookup::Forced), _) if !names_other_library => {}
            (Some(NameLookup::Allowed), Some(source)) if registered && !names_other_library => {
                self.relocations.push(PendingRelocation::NativeSymbol {
                    source,
                    package: current,
                    component: component.into(),
                    symbol: symbol.clone(),
                });
            }
            _ => self.diagnostic(
                from,
                current,
                None,
                RejectCode::UnknownNativeLookup,
                format!(
                    "{} selector \"{symbol}\" of Linked `{component}` cannot be resolved through its own DLL copy",
                    call.callee
                ),
                Some(call.span.clone()),
            ),
        }
    }

    pub(super) fn linked_native_symbol_query(
        &mut self,
        from: NodeId,
        current: PackageId,
        image: &PackageImage,
        binding: &str,
        call: &CallSite,
    ) {
        let formals = ["name", "PACKAGE", "unlist", "withRegistrationInfo"];
        let library = matched_arg_index(call, &formals, "PACKAGE");
        match library.map(|index| (index, call.args.get(index).and_then(Option::as_ref))) {
            None => {
                let Some(StaticArg::String(symbol)) = matched_static_arg(call, &formals, "name")
                else {
                    return;
                };
                if image.index.dynlibs.iter().any(|native| {
                    matches!(
                        &native.library,
                        NativeLibrary::Loaded {
                            name_lookup: NameLookup::Allowed,
                            routines,
                            ..
                        } if routines.names().contains(symbol.as_str())
                    )
                }) {
                    self.diagnostic(
                        from,
                        current,
                        None,
                        RejectCode::UnknownNativeLookup,
                        format!(
                            "getNativeSymbolInfo(\"{symbol}\") searches every loaded DLL, including another copy of this Linked package's"
                        ),
                        Some(call.span.clone()),
                    );
                }
            }
            Some((index, Some(StaticArg::String(library)))) => {
                let Some(native) = image
                    .index
                    .dynlibs
                    .iter()
                    .find(|native| native.name == *library)
                else {
                    return;
                };
                match (
                    native.library.name_lookup(),
                    call.arg_spans.get(index).cloned().flatten(),
                ) {
                    (Some(NameLookup::Forced), _) => {}
                    (Some(NameLookup::Allowed), Some(source)) => {

                        self.relocations.push(PendingRelocation::NativeLibrary {
                            source,
                            package: current,
                            component: library.as_str().into(),
                        });
                    }
                    _ => self.diagnostic(
                        from,
                        current,
                        None,
                        RejectCode::UnknownNativeLookup,
                        format!(
                            "getNativeSymbolInfo() names Linked DLL `{library}`, which cannot be resolved through its own copy"
                        ),
                        Some(call.span.clone()),
                    ),
                }
            }
            Some(_) => self.diagnostic(
                from,
                current,
                Some(binding),
                RejectCode::DynamicLookup,
                "getNativeSymbolInfo() with a computed PACKAGE can name a Linked DLL",
                Some(call.span.clone()),
            ),
        }
    }

    pub(super) fn native_component_for_binding<'a>(
        index: &'a PackageIndex,
        name: &str,
    ) -> Option<&'a str> {
        let mut matches = index
            .dynlibs
            .iter()
            .filter(|native| native.bindings().any(|symbol| symbol.binding == name));
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
        let Some(selector_index) = matched_arg_index(call, &[".NAME"], ".NAME") else {
            return Ok(None);
        };
        let Some(selector) = call.args.get(selector_index).and_then(Option::as_ref) else {
            return Ok(None);
        };
        if let StaticArg::String(symbol) = selector {
            let mut components = image
                .index
                .dynlibs
                .iter()
                .filter(|native| native.bindings().any(|binding| binding.symbol == *symbol));
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
            Resolution::Static(
                BindingTarget::Local
                | BindingTarget::Closure { .. }
                | BindingTarget::Namespace { .. }
                | BindingTarget::Private { .. }
                | BindingTarget::Imported { .. }
                | BindingTarget::External { .. }
                | BindingTarget::Metadata { .. }
                | BindingTarget::Base,
            )
            | Resolution::OpenDynamic(OpenReason::MissingPackage { .. }) => return Ok(None),
        }

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
