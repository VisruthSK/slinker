use super::arguments::{
    declared_strings, matched_arg_index, matched_static_arg, namespace_formal,
    only_package_argument, reflective_name_formals, static_package_arg, static_string_arg,
};
use super::discovery::Discovered;
use super::dynamic_names::{CreatedName, CreatorOperation, NameCreator};
use super::relocation::{NamespaceCall, PendingRelocation, SyntaxObservation};
use super::resolution::{BindingTarget, OpenReason, ReferenceUse, Resolution};
use super::state::{AnalyzerState, NativeCallbackContext, ParsedSite};
use crate::Result;
use crate::analysis::{EdgeKind, Need, NodeId, RejectCode};
use crate::ir::NamespaceOperation;
use crate::package::BindingName;
use crate::package::EnvironmentLabel;
use crate::package::PackageRole;
use crate::package::{ImportSpec, PackageId, PackageImage, PackageProvider};
use crate::syntax::{CallSite, CalleeKind, ParsedRFile, Span, StaticArg};
use std::borrow::Cow;

impl<P: PackageProvider> AnalyzerState<P> {
    fn namespace_info_query(
        &mut self,
        from: NodeId,
        current: PackageId,
        binding: &str,
        call: &CallSite,
    ) -> Result<()> {
        if !matches!(
            matched_static_arg(call, &["ns", "which"], "which"),
            Some(StaticArg::String(field)) if field == "path"
        ) {
            return self.namespace_argument(from, current, binding, call, &["ns", "which"], "ns");
        }
        match matched_static_arg(call, &["ns", "which"], "ns") {
            Some(StaticArg::String(name)) => {
                if self
                    .known_package(name)
                    .is_some_and(|package| self.packages.role(package) == PackageRole::Linked)
                {
                    self.diagnostic(
                        from,
                        current,
                        Some(binding),
                        RejectCode::UnsupportedRootTransformation,
                        format!("{}() reads the installed path, which the synthetic `{name}` namespace does not have", call.callee),
                        Some(call.span.clone()),
                    );
                }
            }
            _ => self.diagnostic(
                from,
                current,
                Some(binding),
                RejectCode::DynamicLookup,
                format!(
                    "{}() reads namespace metadata of a dynamic namespace",
                    call.callee
                ),
                Some(call.span.clone()),
            ),
        }
        Ok(())
    }

    fn namespace_argument(
        &mut self,
        from: NodeId,
        current: PackageId,
        binding: &str,
        call: &CallSite,
        formals: &[&str],
        target: &str,
    ) -> Result<()> {
        let Some(index) = matched_arg_index(call, formals, target) else {
            return Ok(());
        };
        let Some(StaticArg::String(name)) = call.args.get(index).and_then(Option::as_ref) else {
            self.dynamic_package_name(from, current, binding, call);
            return Ok(());
        };
        let name = name.clone();
        match self.discovered_package(from, current, call, &name)? {
            Discovered::Linked(package) => {
                let Some(source) = call.arg_spans.get(index).cloned().flatten() else {
                    self.unrewritable_package_call(from, current, call, &name);
                    return Ok(());
                };
                if package != current {
                    self.require_at(
                        from,
                        Need::Activation { package },
                        EdgeKind::Discovery,
                        format!("{}() names Linked `{name}`", call.callee),
                        Some(call.span.clone()),
                    );
                }
                self.relocations
                    .push(PendingRelocation::NamespaceArgument { source, package });
            }
            Discovered::Missing => self.missing_package_call(from, current, call, &name),
            Discovered::Settled | Discovered::Optional => {}
        }
        Ok(())
    }

    fn installed_package_query(
        &mut self,
        from: NodeId,
        current: PackageId,
        binding: &str,
        call: &CallSite,
        formals: &[&str],
        target: &str,
    ) -> Result<()> {
        let Some(index) = matched_arg_index(call, formals, target) else {
            return Ok(());
        };
        let Some(StaticArg::String(name)) = call.args.get(index).and_then(Option::as_ref) else {
            self.dynamic_package_name(from, current, binding, call);
            return Ok(());
        };
        let name = name.clone();
        match self.discovered_package(from, current, call, &name)? {
            Discovered::Linked(_) => self.unrewritable_package_call(from, current, call, &name),
            Discovered::Missing => self.missing_package_call(from, current, call, &name),
            Discovered::Settled | Discovered::Optional => {}
        }
        Ok(())
    }

    fn package_description(
        &mut self,
        from: NodeId,
        current: PackageId,
        binding: &str,
        call: &CallSite,
    ) -> Result<()> {
        let formals = ["pkg", "lib.loc", "fields", "drop", "encoding"];
        let Some(index) = matched_arg_index(call, &formals, "pkg") else {
            return Ok(());
        };
        let Some(StaticArg::String(name)) = call.args.get(index).and_then(Option::as_ref) else {
            self.dynamic_package_name(from, current, binding, call);
            return Ok(());
        };
        let name = name.clone();
        match self.discovered_package(from, current, call, &name)? {
            Discovered::Linked(package) => match call.arg_spans.get(index).cloned().flatten() {
                Some(source) if only_package_argument(call, &["fields", "drop", "encoding"]) => {
                    self.relocations
                        .push(PendingRelocation::DescriptionArgument { source, package });
                }
                _ => self.unrewritable_package_call(from, current, call, &name),
            },
            Discovered::Missing => self.missing_package_call(from, current, call, &name),
            Discovered::Settled | Discovered::Optional => {}
        }
        Ok(())
    }

    fn loaded_query(
        &mut self,
        from: NodeId,
        current: PackageId,
        binding: &str,
        call: &CallSite,
        name: Option<&str>,
    ) -> Result<()> {
        let Some(name) = name else {
            self.dynamic_package_name(from, current, binding, call);
            return Ok(());
        };
        let imported = self
            .image(current)?
            .index
            .imports
            .iter()
            .any(|import| match import {
                ImportSpec::All { package, .. } | ImportSpec::From { package, .. } => {
                    package == name
                }
            });
        let package = if name == self.packages.name(current) {
            (!self.is_root(current)).then_some(current)
        } else if imported {
            self.packages
                .resolve(name)?
                .filter(|package| self.packages.role(*package) == PackageRole::Linked)
        } else {
            None
        };
        if let Some(package) = package {
            if package != current {
                self.require_at(
                    from,
                    Need::Activation { package },
                    EdgeKind::Discovery,
                    format!("{}() asks whether imported `{name}` is loaded", call.callee),
                    Some(call.span.clone()),
                );
            }
            self.relocations.push(PendingRelocation::LoadedQuery {
                source: call.span.clone(),
                package,
            });
        }
        Ok(())
    }

    fn loaded_membership(
        &mut self,
        current: PackageId,
        image: &PackageImage,
        lexical_environment: &EnvironmentLabel,
        call: &CallSite,
    ) -> Result<Option<String>> {
        let ([Some(StaticArg::String(name)), _], [None, None], [_, Some(set)]) = (
            call.args.as_slice(),
            call.arg_names.as_slice(),
            call.arg_spans.as_slice(),
        ) else {
            return Ok(None);
        };
        let base = match self.parses.text(set).unwrap_or_default().trim() {
            "base::loadedNamespaces()" => true,
            "loadedNamespaces()" => matches!(
                self.resolve_lexical_name(current, image, lexical_environment, "loadedNamespaces")?,
                Resolution::Static(BindingTarget::Base)
            ),
            _ => false,
        };
        Ok(base.then(|| name.clone()))
    }

    fn external_callee(
        &mut self,
        current: PackageId,
        image: &PackageImage,
        lexical_environment: &EnvironmentLabel,
        call: &CallSite,
    ) -> Result<Option<PackageId>> {
        if call.callee_kind != CalleeKind::DefinitelyExternal {
            return Ok(None);
        }
        Ok(match call.qualified_package.as_deref() {
            Some(package) => self
                .known_package(package)
                .filter(|package| self.packages.is_external(*package)),
            None => match self.resolve_lexical_name(
                current,
                image,
                lexical_environment,
                &call.callee,
            )? {
                Resolution::Static(BindingTarget::External { package, binding })
                    if binding == call.callee.as_str() =>
                {
                    Some(package)
                }
                _ => None,
            },
        })
    }

    fn rlang_call(
        &mut self,
        from: NodeId,
        current: PackageId,
        binding: &str,
        call: &CallSite,
    ) -> Result<()> {
        let (rewritable, formals, target) = match call.callee.as_str() {
            "ns_env" | "ns_imports_env" => {
                return self.namespace_argument(from, current, binding, call, &["x"], "x");
            }
            "ns_exports" => {
                return self.namespace_argument(from, current, binding, call, &["ns"], "ns");
            }
            "is_installed" => (&[][..], &["pkg", "...", "version", "compare"][..], "pkg"),
            "check_installed" => (
                &["reason", "call"][..],
                &[
                    "pkg", "reason", "...", "version", "compare", "action", "call",
                ][..],
                "pkg",
            ),
            _ => return Ok(()),
        };
        let Some(index) = matched_arg_index(call, formals, target) else {
            return Ok(());
        };
        let Some(StaticArg::String(name)) = call.args.get(index).and_then(Option::as_ref) else {
            self.dynamic_package_name(from, current, binding, call);
            return Ok(());
        };
        let name = name.clone();
        let Some(package) = self.declared_linked_package(current, &name)? else {
            return Ok(());
        };
        if !only_package_argument(call, rewritable) {
            self.unrewritable_package_call(from, current, call, &name);
            return Ok(());
        }
        self.relocations.push(PendingRelocation::InstalledQuery {
            source: call.span.clone(),
            package,
            check: call.callee == "check_installed",
        });
        Ok(())
    }

    fn declared_linked_package(
        &mut self,
        current: PackageId,
        name: &str,
    ) -> Result<Option<PackageId>> {
        if name == self.packages.name(current) {
            return Ok((!self.is_root(current)).then_some(current));
        }
        if !self.optional_package_selected(name) && !self.package_is_required(current, name)? {
            return Ok(None);
        }
        Ok(self
            .packages
            .resolve(name)?
            .filter(|package| self.packages.role(*package) == PackageRole::Linked))
    }

    fn utils_call(
        &mut self,
        from: NodeId,
        current: PackageId,
        binding: &str,
        call: &CallSite,
    ) -> Result<()> {
        match call.callee.as_str() {
            "packageVersion" => self.identity_query(from, current, call, true),
            "packageDescription" => self.package_description(from, current, binding, call),
            "getFromNamespace" => self.namespace_argument(
                from,
                current,
                binding,
                call,
                &["x", "ns", "pos", "envir"],
                "ns",
            ),
            "assignInNamespace" => self.namespace_argument(
                from,
                current,
                binding,
                call,
                &["x", "value", "ns", "pos", "envir"],
                "ns",
            ),
            "citation" => {
                self.installed_package_query(from, current, binding, call, &["package"], "package")
            }
            "vignette" | "help" => self.installed_package_query(
                from,
                current,
                binding,
                call,
                &["topic", "package"],
                "package",
            ),
            "data" => self.data_call(from, current, binding, call),
            _ => Ok(()),
        }
    }

    fn reflective_lookup(
        &mut self,
        site: ParsedSite<'_>,
        parsed: &ParsedRFile,
        call: &CallSite,
        formals: &[&str],
        target: &str,
    ) -> Result<()> {
        let ParsedSite {
            node: from,
            package: current,
            image,
            binding,
            lexical_environment,
        } = site;
        if call.callee == "exists"
            && self.argument_text(call, "inherits") == Some("FALSE")
            && self.argument_span(call, "envir").is_some_and(|span| {
                self.reflection.is_non_reflective_namespace_use(span)
                    && self.parses.text(span).is_some_and(|text| {
                        let text = text.trim_start_matches("base::");
                        text.starts_with("asNamespace(") || text.starts_with("getNamespace(")
                    })
            })
        {
            return Ok(());
        }
        let computed_environment = call
            .arg_names
            .iter()
            .flatten()
            .any(|name| matches!(name.as_str(), "envir" | "pos" | "where" | "frame"))
            || call.arg_names.iter().filter(|name| name.is_none()).count() > 1
                && call.callee != "do.call";
        match (
            matched_static_arg(call, formals, target),
            declared_strings(parsed, call, formals, target),
        ) {
            (Some(StaticArg::String(name)), _) if !computed_environment => {
                let name = name.clone();
                self.retain_reflective_name(
                    from,
                    current,
                    image,
                    lexical_environment,
                    &name,
                    &call.span,
                )
            }
            (Some(StaticArg::Symbol(_)), Some(names)) if !computed_environment => {
                for name in names {
                    self.retain_reflective_name(
                        from,
                        current,
                        image,
                        lexical_environment,
                        &name,
                        &call.span,
                    )?;
                }
                Ok(())
            }
            (Some(StaticArg::Symbol(_)) | None, _)
                if matches!(call.callee.as_str(), "match.fun" | "do.call")
                    && !self.builds_function_name(call, formals, target) =>
            {
                Ok(())
            }
            _ => {
                self.diagnostic(
                    from,
                    current,
                    Some(binding),
                    RejectCode::DynamicLookup,
                    format!(
                        "{}() looks up a name that is not a static string in the calling scope",
                        call.callee
                    ),
                    Some(call.span.clone()),
                );
                Ok(())
            }
        }
    }

    fn argument_span<'a>(&self, call: &'a CallSite, name: &str) -> Option<&'a Span> {
        call.arg_names
            .iter()
            .position(|argument| argument.as_deref() == Some(name))
            .and_then(|index| call.arg_spans.get(index)?.as_ref())
    }

    pub(super) fn argument_text(&self, call: &CallSite, name: &str) -> Option<&str> {
        let span = self.argument_span(call, name)?;
        Some(self.parses.text(span)?.trim())
    }

    fn builds_function_name(&self, call: &CallSite, formals: &[&str], target: &str) -> bool {
        matched_arg_index(call, formals, target)
            .and_then(|index| call.arg_spans.get(index)?.as_ref())
            .and_then(|span| {
                let text = self.parses.text(span)?;
                Some(
                    ["paste0(", "paste(", "sprintf(", "as.character("]
                        .iter()
                        .any(|builder| text.starts_with(builder)),
                )
            })
            .unwrap_or(false)
    }

    pub(super) fn retain_reflective_name(
        &mut self,
        from: NodeId,
        current: PackageId,
        image: &PackageImage,
        lexical_environment: &EnvironmentLabel,
        name: &str,
        span: &Span,
    ) -> Result<()> {
        let resolved = self.resolve_lexical_name(current, image, lexical_environment, name)?;
        if matches!(resolved, Resolution::OpenDynamic(OpenReason::Unresolved(_))) {
            return Ok(());
        }
        self.require_resolved(
            from,
            current,
            None,
            resolved,
            span.clone(),
            ReferenceUse::Unrecorded,
        );
        Ok(())
    }

    fn is_slinker_semantic_callee(name: &str) -> bool {
        matches!(
            name,
            "library"
                | "require"
                | "requireNamespace"
                | "loadNamespace"
                | "getNamespace"
                | "asNamespace"
                | "packageVersion"
                | "find.package"
                | "system.file"
                | "getNamespaceImports"
                | "getNamespaceInfo"
                | ".Call"
                | ".External"
                | ".C"
                | ".Fortran"
                | "deparse"
                | "substitute"
                | "match.call"
                | "isNamespaceLoaded"
                | "getNamespaceExports"
                | "getNamespaceName"
                | "getNamespaceVersion"
                | "getExportedValue"
                | "attachNamespace"
                | "unloadNamespace"
                | "path.package"
                | "library.dynam"
                | "setHook"
                | "packageEvent"
                | "makeActiveBinding"
                | "environment"
                | "UseMethod"
                | "NextMethod"
        )
    }

    pub(super) fn semantic_call(
        &mut self,
        site: ParsedSite<'_>,
        parsed: &ParsedRFile,
        call: &CallSite,
    ) -> Result<()> {
        let ParsedSite {
            node: from,
            package: current,
            image,
            binding,
            lexical_environment,
        } = site;
        if !self.semantic_callee_is_base(
            from,
            current,
            image,
            binding,
            lexical_environment,
            call,
        )? {
            if let Some(package) =
                self.external_callee(current, image, lexical_environment, call)?
            {
                match self.packages.name(package).as_str() {
                    "utils" => self.utils_call(from, current, binding, call)?,
                    "rlang" => self.rlang_call(from, current, binding, call)?,
                    _ => {}
                }
            } else if self.is_search_path_data_call(current, image, lexical_environment, call)? {
                self.data_call(from, current, binding, call)?;
            }
            return Ok(());
        }
        if let Some((operation, name)) = created_name(call) {
            self.dynamic_names.observe_creator(NameCreator {
                node: site.node,
                package: current,
                binding: BindingName::from(binding),
                operation,
                name,
            });
        }
        if let Some((formals, target)) = reflective_name_formals(&call.callee) {
            return self.reflective_lookup(site, parsed, call, formals, target);
        }
        match call.callee.as_str() {
            "library" | "require" => {
                self.attachment_call(from, current, call)?;
            }
            "requireNamespace" => {
                self.namespace_operation(
                    from,
                    current,
                    binding,
                    parsed,
                    call,
                    NamespaceCall::Require,
                )?;
            }
            "loadNamespace" => {
                self.namespace_operation(
                    from,
                    current,
                    binding,
                    parsed,
                    call,
                    NamespaceCall::Operation(NamespaceOperation::Load),
                )?;
            }
            "getNamespace" => {
                self.namespace_operation(
                    from,
                    current,
                    binding,
                    parsed,
                    call,
                    NamespaceCall::Operation(NamespaceOperation::Get),
                )?;
            }
            "asNamespace" => {
                self.namespace_operation(
                    from,
                    current,
                    binding,
                    parsed,
                    call,
                    NamespaceCall::Operation(NamespaceOperation::As),
                )?;
            }
            "getNamespaceImports" => {
                self.namespace_argument(from, current, binding, call, &["ns"], "ns")?;
            }
            "getNamespaceInfo" => {
                self.namespace_info_query(from, current, binding, call)?;
            }
            "getExportedValue" => {
                self.namespace_argument(from, current, binding, call, &["ns", "name"], "ns")?;
            }
            "getNamespaceExports" | "getNamespaceName" | "getNamespaceVersion" => {
                self.namespace_argument(from, current, binding, call, &["ns"], "ns")?;
            }
            "isNamespaceLoaded" => {
                let name = match matched_static_arg(call, &["name"], "name") {
                    Some(StaticArg::String(name)) => Some(name.clone()),
                    _ => None,
                };
                self.loaded_query(from, current, binding, call, name.as_deref())?;
            }
            "%in%" => {
                if let Some(name) =
                    self.loaded_membership(current, image, lexical_environment, call)?
                {
                    self.loaded_query(from, current, binding, call, Some(&name))?;
                }
            }
            "attachNamespace" | "unloadNamespace" => {
                self.installed_package_query(from, current, binding, call, &["ns"], "ns")?;
            }
            "path.package" => {
                self.installed_package_query(
                    from,
                    current,
                    binding,
                    call,
                    &["package"],
                    "package",
                )?;
            }
            "library.dynam" => {
                self.installed_package_query(
                    from,
                    current,
                    binding,
                    call,
                    &["chname", "package"],
                    "package",
                )?;
            }
            "find.package" => self.identity_query(from, current, call, false)?,
            "UseMethod" | "NextMethod" => {
                self.s3_dispatch(site, parsed, None, call)?;
            }
            ".Call" | ".External" | ".C" | ".Fortran" => {
                self.native_call(site, parsed, call)?;
            }
            "getNativeSymbolInfo" if !self.is_root(current) => {
                self.linked_native_symbol_query(from, current, image, binding, call);
            }
            "deparse" | "substitute" | "match.call" => {
                self.relocations.observe(SyntaxObservation {
                    node: from,
                    package: current,
                    span: call.span.clone(),
                    callee: BindingName::from(call.callee.as_str()),
                });
            }
            _ => {}
        }
        Ok(())
    }

    fn semantic_callee_is_base(
        &mut self,
        from: NodeId,
        current: PackageId,
        image: &PackageImage,
        binding: &str,
        lexical_environment: &EnvironmentLabel,
        call: &CallSite,
    ) -> Result<bool> {
        if lexical_environment.is_unsupported() && call.qualified_package.is_none() {
            return Ok(false);
        }
        match call.callee_kind {
            CalleeKind::DefinitelyLexical => return Ok(false),
            CalleeKind::ConditionalFallthrough => {
                if call.qualified_package.is_none()
                    && Self::is_slinker_semantic_callee(&call.callee)
                    && matches!(
                        self.resolve_lexical_name(
                            current,
                            image,
                            lexical_environment,
                            &call.callee
                        )?,
                        Resolution::Static(BindingTarget::Base)
                    )
                {
                    self.diagnostic(
                        from,
                        current,
                        Some(binding),
                        RejectCode::SemanticAmbiguity,
                        format!(
                            "conditionally local callee `{}` can fall through to base; linker-specific effects are path-dependent",
                            call.callee
                        ),
                        Some(call.span.clone()),
                    );
                }
                return Ok(false);
            }
            CalleeKind::DefinitelyExternal => {}
        }
        match call.qualified_package.as_deref() {
            Some("base") => {}
            Some(_) => return Ok(false),
            None => {
                let resolved =
                    self.resolve_lexical_name(current, image, lexical_environment, &call.callee)?;
                if !matches!(resolved, Resolution::Static(BindingTarget::Base)) {
                    return Ok(false);
                }
            }
        }
        Ok(true)
    }

    fn attachment_call(&mut self, from: NodeId, current: PackageId, call: &CallSite) -> Result<()> {
        let package = static_package_arg(call);
        if let Some(name) = package
            && self.package_is_suggested_only(current, name)?
            && !self.optional_package_selected(name)
        {
            return Ok(());
        }
        self.diagnostic(
            from,
            current,
            None,
            RejectCode::PackageAttachmentUnsupported,
            match package {
                Some(name) => {
                    format!("search-path attachment of `{name}` is outside the current contract")
                }
                None => "dynamic package attachment is outside the current contract".into(),
            },
            Some(call.span.clone()),
        );
        Ok(())
    }

    fn native_call(
        &mut self,
        site: ParsedSite<'_>,
        parsed: &ParsedRFile,
        call: &CallSite,
    ) -> Result<()> {
        let ParsedSite {
            node: from,
            package: current,
            image,
            binding,
            lexical_environment,
        } = site;
        if let Some(target) =
            self.native_component_for_call(current, image, lexical_environment, call)?
        {
            let component = target.component;
            self.require_at(
                from,
                Need::Native {
                    package: current,
                    component: component.clone(),
                },
                EdgeKind::Native,
                format!(
                    "reachable {} resolves its static native selector through `{component}`",
                    call.callee
                ),
                Some(call.span.clone()),
            );
            self.process_native_routine_callbacks(NativeCallbackContext {
                owner: from,
                package: current,
                image,
                binding,
                lexical_environment,
                component: &component,
                parsed,
                call,
            })?;
            if !self.is_root(current) {
                self.linked_native_selector(from, current, image, call, &component);
            }
        } else {
            self.diagnostic(
                from,
                current,
                None,
                RejectCode::UnknownNativeLookup,
                format!(
                    "{} native selector cannot be resolved to one registered package DLL",
                    call.callee
                ),
                Some(call.span.clone()),
            );
        }
        Ok(())
    }

    fn namespace_operation(
        &mut self,
        from: NodeId,
        current: PackageId,
        binding: &str,
        parsed: &ParsedRFile,
        call: &CallSite,
        operation: NamespaceCall,
    ) -> Result<()> {
        let literal = static_string_arg(call);
        let name = literal.map(Cow::Borrowed).or_else(|| {
            self.reflection
                .contextual_namespace(&call.span)
                .map(|name| Cow::Owned(name.to_owned()))
        });
        let Some(name) = name else {
            if self.reflection.is_non_reflective_namespace_use(&call.span) {
                return Ok(());
            }
            if let Some(names) = namespace_formal(&call.callee)
                .and_then(|formal| declared_strings(parsed, call, &[formal], formal))
            {
                for name in names {
                    self.declared_namespace_name(from, current, binding, call, operation, &name)?;
                }
                return Ok(());
            }
            self.diagnostic(
                from,
                current,
                Some(binding),
                RejectCode::DynamicPackageDiscovery,
                format!("{}() with a dynamic namespace name", call.callee),
                Some(call.span.clone()),
            );
            return Ok(());
        };
        let target = match self.discovered_package(from, current, call, &name)? {
            Discovered::Linked(target) => target,
            Discovered::Settled => return Ok(()),
            Discovered::Optional => {
                if operation == NamespaceCall::Require {
                    self.optional_availability_blocker(from, current, binding, &name, &call.span);
                }
                return Ok(());
            }
            Discovered::Missing => {
                if operation == NamespaceCall::Require {
                    self.relocations.push(PendingRelocation::RequireNamespace {
                        source: call.span.clone(),
                        loaded: None,
                    });
                } else {
                    self.record_missing_package(
                        from,
                        current,
                        &name,
                        EdgeKind::Discovery,
                        format!("{} requires unavailable namespace {name}", call.callee),
                        Some(call.span.clone()),
                    );
                }
                return Ok(());
            }
        };
        if literal.is_none() {
            self.diagnostic(
                from,
                current,
                Some(binding),
                RejectCode::DynamicPackageDiscovery,
                format!(
                    "{}() names Linked `{name}` through a computed value, which cannot be rewritten to its private namespace",
                    call.callee
                ),
                Some(call.span.clone()),
            );
            return Ok(());
        }
        let rewritable = match operation {
            NamespaceCall::Require => &["quietly"][..],
            NamespaceCall::Operation(NamespaceOperation::As) => &["base.OK"][..],
            NamespaceCall::Operation(NamespaceOperation::Get | NamespaceOperation::Load) => &[],
        };
        if !only_package_argument(call, rewritable) {
            self.diagnostic(
                from,
                current,
                None,
                RejectCode::UnsupportedRootTransformation,
                format!(
                    "{}() on Linked `{name}` passes arguments that its private namespace cannot honor",
                    call.callee
                ),
                Some(call.span.clone()),
            );
            return Ok(());
        }
        if target != current {
            self.require_at(
                from,
                Need::Activation { package: target },
                EdgeKind::Discovery,
                format!("{} names Linked `{name}`", call.callee),
                Some(call.span.clone()),
            );
        }
        self.relocations.push(PendingRelocation::namespace(
            call.span.clone(),
            target,
            operation,
        ));
        Ok(())
    }

    fn declared_namespace_name(
        &mut self,
        from: NodeId,
        current: PackageId,
        binding: &str,
        call: &CallSite,
        operation: NamespaceCall,
        name: &str,
    ) -> Result<()> {
        let problem = match self.discovered_package(from, current, call, name)? {
            Discovered::Settled => return Ok(()),
            Discovered::Optional if operation != NamespaceCall::Require => return Ok(()),
            Discovered::Missing if operation != NamespaceCall::Require => {
                self.record_missing_package(
                    from,
                    current,
                    name,
                    EdgeKind::Discovery,
                    format!("{} requires unavailable namespace {name}", call.callee),
                    Some(call.span.clone()),
                );
                return Ok(());
            }
            Discovered::Linked(_) => format!(
                "{}() can name Linked `{name}` through a declared computed value, which cannot be rewritten to its private namespace",
                call.callee
            ),
            Discovered::Optional | Discovered::Missing => format!(
                "{}() can name `{name}` through a declared computed value, whose installation slinker does not fix",
                call.callee
            ),
        };
        self.diagnostic(
            from,
            current,
            Some(binding),
            RejectCode::DynamicPackageDiscovery,
            problem,
            Some(call.span.clone()),
        );
        Ok(())
    }

    pub(super) fn call_resolves_definitely_to_base(
        &mut self,
        current: PackageId,
        image: &PackageImage,
        lexical_environment: &EnvironmentLabel,
        call: &CallSite,
    ) -> Result<bool> {
        if call.callee_kind != CalleeKind::DefinitelyExternal {
            return Ok(false);
        }
        match call.qualified_package.as_deref() {
            Some("base") => Ok(true),
            Some(_) => Ok(false),
            None => Ok(matches!(
                self.resolve_lexical_name(current, image, lexical_environment, &call.callee)?,
                Resolution::Static(BindingTarget::Base)
            )),
        }
    }

    fn identity_query(
        &mut self,
        from: NodeId,
        current: PackageId,
        call: &CallSite,
        version: bool,
    ) -> Result<()> {
        let Some(name) = static_string_arg(call) else {
            self.diagnostic(
                from,
                current,
                None,
                RejectCode::DynamicPackageDiscovery,
                "dynamic package identity query",
                Some(call.span.clone()),
            );
            return Ok(());
        };
        let name = name.to_owned();
        let target = match self.discovered_package(from, current, call, &name)? {
            Discovered::Linked(target) => target,
            Discovered::Settled | Discovered::Optional => return Ok(()),
            Discovered::Missing => {
                self.record_missing_package(
                    from,
                    current,
                    &name,
                    EdgeKind::Discovery,
                    format!("{} requires unavailable package {name}", call.callee),
                    Some(call.span.clone()),
                );
                return Ok(());
            }
        };
        if !version {
            self.diagnostic(
                from,
                current,
                None,
                RejectCode::UnsupportedRootTransformation,
                format!("find.package(\"{name}\") has no installed path once `{name}` is Linked"),
                Some(call.span.clone()),
            );
        } else if !only_package_argument(call, &[]) {
            self.diagnostic(
                from,
                current,
                None,
                RejectCode::UnsupportedRootTransformation,
                format!("packageVersion() on Linked `{name}` passes a library location"),
                Some(call.span.clone()),
            );
        } else {
            self.relocations.push(PendingRelocation::PackageVersion {
                source: call.span.clone(),
                version: self.packages.identity(target).version.clone(),
            });
        }
        Ok(())
    }
}

pub(super) fn created_name(call: &CallSite) -> Option<(CreatorOperation, CreatedName)> {
    let (operation, formals, target): (_, &[&str], _) = match call.callee.as_str() {
        "assign" => (
            CreatorOperation::Assign,
            &["x", "value", "pos", "envir", "inherits", "immediate"],
            "x",
        ),
        "delayedAssign" => (
            CreatorOperation::DelayedAssign,
            &["x", "value", "eval.env", "assign.env"],
            "x",
        ),
        "makeActiveBinding" => (
            CreatorOperation::MakeActiveBinding,
            &["sym", "fun", "env"],
            "sym",
        ),
        "list2env" => return Some((CreatorOperation::List2env, CreatedName::Any)),
        _ => return None,
    };
    let name = match matched_static_arg(call, formals, target) {
        Some(StaticArg::String(name)) => CreatedName::Named(name.into()),
        Some(StaticArg::Symbol(_)) | None => CreatedName::Any,
    };
    Some((operation, name))
}
