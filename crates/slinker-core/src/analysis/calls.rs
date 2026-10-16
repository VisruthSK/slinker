use super::arguments::{
    declared_callables, declared_strings, matched_arg_index, matched_static_arg, namespace_formal,
    only_package_argument, reflective_name_formals, static_package_arg, static_string_arg,
};
use super::discovery::Discovered;
use super::dynamic_names::{CreatedName, CreatorOperation, NameCreator};
use super::reflection::ProcessObservation;
use super::relocation::{NamespaceCall, PendingRelocation, SyntaxObservation};
use super::resolution::{BindingTarget, OpenReason, ReferenceUse, Resolution};
use super::state::{AnalyzerState, Caller, NativeCallbackContext, ParsedSite};
use crate::Result;
use crate::analysis::{EdgeKind, Need, NodeId, RejectCode};
use crate::ir::NamespaceOperation;
use crate::package::BindingName;
use crate::package::EnvironmentLabel;
use crate::package::PackageRole;
use crate::package::{PackageId, PackageImage, PackageProvider};
use crate::syntax::{CallSite, ParsedRFile, Span, StaticArg};

impl<P: PackageProvider> AnalyzerState<P> {
    fn namespace_info_query(
        &self,
        caller @ Caller {
            node: from,
            package: current,
            binding,
        }: Caller<'_>,
        call: &CallSite,
    ) -> Result<()> {
        if !matches!(
            matched_static_arg(call, &["ns", "which"], "which"),
            Some(StaticArg::String(field)) if field == "path"
        ) {
            return self.namespace_argument(caller, call, &["ns", "which"], "ns");
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
        &self,
        caller @ Caller {
            node: from,
            package: current,
            ..
        }: Caller<'_>,
        call: &CallSite,
        formals: &[&str],
        target: &str,
    ) -> Result<()> {
        let Some(index) = matched_arg_index(call, formals, target) else {
            return Ok(());
        };
        let Some(StaticArg::String(name)) = call.static_arg(index) else {
            self.dynamic_package_name(caller, call);
            return Ok(());
        };
        let name = name.clone();
        if name.as_str() == self.packages.name(self.root).as_str() {
            let selector = match call.callee.as_str() {
                "getExportedValue" => Some("name"),
                "getFromNamespace" | "assignInNamespace" => Some("x"),
                _ => None,
            };
            if let Some(selector) = selector
                && !matches!(matched_static_arg(call, formals, selector), Some(StaticArg::String(binding)) if !super::namespace_use::changed_root_binding(binding))
            {
                self.diagnostic(from, current, Some(caller.binding), RejectCode::UnsupportedRootTransformation,
                    "Root namespace binding observation can reach generated support or the replaced load hook", Some(call.span.clone()));
                return Ok(());
            }
        }
        match self.discovered_package(from, current, call, &name)? {
            Discovered::Linked(package) => {
                let selector = match call.callee.as_str() {
                    "getExportedValue" => Some("name"),
                    "getFromNamespace" | "assignInNamespace" => Some("x"),
                    _ => None,
                };
                if let Some(selector) = selector {
                    let Some(StaticArg::String(symbol)) =
                        matched_static_arg(call, formals, selector)
                    else {
                        self.diagnostic(
                            from,
                            current,
                            None,
                            RejectCode::DynamicLookup,
                            format!("{}() has an unproven namespace binding name", call.callee),
                            Some(call.span.clone()),
                        );
                        return Ok(());
                    };
                    let image = self.image(package)?;
                    let binding = if call.callee == "getExportedValue" {
                        image.index.exports.get(symbol.as_str()).cloned()
                    } else {
                        image
                            .index
                            .binding_names
                            .contains(symbol.as_str())
                            .then(|| BindingName::from(symbol.as_str()))
                    };
                    let Some(binding) = binding else {
                        self.diagnostic(from, current, None, RejectCode::DynamicLookup,
                            format!("{}() binding `{symbol}` is not established in the installed namespace", call.callee), Some(call.span.clone()));
                        return Ok(());
                    };
                    self.require_at(
                        from,
                        Need::Binding { package, binding },
                        EdgeKind::Discovery,
                        "a namespace query or write reaches its literal binding",
                        Some(call.span.clone()),
                    );
                }
                let Some(source) = call.arg_span(index).cloned() else {
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
                    .lock()
                    .push(PendingRelocation::NamespaceArgument { source, package });
            }
            Discovered::Missing => self.missing_package_call(from, current, call, &name),
            Discovered::Settled | Discovered::Optional => {}
        }
        Ok(())
    }

    fn installed_package_query(
        &self,
        caller @ Caller {
            node: from,
            package: current,
            ..
        }: Caller<'_>,
        call: &CallSite,
        formals: &[&str],
        target: &str,
    ) -> Result<()> {
        let Some(index) = matched_arg_index(call, formals, target) else {
            return Ok(());
        };
        let Some(StaticArg::String(name)) = call.static_arg(index) else {
            self.dynamic_package_name(caller, call);
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
        &self,
        caller @ Caller {
            node: from,
            package: current,
            ..
        }: Caller<'_>,
        call: &CallSite,
    ) -> Result<()> {
        let formals = ["pkg", "lib.loc", "fields", "drop", "encoding"];
        let Some(index) = matched_arg_index(call, &formals, "pkg") else {
            return Ok(());
        };
        let Some(StaticArg::String(name)) = call.static_arg(index) else {
            self.dynamic_package_name(caller, call);
            return Ok(());
        };
        let name = name.clone();
        match self.discovered_package(from, current, call, &name)? {
            Discovered::Linked(package) => match call.arg_span(index).cloned() {
                Some(source)
                    if only_package_argument(
                        call,
                        &formals,
                        "pkg",
                        &["fields", "drop", "encoding"],
                    ) =>
                {
                    self.relocations
                        .lock()
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
        &self,
        caller @ Caller {
            node: from,
            package: current,
            ..
        }: Caller<'_>,
        call: &CallSite,
        name: Option<&str>,
    ) {
        let Some(name) = name else {
            self.dynamic_package_name(caller, call);
            return;
        };
        self.reflection
            .lock()
            .defer_loaded_query(super::reflection::LoadedQuery {
                node: from,
                owner: current,
                name: name.into(),
                call: call.clone(),
            });
    }

    pub(super) fn settle_loaded_queries(&self) -> Result<()> {
        for observation in self.reflection.lock().take_raw_registry_observations() {
            self.diagnostic(observation.node, observation.package, Some(&observation.binding), RejectCode::UnsupportedRootTransformation,
                "raw namespace registry observation cannot preserve private activation and registration", Some(observation.span));
        }
        let queries = self.reflection.lock().take_loaded_queries();
        for query in queries {
            let Some(package) = self.known_package(&query.name) else {
                continue;
            };
            if self.packages.role(package) != PackageRole::Linked {
                continue;
            }
            let image = self.image(query.owner)?;
            let valid_arguments = query.call.callee != "isNamespaceLoaded"
                || only_package_argument(&query.call, &["name"], "name", &[]);
            if !valid_arguments || !self.loaded_state_established(query.owner, &image, &query.name)
            {
                self.diagnostic(query.node, query.owner, None, RejectCode::DynamicLookup,
                    format!("loaded-state observation of Linked `{}` has no established original load boundary or argument contract", query.name), Some(query.call.span));
                continue;
            }
            self.relocations
                .lock()
                .push(PendingRelocation::LoadedQuery {
                    source: query.call.span,
                    package,
                });
        }
        Ok(())
    }

    fn loaded_membership(
        &self,
        current: PackageId,
        image: &PackageImage,
        lexical_environment: &EnvironmentLabel,
        parsed: &ParsedRFile,
        call: &CallSite,
    ) -> Result<Option<String>> {
        let [first, second] = &*call.arguments else {
            return Ok(None);
        };
        let (Some(StaticArg::String(name)), None, None, Some(set)) =
            (&first.value, &first.name, &second.name, &second.span)
        else {
            return Ok(None);
        };
        let Some(nested) = parsed
            .expressions
            .iter()
            .flat_map(|expression| &expression.calls)
            .find(|nested| &nested.span == set)
        else {
            return Ok(None);
        };
        let fact = self.resolved_call(current, image, lexical_environment, nested)?;
        if fact.base()
            && fact.name().is_some_and(|name| name == "loadedNamespaces")
            && nested.arg_count() == 0
        {
            self.reflection.lock().check_registry_use(set.clone());
            Ok(Some(name.as_str().to_owned()))
        } else {
            Ok(None)
        }
    }

    fn rlang_call(
        &self,
        caller @ Caller {
            node: from,
            package: current,
            ..
        }: Caller<'_>,
        call: &CallSite,
    ) -> Result<()> {
        let (rewritable, formals, target) = match call.callee.as_str() {
            "ns_env" | "ns_imports_env" => {
                return self.namespace_argument(caller, call, &["x"], "x");
            }
            "ns_exports" => {
                return self.namespace_argument(caller, call, &["ns"], "ns");
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
        let Some(StaticArg::String(name)) = call.static_arg(index) else {
            self.dynamic_package_name(caller, call);
            return Ok(());
        };
        let name = name.clone();
        let Some(package) = self.declared_linked_package(current, &name)? else {
            return Ok(());
        };
        if !only_package_argument(call, formals, target, rewritable) {
            self.unrewritable_package_call(from, current, call, &name);
            return Ok(());
        }
        self.relocations
            .lock()
            .push(PendingRelocation::InstalledQuery {
                source: call.span.clone(),
                package,
                check: call.callee == "check_installed",
            });
        Ok(())
    }

    fn declared_linked_package(&self, current: PackageId, name: &str) -> Result<Option<PackageId>> {
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
        &self,
        caller @ Caller {
            node: from,
            package: current,
            ..
        }: Caller<'_>,
        call: &CallSite,
    ) -> Result<()> {
        match call.callee.as_str() {
            "packageVersion" => self.identity_query(from, current, call, true),
            "packageDescription" => self.package_description(caller, call),
            "getFromNamespace" => {
                self.namespace_argument(caller, call, &["x", "ns", "pos", "envir"], "ns")
            }
            "assignInNamespace" => {
                self.reflection.lock().observe_namespace_mutation(
                    super::reflection::NamespaceMutation {
                        node: from,
                        package: current,
                        binding: caller.binding.into(),
                        call: call.clone(),
                    },
                );
                self.namespace_argument(caller, call, &["x", "value", "ns", "pos", "envir"], "ns")
            }
            "citation" => self.installed_package_query(caller, call, &["package"], "package"),
            "vignette" | "help" => {
                self.installed_package_query(caller, call, &["topic", "package"], "package")
            }
            "data" => self.data_call(caller, call),
            _ => Ok(()),
        }
    }

    fn reflective_lookup(
        &self,
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
        for usage in parsed
            .expressions
            .iter()
            .flat_map(|expression| &expression.namespace_uses)
        {
            if let crate::syntax::NamespaceUseKind::Argument { call: consumer, .. } =
                &usage.operation
                && consumer == &call.id()
                && self
                    .calls
                    .lock()
                    .get(&usage.producer)
                    .is_some_and(|producer| producer.base())
            {
                return Ok(());
            }
        }
        let computed_environment = call
            .arguments
            .iter()
            .filter_map(|argument| argument.name.as_deref())
            .any(|name| matches!(name, "envir" | "pos" | "where" | "frame"))
            || call
                .arguments
                .iter()
                .filter(|argument| argument.name.is_none())
                .count()
                > 1
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
                    && matched_arg_index(call, formals, target).is_some_and(|index| {
                        call.arg_is_local_closure(index)
                            || declared_callables(parsed, call, index).is_some()
                    }) =>
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

    pub(super) fn retain_reflective_name(
        &self,
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
                | "eval"
                | "evalq"
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
        &self,
        site: ParsedSite<'_>,
        parsed: &ParsedRFile,
        call: &CallSite,
    ) -> Result<()> {
        let fact = self.resolved_call(site.package, site.image, site.lexical_environment, call)?;
        let mut canonical = call.clone();
        if let Some(name) = fact.name() {
            canonical.callee = name.as_str().into();
        }
        let call = &canonical;
        let caller = site.caller();
        let ParsedSite {
            node: from,
            package: current,
            image,
            binding,
            lexical_environment,
        } = site;
        if matches!(
            fact.callee(),
            super::call::ResolvedCallee::Conditional(Resolution::Static(BindingTarget::Base))
        ) && Self::is_slinker_semantic_callee(&call.callee)
        {
            self.diagnostic(from, current, Some(binding), RejectCode::SemanticAmbiguity,
                format!("conditionally local callee `{}` can fall through to base; linker-specific effects are path-dependent", call.callee), Some(call.span.clone()));
        }
        if !fact.base() {
            if let super::call::ResolvedCallee::Established {
                target: Resolution::Static(BindingTarget::External { package, .. }),
                ..
            } = fact.callee()
            {
                match self.packages.name(*package).as_str() {
                    "utils" => self.utils_call(caller, call)?,
                    "rlang" => self.rlang_call(caller, call)?,
                    _ => {}
                }
            } else if self.is_search_path_data_call(current, image, lexical_environment, call)? {
                self.data_call(caller, call)?;
            }
            return Ok(());
        }
        if let Some((operation, name)) = created_name(call) {
            self.dynamic_names.lock().observe_creator(NameCreator {
                node: site.node,
                package: current,
                binding: BindingName::from(binding),
                operation,
                name,
            });
        }
        if matches!(
            call.callee.as_str(),
            "assign" | "delayedAssign" | "makeActiveBinding" | "unlockBinding"
        ) {
            self.reflection.lock().observe_namespace_mutation(
                super::reflection::NamespaceMutation {
                    node: from,
                    package: current,
                    binding: binding.into(),
                    call: call.clone(),
                },
            );
        }
        if matches!(
            call.callee.as_str(),
            "options"
                | "getOption"
                | "getwd"
                | "setwd"
                | "Sys.getenv"
                | "Sys.setenv"
                | "Sys.unsetenv"
                | "Sys.getlocale"
                | "Sys.setlocale"
                | "Sys.time"
                | "Sys.Date"
                | "cat"
                | "print"
                | "message"
                | "warning"
                | "system"
                | "system2"
        ) {
            self.reflection.lock().observe_process(ProcessObservation {
                node: from,
                package: current,
                binding: binding.into(),
                operation: call.callee.as_str().into(),
                span: call.span.clone(),
            });
        }
        if let Some((formals, target)) = reflective_name_formals(&call.callee) {
            return self.reflective_lookup(site, parsed, call, formals, target);
        }
        match call.callee.as_str() {
            "eval" => self.diagnostic(
                from, current, Some(binding), RejectCode::SemanticAmbiguity,
                "eval() executable code and environment are not established by the target frontend",
                Some(call.span.clone()),
            ),
            "evalq" if call.arg_count() != 1 || matched_arg_index(call, &["expr", "envir", "enclos"], "expr") != Some(0) => self.diagnostic(
                from, current, Some(binding), RejectCode::SemanticAmbiguity,
                "evalq() with an unproven evaluation environment is unsupported",
                Some(call.span.clone()),
            ),
            "environment<-" | "parent.env<-" | "list2env" | "reg.finalizer" | "registerNamespace" | "unregisterNamespace" => self.diagnostic(
                from, current, Some(binding), RejectCode::EnvironmentMutation,
                "runtime closure or environment construction is not established by the static profile",
                Some(call.span.clone()),
            ),
            "library" | "require" => {
                self.attachment_call(from, current, call)?;
            }
            "requireNamespace" => {
                self.namespace_operation(caller, parsed, call, NamespaceCall::Require)?;
            }
            "loadNamespace" => {
                self.namespace_operation(
                    caller,
                    parsed,
                    call,
                    NamespaceCall::Operation(NamespaceOperation::Load),
                )?;
            }
            "getNamespace" => {
                self.namespace_operation(
                    caller,
                    parsed,
                    call,
                    NamespaceCall::Operation(NamespaceOperation::Get),
                )?;
            }
            "asNamespace" => {
                self.namespace_operation(
                    caller,
                    parsed,
                    call,
                    NamespaceCall::Operation(NamespaceOperation::As),
                )?;
            }
            "getNamespaceImports" => {
                self.namespace_argument(caller, call, &["ns"], "ns")?;
            }
            "getNamespaceInfo" => {
                self.namespace_info_query(caller, call)?;
            }
            "getExportedValue" => {
                self.namespace_argument(caller, call, &["ns", "name"], "ns")?;
            }
            "getNamespaceExports" | "getNamespaceName" | "getNamespaceVersion" => {
                self.namespace_argument(caller, call, &["ns"], "ns")?;
            }
            "loadedNamespaces" => self.reflection.lock().observe_registry(ProcessObservation {
                node: from, package: current, binding: binding.into(), operation: call.callee.as_str().into(), span: call.span.clone(),
            }),
            "isNamespaceLoaded" => {
                let name = match matched_static_arg(call, &["name"], "name") {
                    Some(StaticArg::String(name)) => Some(name.clone()),
                    _ => None,
                };
                self.loaded_query(caller, call, name.as_deref());
            }
            "%in%" => {
                if let Some(name) =
                    self.loaded_membership(current, image, lexical_environment, parsed, call)?
                {
                    self.loaded_query(caller, call, Some(&name));
                }
            }
            "attachNamespace" | "unloadNamespace" => {
                self.installed_package_query(caller, call, &["ns"], "ns")?;
            }
            "path.package" => {
                self.installed_package_query(caller, call, &["package"], "package")?;
            }
            "library.dynam" => {
                self.installed_package_query(caller, call, &["chname", "package"], "package")?;
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
                self.relocations.lock().observe(SyntaxObservation {
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

    fn attachment_call(&self, from: NodeId, current: PackageId, call: &CallSite) -> Result<()> {
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
        &self,
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
        if let Some(component) =
            self.native_component_for_call(current, image, lexical_environment, call)?
        {
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
        &self,
        caller @ Caller {
            node: from,
            package: current,
            binding,
        }: Caller<'_>,
        parsed: &ParsedRFile,
        call: &CallSite,
        operation: NamespaceCall,
    ) -> Result<()> {
        let literal = static_string_arg(call);
        let Some(name) = literal else {
            for usage in parsed
                .expressions
                .iter()
                .flat_map(|expression| &expression.namespace_uses)
            {
                if usage.producer != call.id() {
                    continue;
                }
                let crate::syntax::NamespaceUseKind::Argument { call: consumer, .. } =
                    &usage.operation
                else {
                    continue;
                };
                if let Some(fact) = self.calls.lock().get(consumer).cloned()
                    && fact.base()
                    && fact.name().is_some_and(|name| name == "exists")
                    && matches!(
                        matched_static_arg(
                            fact.syntax(),
                            &["x", "where", "envir", "frame", "mode", "inherits"],
                            "inherits"
                        ),
                        Some(StaticArg::Logical(false))
                    )
                {
                    return Ok(());
                }
            }
            if let Some(names) = namespace_formal(&call.callee)
                .and_then(|formal| declared_strings(parsed, call, &[formal], formal))
            {
                for name in names {
                    self.declared_namespace_name(caller, call, operation, &name)?;
                }
                return Ok(());
            }
            self.diagnostic(
                from,
                current,
                Some(binding),
                RejectCode::DynamicPackageDiscovery,
                format!("{}() with an unproven namespace name", call.callee),
                Some(call.span.clone()),
            );
            return Ok(());
        };
        self.namespace_operation_named(caller, call, operation, name)
    }

    fn namespace_operation_named(
        &self,
        caller @ Caller {
            node: from,
            package: current,
            ..
        }: Caller<'_>,
        call: &CallSite,
        operation: NamespaceCall,
        name: &str,
    ) -> Result<()> {
        let target = match self.discovered_package(from, current, call, name)? {
            Discovered::Linked(target) => target,
            Discovered::Settled => return Ok(()),
            Discovered::Optional => {
                if operation == NamespaceCall::Require {
                    self.optional_availability_blocker(caller, name, &call.span);
                }
                return Ok(());
            }
            Discovered::Missing => {
                if operation == NamespaceCall::Require {
                    self.relocations
                        .lock()
                        .push(PendingRelocation::RequireNamespace {
                            source: call.span.clone(),
                            loaded: None,
                        });
                } else {
                    self.record_missing_package(
                        from,
                        current,
                        name,
                        EdgeKind::Discovery,
                        format!("{} requires unavailable namespace {name}", call.callee),
                        Some(call.span.clone()),
                    );
                }
                return Ok(());
            }
        };
        let (formals, rewritable) = operation.arguments();
        if !only_package_argument(call, formals, formals[0], rewritable) {
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
        self.relocations.lock().push(PendingRelocation::namespace(
            call.span.clone(),
            target,
            operation,
        ));
        Ok(())
    }

    fn declared_namespace_name(
        &self,
        Caller {
            node: from,
            package: current,
            binding,
        }: Caller<'_>,
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
        &self,
        current: PackageId,
        image: &PackageImage,
        lexical_environment: &EnvironmentLabel,
        call: &CallSite,
    ) -> Result<bool> {
        Ok(self
            .resolved_call(current, image, lexical_environment, call)?
            .base())
    }

    fn identity_query(
        &self,
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
        } else if !only_package_argument(call, &["pkg", "lib.loc"], "pkg", &[]) {
            self.diagnostic(
                from,
                current,
                None,
                RejectCode::UnsupportedRootTransformation,
                format!("packageVersion() on Linked `{name}` passes a library location"),
                Some(call.span.clone()),
            );
        } else {
            self.relocations
                .lock()
                .push(PendingRelocation::PackageVersion {
                    source: call.span.clone(),
                    version: self.packages.identity(target).version,
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
        Some(StaticArg::Symbol(_) | StaticArg::Logical(_)) | None => CreatedName::Any,
    };
    Some((operation, name))
}
