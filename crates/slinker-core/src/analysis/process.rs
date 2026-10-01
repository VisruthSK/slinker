use super::arguments::native_selector_span;
use super::dynamic_names::{CreatedName, CreatorOperation, NameCreator};
use super::execute::ExecutionContext;
use super::object_world::{ClosureId, ObjectId};
use super::parse_cache::ParseState;
use super::resolution::{BindingTarget, OpenReason, ReferenceUse, Resolution};
use super::s3::{CallableId, callable_target};
use super::state::{AnalyzerState, ParseRequest, ParsedSite};
use crate::analysis::{EdgeKind, LifecycleHook, Need, NodeId, RejectCode};
use crate::ir::ExternalBindingAccess;
use crate::package::EnvironmentLabel;
use crate::package::PackageRole;
use crate::package::{
    BindingName, BindingRepresentation, CanonicalSyntax, ClosureSource, Digest, ObjectImage,
    ObjectKind, PackageId, PackageImage, PackageProvider, SyntaxValidation,
};
use crate::syntax::{
    ActiveBindingDef, NameRefKind, NamespaceInfoReceiver, OakParser, ParsedExpression, ParsedRFile,
    SemanticIssueKind, SourceId, SourceKey, Span, StaticEnvironment, SyntaxEffect,
    SyntaxEffectKind,
};
use crate::{Error, Result};
use std::sync::Arc;

struct ClosureSite<'a> {
    node: NodeId,
    package: PackageId,
    image: &'a Arc<PackageImage>,
    owner: &'a SourceKey,
    key: &'a SourceKey,
    closure: &'a ClosureSource,
}

#[derive(Clone, Copy)]
struct ObjectSite<'a> {
    node: NodeId,
    package: PackageId,
    binding: &'a BindingName,
    private: Option<&'a EnvironmentLabel>,
}

impl<P: PackageProvider> AnalyzerState<P> {
    pub(super) fn process_closure_execution(
        &mut self,
        id: PackageId,
        closure: ClosureId,
    ) -> Result<()> {
        let node = self.need_node(&Need::ClosureExecution {
            package: id,
            closure,
        });
        if self.packages.is_external(id) {
            self.external.insert(id);
            return Ok(());
        }
        let image = self.image(id)?;
        let Some(execution) = self.closure_execution_source(id, closure) else {
            self.diagnostic(
                node,
                id,
                None,
                RejectCode::UnsupportedObject,
                "executable closure is missing from the package object graph",
                None,
            );
            return Ok(());
        };
        let source = ClosureSource {
            source: Arc::clone(&execution.closure.source),
            environment: execution.environment,
        };
        self.analyze_closure(&ClosureSite {
            node,
            package: id,
            image: &image,
            owner: &execution.owner,
            key: &execution.key,
            closure: &source,
        })
    }

    pub(super) fn process_binding(&mut self, id: PackageId, binding: &BindingName) -> Result<()> {
        let node = self.need_node(&Need::Binding {
            package: id,
            binding: binding.clone(),
        });
        if self.packages.is_external(id) {
            self.external.insert(id);
            return Ok(());
        }
        let image = self.binding_image(id, binding)?;
        let Some(binding_image) = image.binding(binding) else {
            return self.process_absent_binding(node, id, &image, binding);
        };
        self.diagnose_object(
            ObjectSite {
                node,
                package: id,
                binding,
                private: None,
            },
            &binding_image.object,
        );
        let object = self.objects.graph(id).namespace_binding(binding);
        self.require_member_closures(node, id, object);
        if let Some(closure) = &binding_image.object.closure {
            let key = SourceKey::Binding(binding.clone());
            let site = ClosureSite {
                node,
                package: id,
                image: &image,
                owner: &key,
                key: &key,
                closure,
            };
            if let Some(parsed) = self.parse_closure(&site)? {
                self.check_linked_onload_libname(node, id, binding, &parsed);
                self.process_closure(&site, &parsed)?;
            }
        }
        Ok(())
    }

    fn process_absent_binding(
        &mut self,
        node: NodeId,
        id: PackageId,
        image: &Arc<PackageImage>,
        binding: &str,
    ) -> Result<()> {
        if binding != ".onLoad" && image.index.lifecycle.on_load {
            self.ensure_on_load_analyzed(id)?;
        }
        if self
            .loaded
            .get(&id)
            .is_some_and(|loaded| loaded.namespace.contains(binding))
            && !image.index.binding_names.iter().any(|name| name == binding)
        {
            let lifecycle = self.need_node(&Need::Lifecycle {
                package: id,
                hook: LifecycleHook::OnLoad,
            });
            self.depend(
                node,
                lifecycle,
                EdgeKind::Lifecycle,
                format!("activation creates active binding `{binding}`"),
                None,
            );
            return Ok(());
        }
        if self.is_root(id)
            && image
                .index
                .exports
                .values()
                .any(|exported_binding| exported_binding == binding)
            && self.process_root_reexport(node, id, image, binding)?
        {
            return Ok(());
        }
        self.diagnostic(
            node,
            id,
            Some(binding),
            RejectCode::UnresolvedBinding,
            format!("installed namespace has no binding `{binding}`"),
            None,
        );
        Ok(())
    }

    fn process_root_reexport(
        &mut self,
        node: NodeId,
        id: PackageId,
        image: &Arc<PackageImage>,
        binding: &str,
    ) -> Result<bool> {
        let resolved = self.resolve_name(id, image, binding)?;
        match resolved {
            Resolution::Static(BindingTarget::Imported {
                package,
                binding: foreign_binding,
            }) => {
                self.require(
                    node,
                    Need::Activation { package },
                    EdgeKind::Export,
                    format!("root re-export `{binding}` requires namespace activation"),
                );
                self.require(
                    node,
                    Need::Binding { package, binding: foreign_binding.clone() },
                    EdgeKind::Export,
                    format!("root re-export `{binding}` resolves to imported binding `{foreign_binding}`"),
                );
                return Ok(true);
            }
            Resolution::Static(BindingTarget::External {
                package,
                binding: foreign_binding,
            }) => {
                let external = self.external_binding(
                    package,
                    &foreign_binding,
                    ExternalBindingAccess::Exported,
                    None,
                );
                self.depend(
                    node,
                    external,
                    EdgeKind::Export,
                    format!("root re-export `{binding}` resolves to External `{foreign_binding}`"),
                    None,
                );
                return Ok(true);
            }
            Resolution::Static(BindingTarget::Native {
                package,
                component,
                binding: native_binding,
            }) => {
                self.require(
                    node,
                    Need::Native { package, component: component.clone() },
                    EdgeKind::Export,
                    format!("root export `{binding}` resolves to registered native symbol `{native_binding}` in `{component}`"),
                );
                return Ok(true);
            }
            Resolution::Static(BindingTarget::Base | BindingTarget::Metadata { .. }) => {
                return Ok(true);
            }
            Resolution::OpenDynamic(OpenReason::MissingPackage {
                package,
                binding: foreign_binding,
            }) => {
                let detail = foreign_binding.as_deref().map_or_else(
                    || format!("root re-export `{binding}` requires missing namespace {package}"),
                    |name| format!("root re-export `{binding}` requires missing {package}::{name}"),
                );
                self.record_missing_package(node, id, &package, EdgeKind::Export, detail, None);
                return Ok(true);
            }
            Resolution::OpenDynamic(OpenReason::Unresolved(name)) => {
                self.diagnostic(
                    node,
                    id,
                    Some(binding),
                    RejectCode::UnresolvedBinding,
                    format!("exported name `{binding}` resolves to unknown binding `{name}`"),
                    None,
                );
                return Ok(true);
            }
            Resolution::Static(
                BindingTarget::Namespace { .. }
                | BindingTarget::Private { .. }
                | BindingTarget::Closure { .. }
                | BindingTarget::Local,
            ) => {}
        }
        Ok(false)
    }

    fn check_linked_onload_libname(
        &mut self,
        node: NodeId,
        id: PackageId,
        binding: &BindingName,
        parsed: &ParsedRFile,
    ) {
        let reads_libname = binding == ".onLoad"
            && self.packages.role(id) == PackageRole::Linked
            && parsed.expressions.first().is_some_and(|expression| {
                expression
                    .parameters
                    .first()
                    .is_some_and(|libname| expression.used_parameters.contains(libname))
            });
        if reads_libname {
            self.diagnostic(
                node,
                id,
                Some(binding),
                RejectCode::UnsupportedLinkedLibname,
                "Linked .onLoad reads libname, which has no installed library once linked",
                None,
            );
        }
    }

    fn parse_closure(&mut self, site: &ClosureSite<'_>) -> Result<Option<Arc<ParsedRFile>>> {
        if site.closure.environment.is_unsupported() {
            self.diagnostic(
                site.node,
                site.package,
                Some(&site.owner.to_string()),
                RejectCode::UnknownClosureEnclosure,
                format!(
                    "closure enclosure `{}` cannot be modeled",
                    site.closure.environment
                ),
                None,
            );
        }
        self.parsed_source(
            site.package,
            &site.closure.source,
            site.image,
            &site.closure.environment,
            ParseRequest {
                owner: site.owner,
                source_key: site.key,
                owner_node: site.node,
            },
        )
    }

    fn process_closure(&mut self, site: &ClosureSite<'_>, parsed: &ParsedRFile) -> Result<()> {
        let image = self.prepare_construction_image(
            site.package,
            site.image,
            &site.closure.environment,
            parsed,
        )?;
        self.process_parsed(
            site.node,
            site.package,
            &image,
            &site.owner.to_string(),
            &site.closure.environment,
            parsed,
        )
    }

    fn analyze_closure(&mut self, site: &ClosureSite<'_>) -> Result<()> {
        if let Some(parsed) = self.parse_closure(site)? {
            self.process_closure(site, &parsed)?;
        }
        Ok(())
    }

    fn diagnose_object(&mut self, site: ObjectSite<'_>, object: &ObjectImage) {
        let ObjectSite {
            node,
            package,
            binding,
            private,
        } = site;
        let subject = private
            .map(|environment| format!("private binding {environment}${binding}: "))
            .unwrap_or_default();
        let issues = object
            .issues
            .iter()
            .map(|issue| format!("{}: {} ({})", issue.path, issue.kind, issue.detail))
            .collect::<Vec<_>>();
        let findings = [
            (!issues.is_empty()).then(|| (RejectCode::UnsupportedObject, issues.join("; "))),
            (object.representation == BindingRepresentation::ActiveBinding).then(|| {
                (
                    RejectCode::ActiveBinding,
                    "active binding is preserved without execution".to_owned(),
                )
            }),
            match &object.object_kind {
                ObjectKind::Unsupported(kind) => Some((
                    RejectCode::UnsupportedObject,
                    format!("unsupported installed object type `{kind}`"),
                )),
                ObjectKind::Unavailable => Some((
                    RejectCode::UnsupportedObject,
                    "installed binding could not be forced".to_owned(),
                )),
                _ => None,
            },
        ];
        for (code, message) in findings.into_iter().flatten() {
            self.diagnostic(
                node,
                package,
                Some(binding),
                code,
                format!("{subject}{message}"),
                None,
            );
        }
    }

    fn require_member_closures(&mut self, node: NodeId, id: PackageId, object: Option<ObjectId>) {
        let graph = self.objects.graph(id);
        let closures = object
            .and_then(|object| graph.members_of(object))
            .into_iter()
            .flat_map(|members| members.values())
            .filter_map(|member| graph.closure_of(*member))
            .collect::<Vec<_>>();
        for closure in closures {
            let need = Need::ClosureExecution {
                package: id,
                closure,
            };
            let closure_node = self.need_node(&need);
            self.value_closures.insert(closure_node);
            self.require(
                node,
                need,
                EdgeKind::ClosureExecution,
                "a retained value holds an executable closure",
            );
        }
    }

    pub(super) fn process_private_binding(
        &mut self,
        id: PackageId,
        environment: &EnvironmentLabel,
        binding: &BindingName,
    ) -> Result<()> {
        let node = self.need_node(&Need::PrivateBinding {
            package: id,
            environment: environment.clone(),
            binding: binding.clone(),
        });
        if self.packages.is_external(id) {
            self.external.insert(id);
            return Ok(());
        }
        let image = self.image(id)?;
        let Some(binding_image) = image.private_binding(environment, binding) else {
            self.diagnostic(
                node,
                id,
                Some(binding),
                RejectCode::UnresolvedBinding,
                format!("private environment `{environment}` has no binding `{binding}`"),
                None,
            );
            return Ok(());
        };
        self.diagnose_object(
            ObjectSite {
                node,
                package: id,
                binding,
                private: Some(environment),
            },
            &binding_image.object,
        );
        let object = {
            let graph = self.objects.graph(id);
            graph
                .environment_id(environment)
                .and_then(|private| graph.environment(private).bindings.get(binding).copied())
        };
        self.require_member_closures(node, id, object);
        if let Some(closure) = &binding_image.object.closure {
            let key = SourceKey::private(environment.clone(), binding.clone());
            self.analyze_closure(&ClosureSite {
                node,
                package: id,
                image: &image,
                owner: &key,
                key: &key,
                closure,
            })?;
        }
        Ok(())
    }

    fn process_parsed(
        &mut self,
        node: NodeId,
        package: PackageId,
        image: &PackageImage,
        binding: &str,
        lexical_environment: &EnvironmentLabel,
        parsed: &ParsedRFile,
    ) -> Result<()> {
        let site = ParsedSite {
            node,
            package,
            image,
            binding,
            lexical_environment,
        };
        self.report_semantic_issues(site, parsed);
        for expression in &parsed.expressions {
            self.execute_construction(
                ExecutionContext {
                    node,
                    package,
                    image,
                    lexical_environment,
                    depth: 0,
                    specialized: false,
                },
                &expression.construction,
            )?;
            self.register_active_bindings(site, expression)?;
            let consumed_native_selectors = self.consumed_native_selectors(site, expression)?;
            self.process_references(site, parsed, expression, &consumed_native_selectors)?;
            self.process_package_refs(site, expression)?;
            for resource in &expression.resource_refs {
                if !self.guards_active(site, &resource.guards, &resource.span)? {
                    continue;
                }
                self.resource_access(site, parsed, expression, resource)?;
            }
            self.record_non_reflective_namespace_uses(expression);
            self.process_calls(site, parsed, expression)?;
            self.process_declared_callable_calls(site, parsed, expression)?;
            self.process_namespace_info_reads(site, expression);
            self.process_namespace_enumerations(site, expression);
            self.process_effects(site, expression)?;
        }
        Ok(())
    }

    fn report_semantic_issues(&mut self, site: ParsedSite<'_>, parsed: &ParsedRFile) {
        for issue in &parsed.issues {
            let code = match issue.kind {
                SemanticIssueKind::AmbiguousEffect => RejectCode::SemanticAmbiguity,
                SemanticIssueKind::AmbiguousAttachOrder => RejectCode::SemanticAmbiguity,
                SemanticIssueKind::UninstalledPackage => RejectCode::MissingDependency,
                SemanticIssueKind::SourceCycle => RejectCode::SemanticAmbiguity,
                SemanticIssueKind::InvalidDeclaration => RejectCode::InvalidDeclaration,
            };
            self.diagnostic(
                site.node,
                site.package,
                Some(site.binding),
                code,
                issue.message.clone(),
                issue.span.clone(),
            );
        }
    }

    fn register_active_bindings(
        &mut self,
        site: ParsedSite<'_>,
        expression: &ParsedExpression,
    ) -> Result<()> {
        for active in &expression.active_bindings {
            if site.binding != ".onLoad" || !active.certain {
                continue;
            }
            if !self.guards_active(site, &active.guards, &active.span)? {
                continue;
            }
            if self.active_binding_targets_current_namespace(
                site.package,
                site.image,
                site.lexical_environment,
                active,
            )? && self
                .loaded(site.package)?
                .namespace
                .add_binding(active.name.clone().into())
            {
                self.non_returning_bindings.remove(&site.package);
            }
        }
        Ok(())
    }

    fn consumed_native_selectors(
        &mut self,
        site: ParsedSite<'_>,
        expression: &ParsedExpression,
    ) -> Result<Vec<Span>> {
        let mut consumed_native_selectors = Vec::new();
        for call in &expression.calls {
            if !self.guards_active(site, &call.guards, &call.span)?
                || !matches!(
                    call.callee.as_str(),
                    ".Call" | ".External" | ".C" | ".Fortran"
                )
                || !self.call_resolves_definitely_to_base(
                    site.package,
                    site.image,
                    site.lexical_environment,
                    call,
                )?
            {
                continue;
            }
            let Some(target) = self.native_component_for_call(
                site.package,
                site.image,
                site.lexical_environment,
                call,
            )?
            else {
                continue;
            };
            if target.consumes_selector
                && let Some(span) = native_selector_span(call)
            {
                consumed_native_selectors.push(span.clone());
            }
        }
        Ok(consumed_native_selectors)
    }

    fn process_references(
        &mut self,
        site: ParsedSite<'_>,
        parsed: &ParsedRFile,
        expression: &ParsedExpression,
        consumed_native_selectors: &[Span],
    ) -> Result<()> {
        let enclosure_known = !site.lexical_environment.starts_with("unsupported:");
        for reference in &expression.references {
            if !self.guards_active(site, &reference.guards, &reference.span)? {
                continue;
            }
            if consumed_native_selectors.contains(&reference.span) {
                continue;
            }
            if expression.namespace_info_reads.iter().any(|read| {
                read.span == reference.span
                    && read.receiver == NamespaceInfoReceiver::Lexical
                    && reproduces_namespace_info(read.field.as_deref())
            }) {
                continue;
            }
            let resolved = self.resolve_lexical_name(
                site.package,
                site.image,
                site.lexical_environment,
                &reference.name,
            )?;
            if reference.name == "environment<-"
                && matches!(resolved, Resolution::Static(BindingTarget::Base))
            {
                self.dynamic_names.observe_creator(NameCreator {
                    node: site.node,
                    package: site.package,
                    binding: BindingName::from(site.binding),
                    operation: CreatorOperation::EnvironmentAssign,
                    name: CreatedName::Any,
                });
            }
            if (!enclosure_known
                || reference.kind != NameRefKind::External
                || self.value_closures.contains(&site.node))
                && matches!(
                    &resolved,
                    Resolution::OpenDynamic(OpenReason::Unresolved(_))
                )
            {
                continue;
            }
            if let Some(callable) = callable_target(&resolved)
                && !expression.calls.iter().any(|call| {
                    call.qualified_package.is_none() && call.span.start == reference.span.start
                })
            {
                let invocation =
                    self.base_apply_invocation(site, parsed, expression, &reference.span)?;
                self.record_use(callable, invocation)?;
            }
            self.require_resolved(
                site.node,
                site.package,
                Some(site.binding),
                resolved,
                reference.span.clone(),
                ReferenceUse::Recorded,
            );
        }
        Ok(())
    }

    fn process_package_refs(
        &mut self,
        site: ParsedSite<'_>,
        expression: &ParsedExpression,
    ) -> Result<()> {
        for reference in &expression.package_refs {
            if !self.guards_active(site, &reference.guards, &reference.span)? {
                continue;
            }
            self.namespace_access(site.node, site.package, reference)?;
            if let Some(foreign) = self.known_package(&reference.package)
                && !expression.calls.iter().any(|call| {
                    call.qualified_package.is_some() && call.span.start == reference.span.start
                })
            {
                self.record_escape(CallableId {
                    package: foreign,
                    binding: reference.symbol.clone().into(),
                })?;
            }
        }
        Ok(())
    }

    fn record_non_reflective_namespace_uses(&mut self, expression: &ParsedExpression) {
        let uses = expression
            .calls
            .iter()
            .filter(|call| {
                call.callee == "registerS3method"
                    || (call.callee == "exists"
                        && self.argument_text(call, "inherits") == Some("FALSE"))
            })
            .flat_map(|call| call.arg_names.iter().zip(&call.arg_spans))
            .filter(|(name, _)| name.as_deref() == Some("envir"))
            .filter_map(|(_, span)| span.clone())
            .collect();
        self.reflection.set_non_reflective_namespace_uses(uses);
    }

    fn process_calls(
        &mut self,
        site: ParsedSite<'_>,
        parsed: &ParsedRFile,
        expression: &ParsedExpression,
    ) -> Result<()> {
        for call in &expression.calls {
            if !self.guards_active(site, &call.guards, &call.span)? {
                continue;
            }
            if let Some(callable) =
                self.call_target(site.package, site.image, site.lexical_environment, call)?
            {
                self.record_invocation(parsed, callable, call)?;
            }
            self.retain_lexical_s3_methods(site, call)?;
            if matches!(call.callee.as_str(), "UseMethod" | "NextMethod")
                && call.qualified_package.is_none()
                && matches!(
                    self.resolve_lexical_name(
                        site.package,
                        site.image,
                        site.lexical_environment,
                        &call.callee,
                    )?,
                    Resolution::Static(BindingTarget::Base)
                )
            {
                self.s3_dispatch(site, parsed, Some(&expression.parameters), call)?;
                continue;
            }
            self.semantic_call(site, parsed, call)?;
        }
        Ok(())
    }

    fn process_namespace_info_reads(
        &mut self,
        site: ParsedSite<'_>,
        expression: &ParsedExpression,
    ) {
        for read in &expression.namespace_info_reads {
            if reproduces_namespace_info(read.field.as_deref()) {
                continue;
            }
            let field = read.field.as_deref().unwrap_or("<whole>");
            match &read.receiver {
                NamespaceInfoReceiver::Lexical => {}
                NamespaceInfoReceiver::Namespace(name) => {
                    let linked = self
                        .known_package(name)
                        .is_some_and(|package| self.packages.role(package) == PackageRole::Linked);
                    if linked {
                        self.diagnostic(
                            site.node,
                            site.package,
                            Some(site.binding),
                            RejectCode::UnsupportedRootTransformation,
                            format!("reads `.__NAMESPACE__.` field `{field}`, which the synthetic `{name}` namespace does not reproduce"),
                            Some(read.span.clone()),
                        );
                    }
                }
                NamespaceInfoReceiver::Computed => {
                    self.reflection.defer_computed_namespace_info_read(
                        site.node,
                        site.package,
                        site.binding,
                        field,
                        read.span.clone(),
                    );
                }
            }
        }
    }

    fn process_namespace_enumerations(
        &mut self,
        site: ParsedSite<'_>,
        expression: &ParsedExpression,
    ) {
        for enumeration in &expression.namespace_enumerations {
            let linked = self
                .known_package(&enumeration.package)
                .is_some_and(|package| self.packages.role(package) == PackageRole::Linked);
            if linked {
                self.diagnostic(
                    site.node,
                    site.package,
                    Some(site.binding),
                    RejectCode::UnsupportedRootTransformation,
                    format!(
                        "{}() reads every binding of the synthetic `{}` namespace, which holds stubs for bindings the build never reached",
                        enumeration.callee, enumeration.package
                    ),
                    Some(enumeration.span.clone()),
                );
            }
        }
    }

    fn process_effects(
        &mut self,
        site: ParsedSite<'_>,
        expression: &ParsedExpression,
    ) -> Result<()> {
        let enclosure_known = !site.lexical_environment.starts_with("unsupported:");
        for effect in &expression.effects {
            if !self.guards_active(site, &effect.guards, &effect.span)? {
                continue;
            }
            match effect.kind {
                SyntaxEffectKind::SuperAssignment => {
                    if !enclosure_known {
                        self.dynamic_names.observe_creator(NameCreator {
                            node: site.node,
                            package: site.package,
                            binding: BindingName::from(site.binding),
                            operation: CreatorOperation::SuperAssign,
                            name: CreatedName::Any,
                        });
                        continue;
                    }
                    self.handle_superassignment(
                        site.node,
                        site.package,
                        site.image,
                        site.binding,
                        site.lexical_environment,
                        effect,
                    )?;
                }
                SyntaxEffectKind::IndirectPackageWrite
                | SyntaxEffectKind::UnsupportedAssignmentTarget => {
                    self.diagnostic(
                        site.node,
                        site.package,
                        Some(site.binding),
                        RejectCode::UnsupportedTopLevelEffect,
                        format!("unsupported R effect: {:?}", effect.kind),
                        Some(effect.span.clone()),
                    );
                }
            }
        }
        Ok(())
    }

    pub(super) fn parsed_source(
        &mut self,
        id: PackageId,
        source_text: &Arc<str>,
        image: &PackageImage,
        lexical_environment: &EnvironmentLabel,
        request: ParseRequest<'_>,
    ) -> Result<Option<Arc<ParsedRFile>>> {
        let ParseRequest {
            owner,
            source_key,
            owner_node,
        } = request;
        let key = (id, source_key.clone());
        if let Some(state) = self.parses.state(&key) {
            return Ok(match state {
                ParseState::Parsed(parsed) => Some(Arc::clone(parsed)),
                ParseState::Blocked => None,
            });
        }
        let Some(source) = self.admit_source(id, owner, source_key, owner_node, source_text)?
        else {
            return Ok(None);
        };
        let context = self.oak_parse_context(id, image, lexical_environment)?;
        match OakParser.parse_binding_with_context(source, source_text.as_ref(), &context) {
            Ok(parsed) => {
                let parsed = Arc::new(parsed);
                self.parses.store(key, Arc::clone(&parsed));
                Ok(Some(parsed))
            }
            Err(error) => {
                self.handle_air_rejection(id, owner, source_key, owner_node, &error)?;
                Ok(None)
            }
        }
    }

    pub(super) fn admit_source(
        &mut self,
        id: PackageId,
        owner: &SourceKey,
        source_key: &SourceKey,
        owner_node: NodeId,
        text: &Arc<str>,
    ) -> Result<Option<SourceId>> {
        let key = (id, source_key.clone());
        let source = self
            .parses
            .register(key.clone(), self.packages.name(id), text);
        let CanonicalSyntax::Stable(normalized) = self.packages.canonical_syntax(text)? else {
            self.diagnostic(
                owner_node,
                id,
                Some(&owner.to_string()),
                RejectCode::InvalidInstalledRepresentation,
                format!(
                    "target-R canonical source for {source_key} is not stable across parse/deparse"
                ),
                Some(Span::new(source, 0, text.len())),
            );
            self.parses.block(key);
            return Ok(None);
        };
        self.parses.record_shape(key, Digest::of(&normalized));
        Ok(Some(source))
    }

    pub(super) fn handle_air_rejection(
        &mut self,
        id: PackageId,
        owner: &SourceKey,
        source_key: &SourceKey,
        owner_node: NodeId,
        air_error: &str,
    ) -> Result<()> {
        let key = (id, source_key.clone());
        let (source_id, source_text) = self.parses.registered(&key).ok_or_else(|| {
            Error::Analysis(format!(
                "missing virtual source for {}::{source_key}",
                self.packages.name(id)
            ))
        })?;
        let validation = self.packages.validate_syntax(source_text.as_ref())?;
        let span = Some(Span::new(source_id, 0, source_text.len()));
        match validation {
            SyntaxValidation::Accepted => self.diagnostic(
                owner_node,
                id,
                Some(&owner.to_string()),
                RejectCode::AirUnsupportedSyntax,
                format!(
                    "target R accepts {source_key}; Air {air_error}; analysis of this retained closure is conservatively blocked"
                ),
                span,
            ),
            SyntaxValidation::Rejected(r_error) => self.diagnostic(
                owner_node,
                id,
                Some(&owner.to_string()),
                RejectCode::InvalidInstalledRepresentation,
                format!(
                    "Air rejects generated source for {source_key} ({air_error}); target R also rejects it ({r_error})"
                ),
                span,
            ),
        }
        self.parses.block(key);
        Ok(())
    }

    fn ensure_on_load_analyzed(&mut self, id: PackageId) -> Result<()> {
        if self.packages.is_external(id) || !self.image(id)?.index.lifecycle.on_load {
            return Ok(());
        }

        let lifecycle = Need::Lifecycle {
            package: id,
            hook: LifecycleHook::OnLoad,
        };
        if self.needs.start(&lifecycle) {
            self.process_lifecycle(id, LifecycleHook::OnLoad);
        }

        let hook = Need::Binding {
            package: id,
            binding: ".onLoad".into(),
        };
        if self.needs.start(&hook) {
            self.process_binding(id, &LifecycleHook::OnLoad.binding())?;
        }
        Ok(())
    }

    fn active_binding_targets_current_namespace(
        &mut self,
        package: PackageId,
        image: &PackageImage,
        lexical_environment: &EnvironmentLabel,
        active: &ActiveBindingDef,
    ) -> Result<bool> {
        let expected = format!("namespace:{}", self.packages.name(package));
        Ok(match &active.target {
            StaticEnvironment::Namespace(name) => name == self.packages.name(package),
            StaticEnvironment::ClosureBinding(name) => {
                match self.resolve_lexical_name(package, image, lexical_environment, name)? {
                    Resolution::Static(BindingTarget::Namespace {
                        package: owner,
                        binding,
                    }) if owner == package => image
                        .binding(&binding)
                        .and_then(|binding| binding.object.closure.as_ref())
                        .is_some_and(|closure| closure.environment == expected),
                    Resolution::Static(BindingTarget::Private {
                        package: owner,
                        environment,
                        binding,
                    }) if owner == package => image
                        .private_binding(&environment, &binding)
                        .and_then(|binding| binding.object.closure.as_ref())
                        .is_some_and(|closure| closure.environment == expected),
                    _ => false,
                }
            }
        })
    }

    fn handle_superassignment(
        &mut self,
        from: NodeId,
        package: PackageId,
        image: &PackageImage,
        binding: &str,
        lexical_environment: &EnvironmentLabel,
        effect: &SyntaxEffect,
    ) -> Result<()> {
        if let Some(value) = &effect.value_symbol
            && !is_r_constant(value)
        {
            let resolved = self.resolve_lexical_name(package, image, lexical_environment, value)?;
            let opaque_native = (binding == ".onLoad"
                && matches!(resolved, Resolution::OpenDynamic(OpenReason::Unresolved(_))))
            .then(|| Self::sole_opaque_registered_native_component(&image.index))
            .flatten();
            match opaque_native {
                Some(component) => self.require_at(
                    from,
                    Need::Native {
                        package,
                        component: component.to_owned().into(),
                    },
                    EdgeKind::Native,
                    format!(
                        ".onLoad may receive registered native symbol `{value}` from `{component}`"
                    ),
                    Some(effect.span.clone()),
                ),
                None => self.require_resolved(
                    from,
                    package,
                    Some(binding),
                    resolved,
                    effect.span.clone(),
                    ReferenceUse::Unrecorded,
                ),
            }
        }

        if effect.target_enclosing_local {
            return Ok(());
        }

        let Some(target) = &effect.target else {
            self.diagnostic(
                from,
                package,
                Some(binding),
                RejectCode::EnvironmentMutation,
                "dynamic superassignment target cannot be resolved",
                Some(effect.span.clone()),
            );
            return Ok(());
        };

        match self.resolve_lexical_name(package, image, lexical_environment, target)? {
            Resolution::Static(BindingTarget::Namespace {
                package: owner,
                binding: target_binding,
            }) => self.require_at(
                from,
                Need::Binding {
                    package: owner,
                    binding: target_binding.clone(),
                },
                EdgeKind::Effect,
                format!("superassignment mutates enclosing binding `{target_binding}`"),
                Some(effect.span.clone()),
            ),
            Resolution::Static(BindingTarget::Private {
                package: owner,
                environment,
                binding: target_binding,
            }) => self.require_at(
                from,
                Need::PrivateBinding {
                    package: owner,
                    environment: environment.clone(),
                    binding: target_binding.clone(),
                },
                EdgeKind::Effect,
                format!(
                    "superassignment mutates private binding `{target_binding}` in {environment}"
                ),
                Some(effect.span.clone()),
            ),
            Resolution::Static(BindingTarget::Local)
                if lexical_environment.starts_with("derived:") => {}
            _ => self.diagnostic(
                from,
                package,
                Some(binding),
                RejectCode::EnvironmentMutation,
                format!(
                    "superassignment target `{target}` does not resolve to a mutable enclosing lexical/package/private binding"
                ),
                Some(effect.span.clone()),
            ),
        }
        Ok(())
    }
}

fn is_r_constant(name: &str) -> bool {
    matches!(
        name,
        "NULL"
            | "TRUE"
            | "FALSE"
            | "NA"
            | "NaN"
            | "Inf"
            | "NA_integer_"
            | "NA_real_"
            | "NA_complex_"
            | "NA_character_"
    )
}

fn reproduces_namespace_info(field: Option<&str>) -> bool {
    matches!(
        field,
        Some("exports" | "spec" | "imports" | "dynlibs" | "S3methods")
    )
}
