//! Oak semantic adapter.
//!
//! Air owns parsing. Oak owns lexical scopes, definitions, uses, use-def
//! relationships, lexical fallthrough, and annotated evaluation/NSE effects.
//! Slinker translates Oak results into the fact types consumed by the existing
//! demand linker. Source-text inspection below is limited to recovering static
//! arguments and spans at sites Oak has already classified as semantically live.
//! It does not decide lexical reachability or rebuild control-flow state.

use crate::syntax::facts::{
    ActiveBindingDef, CalleeKind, CallSite, EvalPhase, NameRef, NameRefKind, PackageGuard,
    PackageRef, ParsedExpression, ParsedRFile, ResourceRef, SemanticIssue, SemanticIssueKind,
    StaticArg, StaticEnvironment, SyntaxEffect, SyntaxEffectKind,
};
use crate::syntax::source::{SourceId, Span};
use crate::{Error, Result};
use air_r_parser::{parse, RParserOptions};
use air_r_syntax::RRoot;
use oak_semantic::semantic_index::{
    DefinitionKind, NamespaceAccessKind, ScopeId, SemanticDiagnostic, SemanticIndex,
};
use oak_semantic::{build_index, EffectsHandlers, ImportsResolver, SourceResolution};
use std::collections::{BTreeMap, BTreeSet};

pub trait RParser {
    fn parse(&self, source: SourceId, text: &str) -> Result<ParsedRFile>;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct OakParser;

#[derive(Debug, Clone, PartialEq, Eq)]
enum ExternalNameOrigin {
    Base,
    Imported { package: String, name: String },
    Shadowed,
    UnknownImportAll,
}

/// Resolution result for a name that has already fallen through slinker's
/// package/private/native bindings into NAMESPACE imports and base.
///
/// This table is shared by Oak's effects resolver and the linker's ordinary
/// name resolver so importFrom/import-all precedence cannot drift between the
/// two semantic paths.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum NamespaceImportResolution {
    Imported {
        package: String,
        binding: String,
        effect_name: String,
    },
    MissingImportAll {
        package: String,
        binding: String,
    },
    BaseFallback,
}

#[derive(Debug, Clone)]
struct ImportAllNamespace {
    package: String,
    /// Exported name -> installed binding name. `None` means the imported
    /// namespace was unavailable while constructing the resolver table.
    exports: Option<BTreeMap<String, String>>,
    except: BTreeSet<String>,
}

/// Immutable NAMESPACE import-resolution table.
///
/// `importFrom()` has precedence over import-all, matching slinker's existing
/// resolution contract. Import-all entries retain source order. A missing
/// import-all only makes names ambiguous once resolution actually reaches that
/// entry; it does not erase an earlier package that already exported the name.
#[derive(Debug, Default, Clone)]
pub(crate) struct NamespaceImports {
    imported_names: BTreeMap<String, (String, String)>,
    import_all: Vec<ImportAllNamespace>,
}

impl NamespaceImports {
    pub(crate) fn add_import_from(
        &mut self,
        local: impl Into<String>,
        package: impl Into<String>,
        remote: impl Into<String>,
    ) {
        self.imported_names
            .entry(local.into())
            .or_insert_with(|| (package.into(), remote.into()));
    }

    pub(crate) fn add_import_all(
        &mut self,
        package: impl Into<String>,
        exports: Option<BTreeMap<String, String>>,
        except: impl IntoIterator<Item = String>,
    ) {
        self.import_all.push(ImportAllNamespace {
            package: package.into(),
            exports,
            except: except.into_iter().collect(),
        });
    }

    pub(crate) fn resolve(&self, name: &str) -> NamespaceImportResolution {
        if let Some((package, remote)) = self.imported_names.get(name) {
            return NamespaceImportResolution::Imported {
                package: package.clone(),
                binding: remote.clone(),
                effect_name: remote.clone(),
            };
        }

        for import in &self.import_all {
            if import.except.contains(name) {
                continue;
            }
            let Some(exports) = &import.exports else {
                return NamespaceImportResolution::MissingImportAll {
                    package: import.package.clone(),
                    binding: name.to_owned(),
                };
            };
            let Some(binding) = exports.get(name) else {
                continue;
            };
            return NamespaceImportResolution::Imported {
                package: import.package.clone(),
                binding: binding.clone(),
                // Effects are attached to the package's exported function
                // name, while the linker retains the installed binding name.
                effect_name: name.to_owned(),
            };
        }

        NamespaceImportResolution::BaseFallback
    }
}

/// Installed-namespace facts supplied by slinker to Oak's import/effects
/// resolver. Local lexical state is deliberately absent: Oak owns it.
#[derive(Debug, Default, Clone)]
pub struct OakParseContext {
    shadowed_names: BTreeSet<String>,
    imports: NamespaceImports,
}

impl OakParseContext {
    pub fn new(shadowed_names: BTreeSet<String>) -> Self {
        Self {
            shadowed_names,
            imports: NamespaceImports::default(),
        }
    }

    pub(crate) fn with_imports(
        shadowed_names: BTreeSet<String>,
        imports: NamespaceImports,
    ) -> Self {
        Self {
            shadowed_names,
            imports,
        }
    }

    #[cfg(test)]
    fn add_import_from(
        &mut self,
        local: impl Into<String>,
        package: impl Into<String>,
        remote: impl Into<String>,
    ) {
        self.imports.add_import_from(local, package, remote);
    }

    fn origin(&self, name: &str) -> ExternalNameOrigin {
        if self.shadowed_names.contains(name) {
            return ExternalNameOrigin::Shadowed;
        }
        match self.imports.resolve(name) {
            NamespaceImportResolution::Imported {
                package,
                effect_name,
                ..
            } => ExternalNameOrigin::Imported {
                package,
                name: effect_name,
            },
            NamespaceImportResolution::MissingImportAll { .. } => {
                ExternalNameOrigin::UnknownImportAll
            }
            NamespaceImportResolution::BaseFallback => ExternalNameOrigin::Base,
        }
    }

    fn resolves_to_base(&self, name: &str) -> bool {
        matches!(self.origin(name), ExternalNameOrigin::Base)
    }
}

struct SlinkerImportsResolver<'a> {
    context: &'a OakParseContext,
}

impl ImportsResolver for SlinkerImportsResolver<'_> {
    fn resolve_source(&mut self, _path: &str) -> Option<SourceResolution> {
        // Installed package binding source is self-contained here. Slinker does
        // not model workspace `source()` injection as part of installed-package
        // image linking.
        None
    }

    fn resolve_effects(&mut self, name: &str, attached: &[String]) -> Option<EffectsHandlers> {
        // A search-path attach can mask base. The linker rejects supported
        // attachment calls, so when an attached package has no registered Oak
        // effect we conservatively refuse to claim the base identity.
        if !attached.is_empty() {
            // Search-path attachment is outside slinker's supported contract.
            // Without each attached package's full export table we cannot prove
            // which package owns a bare name: a later package with no Oak
            // annotation may still mask an earlier annotated function. Refuse
            // to assign effects rather than skipping possible maskers.
            return None;
        }

        match self.context.origin(name) {
            ExternalNameOrigin::Base => oak_semantic::effects::lookup("base", name).copied(),
            ExternalNameOrigin::Imported { package, name } => {
                oak_semantic::effects::lookup(&package, &name).copied()
            }
            ExternalNameOrigin::Shadowed | ExternalNameOrigin::UnknownImportAll => None,
        }
    }

    fn resolve_qualified_effects(&mut self, package: &str, name: &str) -> Option<EffectsHandlers> {
        oak_semantic::effects::lookup(package, name).copied()
    }

    fn package_exists(&mut self, _package: &str) -> bool {
        // Availability and target-library policy belong to the linker. Returning
        // true prevents Oak from inventing missing-package diagnostics without
        // access to slinker's selected package universe.
        true
    }
}

#[derive(Debug, Clone)]
struct LiveUse {
    name: String,
    start: usize,
    end: usize,
    callee_kind: CalleeKind,
    phase: EvalPhase,
}

#[derive(Debug, Clone)]
struct RawArgument {
    name: Option<String>,
    value_start: usize,
    value_end: usize,
    static_arg: Option<StaticArg>,
}

#[derive(Debug, Clone)]
struct RawCall {
    start: usize,
    end: usize,
    args: Vec<RawArgument>,
}

#[derive(Debug, Clone)]
struct LiveCall {
    site: CallSite,
    raw: RawCall,
}

#[derive(Debug, Clone, Copy)]
struct IfRegion {
    condition_start: usize,
    condition_end: usize,
    then_start: usize,
    then_end: usize,
    else_start: Option<usize>,
    else_end: Option<usize>,
}

#[derive(Debug, Clone)]
struct SuperAssignmentParts {
    span_start: usize,
    span_end: usize,
    value_symbol: Option<(String, usize, usize)>,
}

impl OakParser {
    pub fn parse_binding(
        &self,
        source: SourceId,
        text: &str,
    ) -> std::result::Result<ParsedRFile, String> {
        self.parse_binding_with_context(source, text, &OakParseContext::default())
    }

    pub fn parse_binding_with_context(
        &self,
        source: SourceId,
        text: &str,
        context: &OakParseContext,
    ) -> std::result::Result<ParsedRFile, String> {
        let parsed = parse(text, RParserOptions::default());
        if let Some(error) = parsed.error() {
            return Err(error.to_string());
        }
        let root = parsed.tree();
        let index = build_semantic_index(&root, context);
        Ok(translate_index(source, text, context, &index))
    }
}

fn build_semantic_index(root: &RRoot, context: &OakParseContext) -> SemanticIndex {
    build_index(root, SlinkerImportsResolver { context })
}

fn translate_index(
    source: SourceId,
    text: &str,
    context: &OakParseContext,
    index: &SemanticIndex,
) -> ParsedRFile {
    let mut references = Vec::new();
    let mut live_uses = Vec::new();

    for scope in index.scope_ids() {
        let phase = phase_for_scope(index, scope);
        for (use_id, use_site) in index.uses(scope).iter() {
            let symbol = index.symbols(scope).symbol(use_site.symbol());
            let name = symbol.name().to_owned();
            let range = use_site.range();
            let start = text_offset(range.start());
            let end = text_offset(range.end());
            let bound = index.use_is_bound(scope, use_id);
            let has_reaching_definitions = index.reaching_definitions(scope, use_id).next().is_some();
            let callee_kind = if bound {
                CalleeKind::DefinitelyLexical
            } else if has_reaching_definitions {
                CalleeKind::ConditionalFallthrough
            } else {
                CalleeKind::DefinitelyExternal
            };

            live_uses.push(LiveUse {
                name: name.clone(),
                start,
                end,
                callee_kind,
                phase,
            });

            let kind = match callee_kind {
                CalleeKind::DefinitelyLexical => continue,
                CalleeKind::DefinitelyExternal => NameRefKind::External,
                CalleeKind::ConditionalFallthrough => NameRefKind::ConditionalFallthrough,
            };
            references.push(NameRef {
                name,
                kind,
                phase,
                guards: Vec::new(),
                span: Span::new(source.clone(), start, end),
            });
        }
    }

    let mut package_refs = Vec::new();
    let mut live_calls = Vec::new();

    for live_use in &live_uses {
        let Some(raw) = call_after_name(text, live_use.start, live_use.end) else {
            continue;
        };
        live_calls.push(LiveCall {
            site: CallSite {
                callee: live_use.name.clone(),
                callee_kind: live_use.callee_kind,
                qualified_package: None,
                args: raw.args.iter().map(|argument| argument.static_arg.clone()).collect(),
                phase: live_use.phase,
                guards: Vec::new(),
                span: Span::new(source.clone(), raw.start, raw.end),
            },
            raw,
        });
    }

    for access in index.namespace_accesses() {
        let start = text_offset(access.offset());
        let (access_end, internal) = namespace_extent(text, start).unwrap_or_else(|| {
            let operator = match access.kind() {
                NamespaceAccessKind::Export => 2,
                NamespaceAccessKind::Internal => 3,
            };
            (
                (start + access.package().len() + operator + access.symbol().len()).min(text.len()),
                matches!(access.kind(), NamespaceAccessKind::Internal),
            )
        });
        package_refs.push(PackageRef {
            package: access.package().to_owned(),
            symbol: access.symbol().to_owned(),
            internal,
            guards: Vec::new(),
            span: Span::new(source.clone(), start, access_end),
        });

        if let Some(raw) = call_after_name(text, start, access_end) {
            let (scope, _) = index.scope_at(access.offset());
            live_calls.push(LiveCall {
                site: CallSite {
                    callee: access.symbol().to_owned(),
                    callee_kind: CalleeKind::DefinitelyExternal,
                    qualified_package: Some(access.package().to_owned()),
                    args: raw.args.iter().map(|argument| argument.static_arg.clone()).collect(),
                    phase: phase_for_scope(index, scope),
                    guards: Vec::new(),
                    span: Span::new(source.clone(), raw.start, raw.end),
                },
                raw,
            });
        }
    }

    deduplicate_calls(&mut live_calls);

    let if_regions = find_if_regions(text);
    let mut guard_regions = if_guard_regions(text, context, &if_regions, &live_calls);
    apply_guard_regions_to_references(&guard_regions, &mut references);
    apply_guard_regions_to_package_refs(&guard_regions, &mut package_refs);
    apply_guard_regions_to_calls(&guard_regions, &mut live_calls);

    let hook_regions = hook_guard_regions(context, &live_calls);
    apply_guard_regions_to_references(&hook_regions, &mut references);
    apply_guard_regions_to_package_refs(&hook_regions, &mut package_refs);
    apply_guard_regions_to_calls(&hook_regions, &mut live_calls);
    guard_regions.extend(hook_regions);

    let resource_refs = collect_resources(source.clone(), context, &live_calls);
    let active_bindings = collect_active_bindings(
        source.clone(),
        text,
        context,
        index,
        &live_calls,
        &if_regions,
    );
    let (mut effects, suppressed_reference_spans) = collect_superassignments(source.clone(), text, index);
    references.retain(|reference| {
        !suppressed_reference_spans.iter().any(|(name, start, end)| {
            reference.name == *name && reference.span.start == *start && reference.span.end == *end
        })
    });
    apply_guard_regions_to_effects(&guard_regions, &mut effects);

    let calls = live_calls.into_iter().map(|call| call.site).collect();
    let issues = translate_diagnostics(source.clone(), index);

    ParsedRFile {
        expressions: vec![ParsedExpression {
            span: Span::new(source, 0, text.len()),
            definitions: Vec::new(),
            references,
            package_refs,
            resource_refs,
            calls,
            active_bindings,
            effects,
        }],
        issues,
    }
}

fn phase_for_scope(index: &SemanticIndex, scope: ScopeId) -> EvalPhase {
    if index.scope_is_eager(scope) {
        EvalPhase::Materialization
    } else {
        EvalPhase::Runtime
    }
}

fn text_offset(offset: impl Into<u32>) -> usize {
    offset.into() as usize
}

fn is_base_call(context: &OakParseContext, call: &CallSite) -> bool {
    if call.callee_kind != CalleeKind::DefinitelyExternal {
        return false;
    }
    match call.qualified_package.as_deref() {
        Some("base") => true,
        Some(_) => false,
        None => context.resolves_to_base(&call.callee),
    }
}

fn deduplicate_calls(calls: &mut Vec<LiveCall>) {
    calls.sort_by(|left, right| {
        (
            left.site.span.start,
            left.site.span.end,
            left.site.qualified_package.as_deref(),
            left.site.callee.as_str(),
        )
            .cmp(&(
                right.site.span.start,
                right.site.span.end,
                right.site.qualified_package.as_deref(),
                right.site.callee.as_str(),
            ))
    });
    calls.dedup_by(|left, right| {
        left.site.span.start == right.site.span.start
            && left.site.span.end == right.site.span.end
            && left.site.callee == right.site.callee
            && left.site.qualified_package == right.site.qualified_package
    });
}

fn collect_resources(
    source: SourceId,
    context: &OakParseContext,
    calls: &[LiveCall],
) -> Vec<ResourceRef> {
    let mut resources = Vec::new();
    for call in calls {
        if call.site.callee != "system.file" || !is_base_call(context, &call.site) {
            continue;
        }
        let package = named_static_string(&call.raw.args, "package");
        let must_work = named_static_bool(&call.raw.args, "mustWork");
        let path_parts = call
            .raw
            .args
            .iter()
            .filter(|argument| argument.name.is_none())
            .map(|argument| match &argument.static_arg {
                Some(StaticArg::String(value)) => Some(value.as_str()),
                _ => None,
            })
            .collect::<Option<Vec<_>>>();
        let path = path_parts.map(|parts| {
            parts
                .into_iter()
                .filter(|part| !part.is_empty())
                .collect::<Vec<_>>()
                .join("/")
        });
        resources.push(ResourceRef {
            package,
            path,
            must_work,
            guards: call.site.guards.clone(),
            span: Span::new(source.clone(), call.site.span.start, call.site.span.end),
        });
    }
    resources
}

fn collect_active_bindings(
    source: SourceId,
    text: &str,
    context: &OakParseContext,
    index: &SemanticIndex,
    calls: &[LiveCall],
    if_regions: &[IfRegion],
) -> Vec<ActiveBindingDef> {
    let aliases = collect_environment_aliases(text, context, index, calls);
    let mut bindings = Vec::new();

    for call in calls {
        if call.site.callee != "makeActiveBinding" || !is_base_call(context, &call.site) {
            continue;
        }
        let Some(StaticArg::String(name)) = call.raw.args.first().and_then(|argument| argument.static_arg.as_ref()) else {
            continue;
        };
        let Some(target_argument) = call.raw.args.get(2) else {
            continue;
        };
        let target = environment_target(context, calls, target_argument, &aliases);
        let Some(target) = target else {
            continue;
        };
        let certain = !if_regions.iter().any(|region| {
            range_contains(region.then_start, region.then_end, call.site.span.start, call.site.span.end)
                || region.else_start.zip(region.else_end).is_some_and(|(start, end)| {
                    range_contains(start, end, call.site.span.start, call.site.span.end)
                })
        });
        bindings.push(ActiveBindingDef {
            name: name.clone(),
            target,
            certain,
            guards: call.site.guards.clone(),
            span: Span::new(source.clone(), call.site.span.start, call.site.span.end),
        });
    }
    bindings
}

fn collect_environment_aliases(
    text: &str,
    context: &OakParseContext,
    index: &SemanticIndex,
    calls: &[LiveCall],
) -> BTreeMap<String, StaticEnvironment> {
    let mut aliases = BTreeMap::new();
    for scope in index.scope_ids() {
        for (_definition_id, definition) in index.definitions(scope).iter() {
            if !matches!(definition.kind(), DefinitionKind::Assignment(_)) {
                continue;
            }
            let name = index.symbols(scope).symbol(definition.symbol()).name();
            let range = definition.range();
            let target_end = text_offset(range.end());
            let Some((value_start, value_end)) = assignment_rhs_after(text, target_end, "<-") else {
                continue;
            };
            let Some(environment_call) = calls.iter().find(|call| {
                call.site.callee == "environment"
                    && is_base_call(context, &call.site)
                    && range_contains(value_start, value_end, call.site.span.start, call.site.span.end)
            }) else {
                continue;
            };
            let Some(StaticArg::Symbol(binding)) = environment_call
                .raw
                .args
                .first()
                .and_then(|argument| argument.static_arg.as_ref())
            else {
                continue;
            };
            aliases.insert(name.to_owned(), StaticEnvironment::ClosureBinding(binding.clone()));
        }
    }
    aliases
}

fn environment_target(
    context: &OakParseContext,
    calls: &[LiveCall],
    argument: &RawArgument,
    aliases: &BTreeMap<String, StaticEnvironment>,
) -> Option<StaticEnvironment> {
    if let Some(StaticArg::Symbol(name)) = &argument.static_arg {
        if let Some(target) = aliases.get(name) {
            return Some(target.clone());
        }
    }

    let nested = calls.iter().find(|call| {
        range_contains(
            argument.value_start,
            argument.value_end,
            call.site.span.start,
            call.site.span.end,
        )
    })?;
    if nested.site.callee == "asNamespace" && is_base_call(context, &nested.site) {
        return nested
            .raw
            .args
            .first()
            .and_then(|argument| match argument.static_arg.as_ref() {
                Some(StaticArg::String(package)) => Some(StaticEnvironment::Namespace(package.clone())),
                _ => None,
            });
    }
    if nested.site.callee == "environment" && is_base_call(context, &nested.site) {
        return nested
            .raw
            .args
            .first()
            .and_then(|argument| match argument.static_arg.as_ref() {
                Some(StaticArg::Symbol(binding)) => Some(StaticEnvironment::ClosureBinding(binding.clone())),
                _ => None,
            });
    }

    None
}

fn collect_superassignments(
    source: SourceId,
    text: &str,
    index: &SemanticIndex,
) -> (Vec<SyntaxEffect>, Vec<(String, usize, usize)>) {
    let mut effects = Vec::new();
    let mut suppressed = Vec::new();

    for scope in index.scope_ids() {
        for (_definition_id, definition) in index.definitions(scope).iter() {
            if !matches!(definition.kind(), DefinitionKind::SuperAssignment(_)) {
                continue;
            }
            let target = index.symbols(scope).symbol(definition.symbol()).name().to_owned();
            let range = definition.range();
            let target_start = text_offset(range.start());
            let target_end = text_offset(range.end());
            let parts = superassignment_parts(text, target_start, target_end);
            let (span_start, span_end, value_symbol) = match parts {
                Some(parts) => {
                    if let Some((name, start, end)) = &parts.value_symbol {
                        suppressed.push((name.clone(), *start, *end));
                    }
                    (
                        parts.span_start,
                        parts.span_end,
                        parts.value_symbol.map(|(name, _, _)| name),
                    )
                }
                None => (target_start, target_end, None),
            };
            let target_enclosing_local = index
                .scope(scope)
                .parent()
                .is_some_and(|parent| index.resolve(&target, parent).is_some());
            effects.push(SyntaxEffect {
                kind: SyntaxEffectKind::SuperAssignment,
                target: Some(target),
                target_enclosing_local,
                value_symbol,
                phase: phase_for_scope(index, scope),
                guards: Vec::new(),
                span: Span::new(source.clone(), span_start, span_end),
            });
        }
    }
    (effects, suppressed)
}

fn translate_diagnostics(source: SourceId, index: &SemanticIndex) -> Vec<SemanticIssue> {
    index
        .diagnostics()
        .iter()
        .map(|diagnostic| match diagnostic {
            SemanticDiagnostic::AmbiguousEffect {
                name,
                call_range,
                reason,
            } => SemanticIssue {
                kind: SemanticIssueKind::AmbiguousEffect,
                message: format!("Oak could not prove one evaluation effect for `{name}`: {reason:?}"),
                span: Some(Span::new(
                    source.clone(),
                    text_offset(call_range.start()),
                    text_offset(call_range.end()),
                )),
            },
            SemanticDiagnostic::AmbiguousAttachOrder { packages, range } => SemanticIssue {
                kind: SemanticIssueKind::AmbiguousAttachOrder,
                message: format!(
                    "Oak found path-dependent search-path attachment order for {}",
                    packages.join(", ")
                ),
                span: Some(Span::new(
                    source.clone(),
                    text_offset(range.start()),
                    text_offset(range.end()),
                )),
            },
            SemanticDiagnostic::UninstalledPackage { package, range } => SemanticIssue {
                kind: SemanticIssueKind::UninstalledPackage,
                message: format!("Oak could not resolve attached package `{package}`"),
                span: Some(Span::new(
                    source.clone(),
                    text_offset(range.start()),
                    text_offset(range.end()),
                )),
            },
            SemanticDiagnostic::SourceCycle => SemanticIssue {
                kind: SemanticIssueKind::SourceCycle,
                message: "Oak detected a source() cycle while building semantic imports".into(),
                span: None,
            },
        })
        .collect()
}

fn apply_guard_regions_to_references(
    regions: &[(usize, usize, PackageGuard)],
    references: &mut [NameRef],
) {
    for (start, end, guard) in regions {
        for reference in references.iter_mut() {
            if range_contains(*start, *end, reference.span.start, reference.span.end) {
                push_guard(&mut reference.guards, guard.clone());
            }
        }
    }
}

fn apply_guard_regions_to_package_refs(
    regions: &[(usize, usize, PackageGuard)],
    references: &mut [PackageRef],
) {
    for (start, end, guard) in regions {
        for reference in references.iter_mut() {
            if range_contains(*start, *end, reference.span.start, reference.span.end) {
                push_guard(&mut reference.guards, guard.clone());
            }
        }
    }
}

fn apply_guard_regions_to_calls(
    regions: &[(usize, usize, PackageGuard)],
    calls: &mut [LiveCall],
) {
    for (start, end, guard) in regions {
        for call in calls.iter_mut() {
            if range_contains(*start, *end, call.site.span.start, call.site.span.end) {
                push_guard(&mut call.site.guards, guard.clone());
            }
        }
    }
}

fn apply_guard_regions_to_effects(
    regions: &[(usize, usize, PackageGuard)],
    effects: &mut [SyntaxEffect],
) {
    for (start, end, guard) in regions {
        for effect in effects.iter_mut() {
            if range_contains(*start, *end, effect.span.start, effect.span.end) {
                push_guard(&mut effect.guards, guard.clone());
            }
        }
    }
}

fn if_guard_regions(
    text: &str,
    context: &OakParseContext,
    regions: &[IfRegion],
    calls: &[LiveCall],
) -> Vec<(usize, usize, PackageGuard)> {
    let mut guards = Vec::new();
    for region in regions {
        if condition_contains_or(text, region.condition_start, region.condition_end) {
            continue;
        }
        for call in calls {
            if !range_contains(
                region.condition_start,
                region.condition_end,
                call.site.span.start,
                call.site.span.end,
            ) || !is_base_call(context, &call.site)
                || directly_negated(text, region.condition_start, call.site.span.start)
            {
                continue;
            }
            let guard = match call.site.callee.as_str() {
                "requireNamespace" => static_first_string(&call.site)
                    .map(|package| PackageGuard::Available(package.to_owned())),
                "isNamespaceLoaded" => static_first_string(&call.site)
                    .map(|package| PackageGuard::Loaded(package.to_owned())),
                _ => None,
            };
            if let Some(guard) = guard {
                guards.push((region.then_start, region.then_end, guard));
            }
        }
    }
    guards
}

fn hook_guard_regions(
    context: &OakParseContext,
    calls: &[LiveCall],
) -> Vec<(usize, usize, PackageGuard)> {
    let mut guards = Vec::new();
    for call in calls {
        if call.site.callee != "setHook" || !is_base_call(context, &call.site) {
            continue;
        }
        let Some(event_argument) = call.raw.args.first() else {
            continue;
        };
        let Some(callback_argument) = call.raw.args.get(1) else {
            continue;
        };
        let Some(event_call) = calls.iter().find(|nested| {
            nested.site.callee == "packageEvent"
                && is_base_call(context, &nested.site)
                && range_contains(
                    event_argument.value_start,
                    event_argument.value_end,
                    nested.site.span.start,
                    nested.site.span.end,
                )
        }) else {
            continue;
        };
        let Some(package) = static_first_string(&event_call.site) else {
            continue;
        };
        let event = event_call
            .site
            .args
            .get(1)
            .and_then(|argument| argument.as_ref())
            .and_then(|argument| match argument {
                StaticArg::String(value) => Some(value.as_str()),
                StaticArg::Symbol(_) => None,
            });
        if event == Some("onLoad") {
            guards.push((
                callback_argument.value_start,
                callback_argument.value_end,
                PackageGuard::Selected(package.to_owned()),
            ));
        }
    }
    guards
}

fn condition_contains_or(text: &str, start: usize, end: usize) -> bool {
    let bytes = text.as_bytes();
    let mut cursor = start;
    let mut quote = None;
    while cursor < end {
        let byte = bytes[cursor];
        if let Some(delimiter) = quote {
            if byte == b'\\' {
                cursor += 2;
                continue;
            }
            if byte == delimiter {
                quote = None;
            }
            cursor += 1;
            continue;
        }
        match byte {
            b'\'' | b'"' | b'`' => {
                quote = Some(byte);
                cursor += 1;
            }
            b'#' => cursor = skip_comment(text, cursor, end),
            b'|' => return true,
            _ => cursor += 1,
        }
    }
    false
}

fn directly_negated(text: &str, condition_start: usize, call_start: usize) -> bool {
    let bytes = text.as_bytes();
    let mut cursor = call_start;
    while cursor > condition_start && bytes[cursor - 1].is_ascii_whitespace() {
        cursor -= 1;
    }
    if cursor > condition_start && bytes[cursor - 1] == b'!' {
        return true;
    }
    false
}

fn push_guard(guards: &mut Vec<PackageGuard>, guard: PackageGuard) {
    if !guards.contains(&guard) {
        guards.push(guard);
    }
}

fn static_first_string(call: &CallSite) -> Option<&str> {
    match call.args.first()?.as_ref()? {
        StaticArg::String(value) => Some(value),
        StaticArg::Symbol(_) => None,
    }
}

fn named_static_string(arguments: &[RawArgument], name: &str) -> Option<String> {
    arguments.iter().find_map(|argument| {
        (argument.name.as_deref() == Some(name)).then(|| match &argument.static_arg {
            Some(StaticArg::String(value)) => Some(value.clone()),
            _ => None,
        })?
    })
}

fn named_static_bool(arguments: &[RawArgument], name: &str) -> Option<bool> {
    arguments.iter().find_map(|argument| {
        if argument.name.as_deref() != Some(name) {
            return None;
        }
        match argument.static_arg.as_ref() {
            Some(StaticArg::Symbol(value)) if value == "TRUE" || value == "T" => Some(true),
            Some(StaticArg::Symbol(value)) if value == "FALSE" || value == "F" => Some(false),
            _ => None,
        }
    })
}

fn range_contains(start: usize, end: usize, inner_start: usize, inner_end: usize) -> bool {
    start <= inner_start && inner_end <= end
}

fn assignment_rhs_after(text: &str, target_end: usize, operator: &str) -> Option<(usize, usize)> {
    let mut cursor = skip_trivia(text, target_end);
    if !text.get(cursor..)?.starts_with(operator) {
        return None;
    }
    cursor += operator.len();
    let value_start = skip_trivia(text, cursor);
    let value_end = expression_end(text, value_start);
    (value_start < value_end).then_some((value_start, value_end))
}

fn superassignment_parts(text: &str, target_start: usize, target_end: usize) -> Option<SuperAssignmentParts> {
    let mut cursor = skip_trivia(text, target_end);
    if text.get(cursor..)?.starts_with("<<-") {
        cursor += 3;
        let value_start = skip_trivia(text, cursor);
        let value_end = expression_end(text, value_start);
        return Some(SuperAssignmentParts {
            span_start: target_start,
            span_end: value_end,
            value_symbol: static_symbol_range(text, value_start, value_end),
        });
    }

    let statement_start = statement_start(text, target_start);
    let before_target = text.get(statement_start..target_start)?;
    let operator = before_target.rfind("->>")?;
    let value_start = skip_trivia(text, statement_start);
    let value_end = trim_end_offset(text, statement_start + operator);
    Some(SuperAssignmentParts {
        span_start: value_start,
        span_end: target_end,
        value_symbol: static_symbol_range(text, value_start, value_end),
    })
}

fn static_symbol_range(text: &str, start: usize, end: usize) -> Option<(String, usize, usize)> {
    let start = skip_trivia(text, start);
    let end = trim_end_offset(text, end);
    let value = text.get(start..end)?;
    match static_arg(value) {
        Some(StaticArg::Symbol(name)) => Some((name, start, end)),
        _ => None,
    }
}

fn call_after_name(text: &str, name_start: usize, name_end: usize) -> Option<RawCall> {
    let open = skip_trivia(text, name_end);
    if text.as_bytes().get(open).copied()? != b'(' {
        return None;
    }
    let close = matching_delimiter(text, open)?;
    Some(RawCall {
        start: name_start,
        end: close + 1,
        args: split_arguments(text, open + 1, close),
    })
}

fn split_arguments(text: &str, start: usize, end: usize) -> Vec<RawArgument> {
    let mut arguments = Vec::new();
    let mut segment_start = start;
    let mut cursor = start;
    let mut stack = Vec::new();
    let bytes = text.as_bytes();
    let mut quote = None;

    while cursor < end {
        let byte = bytes[cursor];
        if let Some(delimiter) = quote {
            if byte == b'\\' {
                cursor = (cursor + 2).min(end);
                continue;
            }
            if byte == delimiter {
                quote = None;
            }
            cursor += 1;
            continue;
        }
        match byte {
            b'\'' | b'"' | b'`' => {
                quote = Some(byte);
                cursor += 1;
            }
            b'#' => {
                cursor = skip_comment(text, cursor, end);
            }
            b'(' | b'[' | b'{' => {
                stack.push(byte);
                cursor += 1;
            }
            b')' | b']' | b'}' => {
                let _ = stack.pop();
                cursor += 1;
            }
            b',' if stack.is_empty() => {
                if let Some(argument) = raw_argument(text, segment_start, cursor) {
                    arguments.push(argument);
                } else {
                    arguments.push(RawArgument {
                        name: None,
                        value_start: cursor,
                        value_end: cursor,
                        static_arg: None,
                    });
                }
                segment_start = cursor + 1;
                cursor += 1;
            }
            _ => cursor += 1,
        }
    }

    if segment_start < end || !arguments.is_empty() {
        if let Some(argument) = raw_argument(text, segment_start, end) {
            arguments.push(argument);
        } else if segment_start < end {
            arguments.push(RawArgument {
                name: None,
                value_start: end,
                value_end: end,
                static_arg: None,
            });
        }
    }
    arguments
}

fn raw_argument(text: &str, start: usize, end: usize) -> Option<RawArgument> {
    let start = skip_trivia_bounded(text, start, end);
    let end = trim_end_offset_bounded(text, start, end);
    if start >= end {
        return None;
    }
    let (name, value_start) = named_argument_split(text, start, end)
        .map(|(name, value_start)| (Some(name), value_start))
        .unwrap_or((None, start));
    let value_start = skip_trivia_bounded(text, value_start, end);
    let value_end = trim_end_offset_bounded(text, value_start, end);
    let static_arg = text.get(value_start..value_end).and_then(static_arg);
    Some(RawArgument {
        name,
        value_start,
        value_end,
        static_arg,
    })
}

fn named_argument_split(text: &str, start: usize, end: usize) -> Option<(String, usize)> {
    let bytes = text.as_bytes();
    let mut cursor = start;
    let mut stack = Vec::new();
    let mut quote = None;
    while cursor < end {
        let byte = bytes[cursor];
        if let Some(delimiter) = quote {
            if byte == b'\\' {
                cursor = (cursor + 2).min(end);
                continue;
            }
            if byte == delimiter {
                quote = None;
            }
            cursor += 1;
            continue;
        }
        match byte {
            b'\'' | b'"' | b'`' => {
                quote = Some(byte);
                cursor += 1;
            }
            b'(' | b'[' | b'{' => {
                stack.push(byte);
                cursor += 1;
            }
            b')' | b']' | b'}' => {
                let _ = stack.pop();
                cursor += 1;
            }
            b'=' if stack.is_empty() => {
                let previous = cursor.checked_sub(1).and_then(|index| bytes.get(index)).copied();
                let next = bytes.get(cursor + 1).copied();
                if matches!(previous, Some(b'=' | b'!' | b'<' | b'>')) || next == Some(b'=') {
                    cursor += 1;
                    continue;
                }
                let lhs_start = skip_trivia_bounded(text, start, cursor);
                let lhs_end = trim_end_offset_bounded(text, lhs_start, cursor);
                let lhs = text.get(lhs_start..lhs_end)?;
                let name = static_symbol(lhs)?;
                return Some((name, cursor + 1));
            }
            _ => cursor += 1,
        }
    }
    None
}

fn static_arg(value: &str) -> Option<StaticArg> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    if let Some(string) = static_string(value) {
        return Some(StaticArg::String(string));
    }
    static_symbol(value).map(StaticArg::Symbol)
}

fn static_string(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    let quote = *bytes.first()?;
    if !matches!(quote, b'\'' | b'"') || bytes.last().copied()? != quote || bytes.len() < 2 {
        return None;
    }
    let mut output = String::new();
    let mut cursor = 1;
    while cursor + 1 < bytes.len() {
        let byte = bytes[cursor];
        if byte != b'\\' {
            let character = value.get(cursor..)?.chars().next()?;
            output.push(character);
            cursor += character.len_utf8();
            continue;
        }
        cursor += 1;
        let escaped = *bytes.get(cursor)?;
        match escaped {
            b'\\' => output.push('\\'),
            b'\'' => output.push('\''),
            b'"' => output.push('"'),
            b'n' => output.push('\n'),
            b'r' => output.push('\r'),
            b't' => output.push('\t'),
            b'b' => output.push('\u{0008}'),
            b'f' => output.push('\u{000c}'),
            b'a' => output.push('\u{0007}'),
            b'v' => output.push('\u{000b}'),
            _ => return None,
        }
        cursor += 1;
    }
    Some(output)
}

fn static_symbol(value: &str) -> Option<String> {
    let value = value.trim();
    if value.starts_with('`') && value.ends_with('`') && value.len() >= 2 {
        let inner = &value[1..value.len() - 1];
        if inner.contains('`') || inner.contains('\\') {
            return None;
        }
        return Some(inner.to_owned());
    }
    let mut chars = value.chars();
    let first = chars.next()?;
    if !(first.is_alphabetic() || first == '.' || first == '_') {
        return None;
    }
    if !chars.all(|character| character.is_alphanumeric() || character == '.' || character == '_') {
        return None;
    }
    Some(value.to_owned())
}

fn namespace_extent(text: &str, start: usize) -> Option<(usize, bool)> {
    let mut cursor = name_token_end(text, start)?;
    cursor = skip_trivia(text, cursor);
    let rest = text.get(cursor..)?;
    let (operator_len, internal) = if rest.starts_with(":::") {
        (3, true)
    } else if rest.starts_with("::") {
        (2, false)
    } else {
        return None;
    };
    cursor += operator_len;
    cursor = skip_trivia(text, cursor);
    let end = name_token_end(text, cursor)?;
    Some((end, internal))
}

fn name_token_end(text: &str, start: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    let first = *bytes.get(start)?;
    if first == b'`' {
        let mut cursor = start + 1;
        while cursor < bytes.len() {
            if bytes[cursor] == b'\\' {
                cursor += 2;
                continue;
            }
            if bytes[cursor] == b'`' {
                return Some(cursor + 1);
            }
            cursor += 1;
        }
        return None;
    }
    if first == b'\'' || first == b'"' {
        return quoted_end(text, start);
    }
    let mut cursor = start;
    while cursor < bytes.len() {
        let character = text.get(cursor..)?.chars().next()?;
        if !(character.is_alphanumeric() || character == '.' || character == '_') {
            break;
        }
        cursor += character.len_utf8();
    }
    (cursor > start).then_some(cursor)
}

fn quoted_end(text: &str, start: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    let quote = *bytes.get(start)?;
    let mut cursor = start + 1;
    while cursor < bytes.len() {
        if bytes[cursor] == b'\\' {
            cursor += 2;
            continue;
        }
        if bytes[cursor] == quote {
            return Some(cursor + 1);
        }
        cursor += 1;
    }
    None
}

fn matching_delimiter(text: &str, open: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    let opener = *bytes.get(open)?;
    let expected = match opener {
        b'(' => b')',
        b'[' => b']',
        b'{' => b'}',
        _ => return None,
    };
    let mut stack = vec![expected];
    let mut quote = None;
    let mut cursor = open + 1;

    while cursor < bytes.len() {
        let byte = bytes[cursor];
        if let Some(delimiter) = quote {
            if byte == b'\\' {
                cursor += 2;
                continue;
            }
            if byte == delimiter {
                quote = None;
            }
            cursor += 1;
            continue;
        }
        match byte {
            b'\'' | b'"' | b'`' => {
                quote = Some(byte);
                cursor += 1;
            }
            b'#' => cursor = skip_comment(text, cursor, bytes.len()),
            b'(' => {
                stack.push(b')');
                cursor += 1;
            }
            b'[' => {
                stack.push(b']');
                cursor += 1;
            }
            b'{' => {
                stack.push(b'}');
                cursor += 1;
            }
            b')' | b']' | b'}' => {
                if stack.pop()? != byte {
                    return None;
                }
                if stack.is_empty() {
                    return Some(cursor);
                }
                cursor += 1;
            }
            _ => cursor += 1,
        }
    }
    None
}

fn find_if_regions(text: &str) -> Vec<IfRegion> {
    let mut regions = Vec::new();
    let bytes = text.as_bytes();
    let mut cursor = 0;
    let mut quote = None;
    while cursor < bytes.len() {
        let byte = bytes[cursor];
        if let Some(delimiter) = quote {
            if byte == b'\\' {
                cursor += 2;
                continue;
            }
            if byte == delimiter {
                quote = None;
            }
            cursor += 1;
            continue;
        }
        match byte {
            b'\'' | b'"' | b'`' => {
                quote = Some(byte);
                cursor += 1;
            }
            b'#' => cursor = skip_comment(text, cursor, bytes.len()),
            b'i' if text.get(cursor..).is_some_and(|rest| rest.starts_with("if"))
                && word_boundary_before(text, cursor)
                && word_boundary_after(text, cursor + 2) =>
            {
                let open = skip_trivia(text, cursor + 2);
                if bytes.get(open).copied() != Some(b'(') {
                    cursor += 2;
                    continue;
                }
                let Some(close) = matching_delimiter(text, open) else {
                    cursor += 2;
                    continue;
                };
                let then_start = skip_trivia(text, close + 1);
                let then_end = expression_end(text, then_start);
                let after_then = skip_trivia(text, then_end);
                let (else_start, else_end) = if text
                    .get(after_then..)
                    .is_some_and(|rest| rest.starts_with("else"))
                    && word_boundary_after(text, after_then + 4)
                {
                    let start = skip_trivia(text, after_then + 4);
                    let end = expression_end(text, start);
                    (Some(start), Some(end))
                } else {
                    (None, None)
                };
                regions.push(IfRegion {
                    condition_start: open + 1,
                    condition_end: close,
                    then_start,
                    then_end,
                    else_start,
                    else_end,
                });
                cursor += 2;
            }
            _ => cursor += 1,
        }
    }
    regions
}

fn expression_end(text: &str, start: usize) -> usize {
    let start = skip_trivia(text, start);
    let bytes = text.as_bytes();
    if start >= bytes.len() {
        return start;
    }
    if matches!(bytes[start], b'(' | b'[' | b'{') {
        return matching_delimiter(text, start).map_or(bytes.len(), |close| close + 1);
    }
    if text.get(start..).is_some_and(|rest| rest.starts_with("if"))
        && word_boundary_after(text, start + 2)
    {
        let regions = find_if_regions(&text[start..]);
        if let Some(region) = regions.first() {
            return start + region.else_end.unwrap_or(region.then_end);
        }
    }

    let mut cursor = start;
    let mut stack = Vec::new();
    let mut quote = None;
    while cursor < bytes.len() {
        let byte = bytes[cursor];
        if let Some(delimiter) = quote {
            if byte == b'\\' {
                cursor += 2;
                continue;
            }
            if byte == delimiter {
                quote = None;
            }
            cursor += 1;
            continue;
        }
        match byte {
            b'\'' | b'"' | b'`' => {
                quote = Some(byte);
                cursor += 1;
            }
            b'#' if stack.is_empty() => return trim_end_offset(text, cursor),
            b'#' => cursor = skip_comment(text, cursor, bytes.len()),
            b'(' | b'[' | b'{' => {
                stack.push(byte);
                cursor += 1;
            }
            b')' | b']' | b'}' => {
                if stack.is_empty() {
                    return trim_end_offset(text, cursor);
                }
                let _ = stack.pop();
                cursor += 1;
            }
            b';' | b'\n' if stack.is_empty() => return trim_end_offset(text, cursor),
            _ => cursor += 1,
        }
    }
    trim_end_offset(text, cursor)
}

fn statement_start(text: &str, position: usize) -> usize {
    let bytes = text.as_bytes();
    let mut cursor = position;
    while cursor > 0 {
        let byte = bytes[cursor - 1];
        if matches!(byte, b';' | b'\n' | b'{' | b'}') {
            break;
        }
        cursor -= 1;
    }
    cursor
}

fn skip_trivia(text: &str, mut cursor: usize) -> usize {
    let bytes = text.as_bytes();
    loop {
        while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        if bytes.get(cursor).copied() != Some(b'#') {
            return cursor;
        }
        cursor = skip_comment(text, cursor, bytes.len());
    }
}

fn skip_trivia_bounded(text: &str, mut cursor: usize, end: usize) -> usize {
    let bytes = text.as_bytes();
    loop {
        while cursor < end && bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        if cursor >= end || bytes[cursor] != b'#' {
            return cursor;
        }
        cursor = skip_comment(text, cursor, end);
    }
}

fn skip_comment(text: &str, mut cursor: usize, end: usize) -> usize {
    let bytes = text.as_bytes();
    while cursor < end && bytes[cursor] != b'\n' {
        cursor += 1;
    }
    cursor
}

fn trim_end_offset(text: &str, end: usize) -> usize {
    trim_end_offset_bounded(text, 0, end)
}

fn trim_end_offset_bounded(text: &str, start: usize, mut end: usize) -> usize {
    let bytes = text.as_bytes();
    while end > start && bytes[end - 1].is_ascii_whitespace() {
        end -= 1;
    }
    end
}

fn word_boundary_before(text: &str, position: usize) -> bool {
    position == 0
        || text
            .get(..position)
            .and_then(|prefix| prefix.chars().next_back())
            .is_none_or(|character| !(character.is_alphanumeric() || character == '.' || character == '_'))
}

fn word_boundary_after(text: &str, position: usize) -> bool {
    text.get(position..)
        .and_then(|suffix| suffix.chars().next())
        .is_none_or(|character| !(character.is_alphanumeric() || character == '.' || character == '_'))
}

impl RParser for OakParser {
    fn parse(&self, source: SourceId, text: &str) -> Result<ParsedRFile> {
        self.parse_binding(source.clone(), text).map_err(|message| Error::Parse {
            path: format!("source:{}", source.0),
            message,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_source(text: &str) -> ParsedRFile {
        OakParser.parse_binding(SourceId(0), text).unwrap()
    }

    fn reference_names(parsed: &ParsedRFile) -> Vec<&str> {
        parsed.expressions[0]
            .references
            .iter()
            .map(|reference| reference.name.as_str())
            .collect()
    }

    #[test]
    fn quote_suppresses_ordinary_symbol_uses() {
        let parsed = parse_source("f <- function() quote(foo)");
        assert!(!reference_names(&parsed).contains(&"foo"));
    }

    #[test]
    fn bquote_analyzes_holes_only() {
        let parsed = parse_source("f <- function() bquote(foo + .(bar))");
        let names = reference_names(&parsed);
        assert!(!names.contains(&"foo"));
        assert!(names.contains(&"bar"));
    }

    #[test]
    fn shadowed_bquote_does_not_receive_base_effects() {
        let context = OakParseContext::new(BTreeSet::from(["bquote".to_owned()]));
        let parsed = OakParser
            .parse_binding_with_context(
                SourceId(0),
                "f <- function() bquote(foo + .(bar))",
                &context,
            )
            .unwrap();
        assert!(reference_names(&parsed).contains(&"foo"));
    }

    #[test]
    fn qualified_base_bquote_receives_base_effects() {
        let parsed = parse_source("f <- function() base::bquote(foo + .(bar))");
        let names = reference_names(&parsed);
        assert!(!names.contains(&"foo"));
        assert!(names.contains(&"bar"));
    }

    #[test]
    fn conditional_local_read_keeps_external_fallthrough_distinct() {
        let parsed = parse_source("f <- function(flag) { if (flag) x <- 1; x }");
        let reference = parsed.expressions[0]
            .references
            .iter()
            .find(|reference| reference.name == "x")
            .unwrap();
        assert_eq!(reference.kind, NameRefKind::ConditionalFallthrough);
    }

    #[test]
    fn local_parameter_never_becomes_external_reference() {
        let parsed = parse_source("f <- function(x) x");
        assert!(!reference_names(&parsed).contains(&"x"));
    }

    #[test]
    fn quoted_namespace_access_is_inert() {
        let parsed = parse_source("f <- function() quote(foo::bar)");
        assert!(parsed.expressions[0].package_refs.is_empty());
    }

    #[test]
    fn evaluated_namespace_access_is_recorded() {
        let parsed = parse_source("f <- function() foo::bar()");
        assert!(parsed.expressions[0]
            .package_refs
            .iter()
            .any(|reference| reference.package == "foo" && reference.symbol == "bar"));
    }

    #[test]
    fn quoted_system_file_is_inert() {
        let parsed = parse_source("f <- function() quote(system.file('data', package = 'foo'))");
        assert!(parsed.expressions[0].resource_refs.is_empty());
    }

    #[test]
    fn imported_bquote_does_not_receive_base_effects() {
        let mut context = OakParseContext::default();
        context.add_import_from("bquote", "fake", "bquote");
        let parsed = OakParser
            .parse_binding_with_context(
                SourceId(0),
                "f <- function() bquote(foo + .(bar))",
                &context,
            )
            .unwrap();
        assert!(reference_names(&parsed).contains(&"foo"));
    }

    #[test]
    fn substitute_quotes_expression_but_evaluates_environment_argument() {
        let parsed = parse_source(
            "f <- function() substitute(quoted_symbol + other_quoted, external_environment)",
        );
        let names = reference_names(&parsed);
        assert!(!names.contains(&"quoted_symbol"));
        assert!(!names.contains(&"other_quoted"));
        assert!(names.contains(&"external_environment"));
    }

    #[test]
    fn attached_search_path_does_not_claim_bare_effect_identity() {
        let context = OakParseContext::default();
        let mut resolver = SlinkerImportsResolver { context: &context };
        assert!(resolver
            .resolve_effects("quote", &["some_attached_package".to_owned()])
            .is_none());
    }

    #[test]
    fn definite_local_assignment_never_becomes_external_reference() {
        let parsed = parse_source("f <- function() { x <- 1; x }");
        assert!(!reference_names(&parsed).contains(&"x"));
    }

    #[test]
    fn duplicate_import_from_preserves_first_namespace_precedence() {
        let mut imports = NamespaceImports::default();
        imports.add_import_from("target", "first", "first_target");
        imports.add_import_from("target", "second", "second_target");

        assert_eq!(
            imports.resolve("target"),
            NamespaceImportResolution::Imported {
                package: "first".to_owned(),
                binding: "first_target".to_owned(),
                effect_name: "first_target".to_owned(),
            }
        );
    }

    #[test]
    fn import_all_resolution_stops_at_first_exporting_namespace() {
        let mut imports = NamespaceImports::default();
        imports.add_import_all(
            "first",
            Some(BTreeMap::from([("target".to_owned(), "target_impl".to_owned())])),
            Vec::<String>::new(),
        );
        imports.add_import_all("missing", None, Vec::<String>::new());

        assert_eq!(
            imports.resolve("target"),
            NamespaceImportResolution::Imported {
                package: "first".to_owned(),
                binding: "target_impl".to_owned(),
                effect_name: "target".to_owned(),
            }
        );
    }

    #[test]
    fn missing_import_all_blocks_only_after_resolution_reaches_it() {
        let mut imports = NamespaceImports::default();
        imports.add_import_all("missing", None, Vec::<String>::new());
        imports.add_import_all(
            "later",
            Some(BTreeMap::from([("target".to_owned(), "target".to_owned())])),
            Vec::<String>::new(),
        );

        assert_eq!(
            imports.resolve("target"),
            NamespaceImportResolution::MissingImportAll {
                package: "missing".to_owned(),
                binding: "target".to_owned(),
            }
        );
    }
}
