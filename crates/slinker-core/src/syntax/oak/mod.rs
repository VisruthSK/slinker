use crate::package::{Atom, BindingName};
use crate::profile::{self, Probe};
use crate::syntax::facts::{
    CallArgument, CallSite, CalleeKind, EvalPhase, LexicalBindingId, LexicalScopeId, NameRef,
    NameRefKind, ParsedExpression, ParsedRFile, PinnedDefault, ResourceArguments, ResourcePackage,
    ResourceRef, SemanticIssue, SemanticIssueKind, StaticArg, SyntaxEffect, SyntaxEffectKind,
};
use crate::syntax::source::{SourceId, Span, TextRange};
use air_r_parser::{RParserOptions, parse};
use air_r_syntax::{AnyRExpression, RCall, RRoot};
use biome_rowan::{AstNode, AstNodeList, AstSeparatedList};
use oak_semantic::build_index;
use oak_semantic::semantic_index::{
    DefinitionKind, ScopeId, ScopeKind, SemanticDiagnostic, SemanticIndex, UseId,
};
use std::collections::{BTreeSet, HashMap};

mod census;
use census::{
    Census, ForRegion, FunctionRegion, IfRegion, assignment_of, node_name, node_range,
    parameter_name, static_arg_of,
};
mod context;
mod declarations;
mod guards;
mod namespace;
mod proofs;
#[cfg(test)]
#[path = "../../../tests/unit/syntax/oak/parser.rs"]
mod tests;

pub use context::OakParseContext;
pub(crate) use context::SharedNames;
pub(crate) use context::{NamespaceImportResolution, NamespaceImports};

use context::SlinkerImportsResolver;
use declarations::{Declarations, collect_declarations, sole_positional_argument};
use guards::{
    apply_guard_regions_to_calls, apply_guard_regions_to_effects,
    apply_guard_regions_to_package_refs, apply_guard_regions_to_references, hook_guard_regions,
    if_guard_regions,
};
use namespace::{
    collect_active_bindings, collect_environment_aliases, collect_namespace_info_reads,
    collect_namespace_uses, namespace_access_facts,
};
use proofs::{
    ControlRegions, conditional_fallthrough_proven_bound, definition_must_execute_before_position,
    formal_default_use_is_bound, is_base_call, recursive_closure_binding_is_initialized,
};

#[derive(Debug, Default, Clone, Copy)]
pub struct OakParser;

#[derive(Debug, Clone)]
struct LiveUse {
    name: String,
    start: usize,
    end: usize,
    scope: ScopeId,
    use_id: UseId,
    callee_kind: CalleeKind,
    phase: EvalPhase,
}

#[derive(Debug, Clone)]
struct SuperAssignmentParts {
    span: TextRange,
    value_symbol: Option<(Atom, usize, usize)>,
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
        if u32::try_from(text.len()).is_err() {
            return Err("source exceeds the 4 GiB text size Air can address".into());
        }
        let parsed = profile::scoped(Probe::ParseSyntax, || {
            parse(text, RParserOptions::default())
        });
        if let Some(error) = parsed.error() {
            return Err(error.to_string());
        }
        let root = parsed.tree();
        let value_start = assignment_value_start(&root);
        let index = build_semantic_index(&root, context);
        Ok(with_value_start(
            translate_index(source, text, context, &root, &index)?,
            value_start,
        ))
    }
}

fn data_mask_ranges(context: &OakParseContext, census: &Census) -> Vec<std::ops::Range<usize>> {
    census
        .calls
        .iter()
        .filter(|call| {
            base_callee(call, context).is_some_and(|callee| {
                matches!(callee.as_str(), "with" | "within" | "subset" | "transform")
            })
        })
        .filter_map(|call| call.arguments().ok())
        .flat_map(|arguments| {
            arguments
                .items()
                .iter()
                .skip(1)
                .filter_map(|argument| argument.ok()?.value())
                .map(|value| {
                    let range = value.syntax().text_trimmed_range();
                    text_offset(range.start())..text_offset(range.end())
                })
                .collect::<Vec<_>>()
        })
        .collect()
}

fn used_parameters(index: &SemanticIndex) -> Vec<String> {
    let mut used = BTreeSet::new();
    for scope in index.scope_ids() {
        for (_, use_site) in index.uses(scope).iter() {
            let name = index.symbols(scope).symbol(use_site.symbol()).name();
            if let Some((owner, _, definition)) = index.resolve(name, scope)
                && matches!(definition.kind(), DefinitionKind::Parameter(_))
                && index
                    .scope(owner)
                    .parent()
                    .is_some_and(|file| index.scope(file).parent().is_none())
            {
                used.insert(name.to_owned());
            }
        }
    }
    used.into_iter().collect()
}

const FRAME_REBINDING_CALLEES: &[&str] = &[
    "assign",
    "delayedAssign",
    "makeActiveBinding",
    "list2env",
    "eval",
    "evalq",
    "local",
    "with",
    "within",
    "rm",
    "environment",
    "sys.frame",
    "sys.frames",
    "sys.function",
    "parent.frame",
    "as.environment",
    "pos.to.env",
    "environment<-",
];

fn pinned_defaults(
    root: &RRoot,
    index: &SemanticIndex,
    calls: &[CallSite],
    effects: &[SyntaxEffect],
) -> Vec<PinnedDefault> {
    let Some(function) = root
        .expressions()
        .iter()
        .find_map(|expression| outer_function(&expression))
    else {
        return Vec::new();
    };
    if calls
        .iter()
        .any(|call| FRAME_REBINDING_CALLEES.contains(&call.callee.as_str()))
    {
        return Vec::new();
    }
    let Ok(parameters) = function.parameters() else {
        return Vec::new();
    };
    parameters
        .items()
        .iter()
        .filter_map(std::result::Result::ok)
        .filter_map(|parameter| {
            let name = parameter_name(&parameter.name().ok()?)?;
            let default = parameter.default()?.value().ok()?;
            let Some(StaticArg::String(value)) = static_arg_of(&default) else {
                return None;
            };
            let rebound = effects.iter().any(|effect| {
                effect.kind == SyntaxEffectKind::SuperAssignment
                    && effect.target.as_deref() == Some(name.as_str())
            });
            (!rebound && definition_count(index, &name) == 1).then_some(PinnedDefault {
                name: Atom::from(name),
                value,
            })
        })
        .collect()
}

fn definition_count(index: &SemanticIndex, name: &str) -> usize {
    index
        .scope_ids()
        .map(|scope| {
            index
                .definitions(scope)
                .iter()
                .filter(|(_, definition)| {
                    index.symbols(scope).symbol(definition.symbol()).name() == name
                })
                .count()
        })
        .sum()
}

fn is_dots_element(name: &str) -> bool {
    name.strip_prefix("..")
        .is_some_and(|index| !index.is_empty() && index.bytes().all(|byte| byte.is_ascii_digit()))
}

fn identifier_callee(call: &RCall) -> Option<String> {
    let AnyRExpression::RIdentifier(identifier) = call.function().ok()? else {
        return None;
    };
    census::identifier_name(&identifier)
}

fn base_callee(call: &RCall, context: &OakParseContext) -> Option<String> {
    identifier_callee(call).filter(|callee| context.resolves_to_base(callee.as_str()))
}

struct LexicalScopes {
    ids: HashMap<ScopeId, LexicalScopeId>,
    parents: Vec<Option<LexicalScopeId>>,
}

impl LexicalScopes {
    fn new(index: &SemanticIndex) -> Self {
        let ids = index
            .scope_ids()
            .enumerate()
            .map(|(position, scope)| {
                let position =
                    u32::try_from(position).expect("Air scopes are bounded by its u32 text size");
                (scope, LexicalScopeId(position))
            })
            .collect::<HashMap<_, _>>();
        let parents = index
            .scope_ids()
            .map(|scope| index.scope(scope).parent().map(|parent| ids[&parent]))
            .collect();
        Self { ids, parents }
    }

    fn at(&self, index: &SemanticIndex, offset: usize) -> (ScopeId, LexicalScopeId) {
        let offset = u32::try_from(offset).expect("offsets lie within text Air accepted");
        let (scope, _) = index.scope_at(offset.into());
        (scope, self.ids[&scope])
    }

    fn binding(
        &self,
        index: &SemanticIndex,
        name: &str,
        scope: ScopeId,
    ) -> Option<LexicalBindingId> {
        index
            .resolve(name, scope)
            .map(|(owner, _, _)| LexicalBindingId {
                defining_scope: self.ids[&owner],
                name: Atom::from(name),
            })
    }

    fn argument_binding(
        &self,
        index: &SemanticIndex,
        scope: ScopeId,
        value: Option<&StaticArg>,
    ) -> Option<LexicalBindingId> {
        match value {
            Some(StaticArg::Symbol(name)) => self.binding(index, name, scope),
            Some(StaticArg::String(_) | StaticArg::Logical(_)) | None => None,
        }
    }
}

fn with_value_start(mut parsed: ParsedRFile, value_start: Option<usize>) -> ParsedRFile {
    if let Some(expression) = parsed.expressions.first_mut() {
        expression.assigned_value_start = value_start;
    }
    parsed
}

fn assignment_value_start(root: &RRoot) -> Option<usize> {
    let assignment = root
        .expressions()
        .iter()
        .find_map(|expression| match expression {
            AnyRExpression::RBinaryExpression(binary) => Some(binary),
            _ => None,
        })?;
    let operator = assignment.operator().ok()?;
    (operator.text_trimmed() == "<-").then_some(())?;
    Some(text_offset(
        assignment
            .right()
            .ok()?
            .syntax()
            .text_trimmed_range()
            .start(),
    ))
}

fn build_semantic_index(root: &RRoot, context: &OakParseContext) -> SemanticIndex {
    profile::scoped(Probe::ParseIndex, || {
        build_index(root, SlinkerImportsResolver { context })
    })
}

fn translate_index(
    source: SourceId,
    text: &str,
    context: &OakParseContext,
    root: &RRoot,
    index: &SemanticIndex,
) -> Result<ParsedRFile, String> {
    let scopes = LexicalScopes::new(index);
    let census = profile::scoped(Probe::ParseRegions, || Census::of(root))?;
    let declarations = profile::scoped(Probe::ParseDeclarations, || {
        collect_declarations(&source, &census, context, index, &scopes)
    });
    let mut live_uses = collect_live_uses(index, &declarations);

    let (function_regions, for_regions, if_regions) =
        (&census.functions, &census.fors, &census.ifs);
    profile::scoped(Probe::ParseRegions, || {
        refine_callee_kinds(root, context, index, &census, &mut live_uses);
    });

    let mut references = profile::scoped(Probe::ParseReferences, || {
        name_references(&source, context, root, index, &census, &live_uses)
    });
    let calls_span = profile::span(Probe::ParseCalls);
    let mut live_calls = lexical_calls(&source, root, index, &scopes, &census, &live_uses);
    let translation = Translation {
        source: &source,
        root,
        index,
        scopes: &scopes,
        declarations: &declarations,
        census: &census,
    };
    let mut package_refs = namespace_access_facts(translation, &live_uses, &mut live_calls)?;
    binary_operator_facts(translation, context, &mut references, &mut live_calls);
    dispatching_syntax_facts(translation, context, &mut references, &mut live_calls);

    deduplicate_calls(&mut live_calls);
    drop(calls_span);

    let guards_span = profile::span(Probe::ParseGuards);
    let mut guard_regions = if_guard_regions(context, index, &census, &live_calls);
    apply_guard_regions_to_references(&guard_regions, &mut references);
    apply_guard_regions_to_package_refs(&guard_regions, &mut package_refs);
    apply_guard_regions_to_calls(&guard_regions, &mut live_calls);

    let hook_regions = hook_guard_regions(context, &live_calls);
    apply_guard_regions_to_references(&hook_regions, &mut references);
    apply_guard_regions_to_package_refs(&hook_regions, &mut package_refs);
    apply_guard_regions_to_calls(&hook_regions, &mut live_calls);
    guard_regions.extend(hook_regions);

    let resource_refs = collect_resources(source, context, &live_calls);
    let environment_aliases = collect_environment_aliases(root, context, index, &live_calls);
    let active_bindings = collect_active_bindings(
        source,
        context,
        &live_calls,
        &environment_aliases,
        if_regions,
    );
    let namespace_uses = collect_namespace_uses(
        source,
        &census,
        root,
        index,
        &scopes,
        &live_calls,
        &declarations,
    );
    drop(guards_span);

    let effects_span = profile::span(Probe::ParseEffects);
    let (mut effects, suppressed_reference_spans) = collect_superassignments(
        source,
        root,
        index,
        function_regions,
        for_regions,
        if_regions,
    );
    suppress_superassignment_references(&mut effects, &mut references, &suppressed_reference_spans);
    apply_guard_regions_to_effects(&guard_regions, &mut effects);
    drop(effects_span);

    let parameters = root
        .expressions()
        .iter()
        .find_map(|expression| outer_function(&expression))
        .and_then(|function| function.parameters().ok())
        .map(|parameters| {
            parameters
                .items()
                .iter()
                .filter_map(|parameter| parameter.ok()?.name().ok())
                .filter_map(|name| parameter_name(&name))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let namespace_info_reads = collect_namespace_info_reads(source, &census, &declarations);
    let calls: Vec<CallSite> = live_calls;
    let mut issues = translate_diagnostics(source, index);
    issues.extend(declarations.issues);
    for expression in &census.expressions {
        let name = match &expression {
            AnyRExpression::RIfStatement(_) => "if",
            AnyRExpression::RReturnExpression(_) => "return",
            _ => continue,
        };
        let (scope, _) = index.scope_at(expression.syntax().text_trimmed_range().start());
        if !context.resolves_to_base(name) || index.resolve(name, scope).is_some() {
            issues.push(SemanticIssue {
                kind: SemanticIssueKind::UnsupportedControl,
                message: format!(
                    "shadowed `{name}` control form has no established callee/evaluation contract"
                ),
                span: Some(ast_span(&source, expression)),
            });
        }
    }

    let used_parameters = used_parameters(index);
    let pinned_defaults = pinned_defaults(root, index, &calls, &effects);
    Ok(ParsedRFile {
        expressions: vec![ParsedExpression {
            span: Span::new(source, 0, text.len()),
            assigned_value_start: None,
            parameters: parameters.into_iter().map(Atom::from).collect(),
            used_parameters: used_parameters.into_iter().map(Atom::from).collect(),
            pinned_defaults,
            references: exact(references),
            package_refs: exact(package_refs),
            resource_refs: exact(resource_refs),
            calls: exact(calls),
            active_bindings,
            effects: exact(effects),
            namespace_info_reads,
            namespace_uses,
        }],
        issues,
        scope_parents: exact(scopes.parents),
        declarations: exact(declarations.declarations),
    })
}

fn exact<T>(mut values: Vec<T>) -> Vec<T> {
    values.shrink_to_fit();
    values
}

fn collect_live_uses(index: &SemanticIndex, declarations: &Declarations) -> Vec<LiveUse> {
    let mut live_uses = Vec::new();

    for scope in index.scope_ids() {
        let phase = phase_for_scope(index, scope);
        for (use_id, use_site) in index.uses(scope).iter() {
            if declarations.is_inert(text_offset(use_site.range().start())) {
                continue;
            }
            let symbol = index.symbols(scope).symbol(use_site.symbol());
            let name = symbol.name().to_owned();
            if is_dots_element(&name) {
                continue;
            }
            let range = use_site.range();
            let start = text_offset(range.start());
            let end = text_offset(range.end());
            let bound = index.use_is_bound(scope, use_id);
            let has_reaching_definitions =
                index.reaching_definitions(scope, use_id).next().is_some();
            let callee_kind = if bound {
                CalleeKind::DefinitelyLexical
            } else if has_reaching_definitions {
                CalleeKind::ConditionalFallthrough
            } else {
                CalleeKind::DefinitelyExternal
            };

            live_uses.push(LiveUse {
                name,
                start,
                end,
                scope,
                use_id,
                callee_kind,
                phase,
            });
        }
    }
    live_uses
}

fn refine_callee_kinds(
    root: &RRoot,
    context: &OakParseContext,
    index: &SemanticIndex,
    census: &Census,
    live_uses: &mut [LiveUse],
) {
    let function_regions = &census.functions;
    for live_use in live_uses.iter_mut() {
        if live_use.callee_kind != CalleeKind::DefinitelyLexical
            && (formal_default_use_is_bound(function_regions, live_use)
                || recursive_closure_binding_is_initialized(
                    root,
                    index,
                    function_regions,
                    live_use,
                ))
        {
            live_use.callee_kind = CalleeKind::DefinitelyLexical;
            continue;
        }
        if live_use.callee_kind == CalleeKind::ConditionalFallthrough
            && conditional_fallthrough_proven_bound(context, index, census, live_use)
        {
            live_use.callee_kind = CalleeKind::DefinitelyLexical;
        }
    }
}

fn name_references(
    source: &SourceId,
    context: &OakParseContext,
    root: &RRoot,
    index: &SemanticIndex,
    census: &Census,
    live_uses: &[LiveUse],
) -> Vec<NameRef> {
    let data_masks = data_mask_ranges(context, census);
    live_uses
        .iter()
        .filter_map(|live_use| {
            let masked = data_masks.iter().any(|mask| mask.contains(&live_use.start));
            let kind = match live_use.callee_kind {
                CalleeKind::DefinitelyLexical
                    if census.is_callee(live_use.start, live_use.end)
                        && !reaches_only_closures(root, index, live_use) =>
                {
                    NameRefKind::MaybeLocal
                }
                CalleeKind::DefinitelyLexical => return None,
                CalleeKind::DefinitelyExternal | CalleeKind::ConditionalFallthrough if masked => {
                    NameRefKind::MaybeLocal
                }
                CalleeKind::DefinitelyExternal => NameRefKind::External,
                CalleeKind::ConditionalFallthrough => NameRefKind::ConditionalFallthrough,
            };
            let replaced = census.is_replacement_target(live_use.start, live_use.end);
            Some(NameRef {
                name: if replaced {
                    BindingName::from(format!("{}<-", live_use.name))
                } else {
                    BindingName::from(live_use.name.as_str())
                },
                kind,
                phase: live_use.phase,
                guards: Vec::new(),
                span: Span::new(*source, live_use.start, live_use.end),
            })
        })
        .collect()
}

fn lexical_calls(
    source: &SourceId,
    root: &RRoot,
    index: &SemanticIndex,
    scopes: &LexicalScopes,
    census: &Census,
    live_uses: &[LiveUse],
) -> Vec<CallSite> {
    let mut live_calls = Vec::new();

    for live_use in live_uses {
        let Some(raw) = census.call_of(live_use.start, live_use.end) else {
            continue;
        };
        let Some((scope, arguments)) = call_arguments(source, root, index, live_uses, scopes, raw)
        else {
            continue;
        };
        live_calls.push(CallSite {
            callee: Atom::from(live_use.name.as_str()),
            callee_kind: live_use.callee_kind,
            qualification: None,
            arguments,
            scope,
            phase: live_use.phase,
            guards: Vec::new(),
            span: Span::new(*source, node_range(raw).start, node_range(raw).end),
        });
    }
    live_calls
}

#[derive(Clone, Copy)]
struct Translation<'a> {
    source: &'a SourceId,
    root: &'a RRoot,
    index: &'a SemanticIndex,
    scopes: &'a LexicalScopes,
    declarations: &'a Declarations,
    census: &'a Census,
}

fn binary_operator_facts(
    translation: Translation<'_>,
    context: &OakParseContext,
    references: &mut Vec<NameRef>,
    live_calls: &mut Vec<CallSite>,
) {
    let Translation {
        source,
        census,
        index,
        scopes,
        declarations,
        ..
    } = translation;
    for expression in &census.expressions {
        let AnyRExpression::RBinaryExpression(binary) = expression else {
            continue;
        };
        let Ok(operator_token) = binary.operator() else {
            continue;
        };
        let operator = operator_token.text_trimmed().to_owned();
        if matches!(operator.as_str(), "<-" | "=" | "<<-" | "->" | "->>") {
            continue;
        }
        let (Ok(left), Ok(right)) = (binary.left(), binary.right()) else {
            continue;
        };
        let span = ast_span(source, binary);
        if declarations.is_inert(span.start) {
            continue;
        }
        let (scope, _) = index.scope_at(binary.range().start());
        let callee_start = text_offset(operator_token.text_trimmed_range().start());
        let callee_kind = if index.resolve(&operator, scope).is_some() {
            CalleeKind::ConditionalFallthrough
        } else {
            CalleeKind::DefinitelyExternal
        };
        if operator.starts_with('%')
            || !context.resolves_to_base(&operator)
            || callee_kind != CalleeKind::DefinitelyExternal
        {
            let range = operator_token.text_trimmed_range();
            references.push(NameRef {
                name: BindingName::from(operator.as_str()),
                kind: if callee_kind == CalleeKind::DefinitelyExternal {
                    NameRefKind::External
                } else {
                    NameRefKind::ConditionalFallthrough
                },
                phase: phase_for_scope(index, scope),
                guards: Vec::new(),
                span: Span::new(*source, callee_start, text_offset(range.end())),
            });
        }
        let (argument_scope, lexical_scope) = scopes.at(index, span.start);
        live_calls.push(CallSite {
            callee: Atom::from(operator.as_str()),
            callee_kind,
            qualification: None,
            arguments: [left, right]
                .map(|expression| {
                    let value = static_arg_of(&expression);
                    CallArgument {
                        binding: scopes.argument_binding(index, argument_scope, value.as_ref()),
                        value,
                        name: None,
                        span: Some(ast_span(source, &expression)),
                        is_local_closure: false,
                    }
                })
                .into(),
            scope: lexical_scope,
            phase: phase_for_scope(index, scope),
            guards: Vec::new(),
            span: span.clone(),
        });
    }
}

fn suppress_superassignment_references(
    effects: &mut [SyntaxEffect],
    references: &mut Vec<NameRef>,
    suppressed_reference_spans: &[(Atom, usize, usize)],
) {
    let is_suppressed = |reference: &NameRef, (name, start, end): &(Atom, usize, usize)| {
        reference.name == *name && reference.span.start == *start && reference.span.end == *end
    };
    for effect in effects.iter_mut() {
        let span = &effect.span;
        let free = |symbol: &Atom| {
            suppressed_reference_spans.iter().any(|value| {
                value.0 == *symbol
                    && span.start <= value.1
                    && value.2 <= span.end
                    && references
                        .iter()
                        .any(|reference| is_suppressed(reference, value))
            })
        };
        if !effect.value_symbol.as_ref().is_some_and(free) {
            effect.value_symbol = None;
        }
    }
    references.retain(|reference| {
        !suppressed_reference_spans
            .iter()
            .any(|value| is_suppressed(reference, value))
    });
}

fn ast_span(source: &SourceId, node: &impl AstNode<Language = air_r_syntax::RLanguage>) -> Span {
    let range = node.syntax().text_trimmed_range();
    Span::new(
        *source,
        text_offset(range.start()),
        text_offset(range.end()),
    )
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

fn deduplicate_calls(calls: &mut Vec<CallSite>) {
    calls.sort_by(|left, right| {
        (
            left.span.start,
            left.span.end,
            left.qualified_package(),
            left.callee.as_str(),
        )
            .cmp(&(
                right.span.start,
                right.span.end,
                right.qualified_package(),
                right.callee.as_str(),
            ))
    });
    calls.dedup_by(|left, right| {
        left.span.start == right.span.start
            && left.span.end == right.span.end
            && left.callee == right.callee
            && left.qualification == right.qualification
    });
}

fn collect_resources(
    source: SourceId,
    context: &OakParseContext,
    calls: &[CallSite],
) -> Vec<ResourceRef> {
    let mut resources = Vec::new();
    for call in calls {
        if call.callee != "system.file" || !is_base_call(context, call) {
            continue;
        }
        if !call
            .arguments
            .iter()
            .any(|argument| argument.name.as_deref() == Some("package"))
        {
            continue;
        }
        let package = named_static_string(&call.arguments, "package").map_or_else(
            || {
                ResourcePackage::Computed(
                    call.arguments
                        .iter()
                        .find(|argument| argument.name.as_deref() == Some("package"))
                        .and_then(|argument| argument.binding.clone()),
                )
            },
            ResourcePackage::Literal,
        );
        resources.push(ResourceRef {
            package,
            arguments: resource_arguments(&call.arguments)
                .unwrap_or(ResourceArguments::Unsupported),
            guards: call.guards.clone(),
            scope: call.scope,
            span: Span::new(source, call.span.start, call.span.end),
        });
    }
    resources
}

fn collect_superassignments(
    source: SourceId,
    root: &RRoot,
    index: &SemanticIndex,
    function_regions: &[FunctionRegion],
    for_regions: &[ForRegion],
    if_regions: &[IfRegion],
) -> (Vec<SyntaxEffect>, Vec<(Atom, usize, usize)>) {
    let mut effects = Vec::new();
    let mut suppressed = Vec::new();

    for scope in index.scope_ids() {
        for (_definition_id, definition) in index.definitions(scope).iter() {
            if !matches!(definition.kind(), DefinitionKind::SuperAssignment(_)) {
                continue;
            }
            let target = index
                .symbols(scope)
                .symbol(definition.symbol())
                .name()
                .to_owned();
            let range = definition.range();
            let target_start = text_offset(range.start());
            let target_end = text_offset(range.end());
            let parts = superassignment_parts(root, definition.kind(), target_start, target_end);
            let (span_start, span_end, value_symbol) = match parts {
                Some(parts) => {
                    if let Some((name, start, end)) = &parts.value_symbol {
                        suppressed.push((name.clone(), *start, *end));
                    }
                    (
                        parts.span.start,
                        parts.span.end,
                        parts.value_symbol.map(|(name, _, _)| name),
                    )
                }
                None => (target_start, target_end, None),
            };
            let target_enclosing_local = superassignment_targets_captured_activation(
                index,
                function_regions,
                ControlRegions {
                    for_regions,
                    if_regions,
                },
                scope,
                target_start,
                &target,
            );
            effects.push(SyntaxEffect {
                kind: SyntaxEffectKind::SuperAssignment,
                target: Some(Atom::from(target)),
                target_enclosing_local,
                value_symbol,
                phase: phase_for_scope(index, scope),
                guards: Vec::new(),
                span: Span::new(source, span_start, span_end),
            });
        }
    }
    (effects, suppressed)
}

fn superassignment_targets_captured_activation(
    index: &SemanticIndex,
    function_regions: &[FunctionRegion],
    regions: ControlRegions<'_>,
    scope: ScopeId,
    target_start: usize,
    target: &str,
) -> bool {
    if index.scope(scope).kind() != ScopeKind::Function {
        return false;
    }

    let Some(current_function) = innermost_function_region(function_regions, target_start) else {
        return false;
    };

    let mut ancestors = function_regions
        .iter()
        .filter(|region| {
            region.function_start != current_function.function_start
                && region.body.start <= current_function.function_start
                && current_function.function_start < region.body.end
        })
        .collect::<Vec<_>>();
    ancestors.sort_by_key(|region| region.body.end.saturating_sub(region.body.start));

    for ancestor in ancestors {
        if ancestor.parameters.contains(target) {
            return true;
        }

        let child_start = direct_child_function_start(function_regions, ancestor, current_function)
            .unwrap_or(current_function.function_start);

        for definition_scope in index.scope_ids() {
            if index.scope(definition_scope).kind() != ScopeKind::Function {
                continue;
            }
            for (_definition_id, definition) in index.definitions(definition_scope).iter() {
                let symbol = index.symbols(definition_scope).symbol(definition.symbol());
                if symbol.name() != target {
                    continue;
                }

                let definition_start = text_offset(definition.range().start());
                if definition_start < ancestor.body.start || definition_start >= ancestor.body.end {
                    continue;
                }
                if innermost_function_region(function_regions, definition_start)
                    .is_some_and(|owner| owner.function_start != ancestor.function_start)
                {
                    continue;
                }

                match definition.kind() {
                    DefinitionKind::Parameter(_) => return true,
                    DefinitionKind::Assignment(_) | DefinitionKind::Assign { .. } => {
                        if definition_must_execute_before_position(
                            definition_start,
                            child_start,
                            regions.for_regions,
                            regions.if_regions,
                        ) {
                            return true;
                        }
                    }
                    DefinitionKind::ForVariable(_) => {
                        if regions.for_regions.iter().any(|region| {
                            region.variable == target
                                && region.variable_start >= ancestor.body.start
                                && region.variable_start < ancestor.body.end
                                && child_start >= region.body.start
                                && child_start < region.body.end
                        }) {
                            return true;
                        }
                    }
                    DefinitionKind::SuperAssignment(_) | DefinitionKind::Import { .. } => {}
                }
            }
        }
    }

    false
}

fn innermost_function_region(
    regions: &[FunctionRegion],
    position: usize,
) -> Option<&FunctionRegion> {
    regions
        .iter()
        .filter(|region| position >= region.body.start && position < region.body.end)
        .min_by_key(|region| region.body.end.saturating_sub(region.body.start))
}

fn direct_child_function_start(
    regions: &[FunctionRegion],
    ancestor: &FunctionRegion,
    descendant: &FunctionRegion,
) -> Option<usize> {
    regions
        .iter()
        .filter(|candidate| {
            candidate.function_start != ancestor.function_start
                && candidate.function_start >= ancestor.body.start
                && candidate.body.end <= ancestor.body.end
                && candidate.body.start <= descendant.function_start
                && descendant.function_start < candidate.body.end
        })
        .max_by_key(|candidate| candidate.body.end.saturating_sub(candidate.body.start))
        .map(|candidate| candidate.function_start)
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
                message: format!(
                    "Oak could not prove one evaluation effect for `{name}`: {reason:?}"
                ),
                span: Some(Span::new(
                    source,
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
                    source,
                    text_offset(range.start()),
                    text_offset(range.end()),
                )),
            },
            SemanticDiagnostic::UninstalledPackage { package, range } => SemanticIssue {
                kind: SemanticIssueKind::UninstalledPackage,
                message: format!("Oak could not resolve attached package `{package}`"),
                span: Some(Span::new(
                    source,
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

fn static_first_string(call: &CallSite) -> Option<&str> {
    match call.static_arg(0)? {
        StaticArg::String(value) => Some(value),
        StaticArg::Symbol(_) | StaticArg::Logical(_) => None,
    }
}

fn named_static_string(arguments: &[CallArgument], name: &str) -> Option<String> {
    arguments.iter().find_map(|argument| {
        (argument.name.as_deref() == Some(name)).then(|| match &argument.value {
            Some(StaticArg::String(value)) => Some(value.as_str().to_owned()),
            _ => None,
        })?
    })
}

fn resource_arguments(arguments: &[CallArgument]) -> Option<ResourceArguments> {
    let mut parts = Vec::new();
    let mut package_seen = false;
    let mut must_work = None;
    for argument in arguments {
        match argument.name.as_deref() {
            None => match &argument.value {
                Some(StaticArg::String(value)) => {
                    if !value.is_empty() {
                        parts.push(value.as_str());
                    }
                }
                _ => return None,
            },
            Some("package") if !package_seen => package_seen = true,
            Some("mustWork") if must_work.is_none() => {
                let Some(StaticArg::Logical(value)) = argument.value else {
                    return None;
                };
                must_work = Some(value);
            }
            _ => return None,
        }
    }
    Some(ResourceArguments::Static {
        path: parts.join("/"),
        must_work: must_work.unwrap_or(false),
    })
}

fn call_arguments(
    source: &SourceId,
    root: &RRoot,
    index: &SemanticIndex,
    live_uses: &[LiveUse],
    scopes: &LexicalScopes,
    call: &RCall,
) -> Option<(LexicalScopeId, Box<[CallArgument]>)> {
    let (scope, lexical) = scopes.at(index, node_range(call).start);
    let mut arguments = call
        .arguments()
        .ok()?
        .items()
        .iter()
        .map(|argument| {
            let argument = argument.ok()?;
            let value = argument.value();
            let static_value = value.as_ref().and_then(static_arg_of);
            let mut argument = CallArgument {
                binding: scopes.argument_binding(index, scope, static_value.as_ref()),
                value: static_value,
                name: argument
                    .name_clause()
                    .and_then(|clause| clause.name().ok())
                    .and_then(|name| census::argument_name(&name))
                    .map(Atom::from),
                span: value.as_ref().map(|value| ast_span(source, value)),
                is_local_closure: false,
            };
            argument.is_local_closure = local_closure_argument(root, index, live_uses, &argument);
            Some(argument)
        })
        .collect::<Option<Vec<_>>>()?;
    if arguments
        .last()
        .is_some_and(|argument| argument.name.is_none() && argument.span.is_none())
    {
        arguments.pop();
    }
    Some((lexical, arguments.into()))
}

fn local_closure_argument(
    root: &RRoot,
    index: &SemanticIndex,
    live_uses: &[LiveUse],
    argument: &CallArgument,
) -> bool {
    let Some(StaticArg::Symbol(name)) = &argument.value else {
        return false;
    };
    let Some(use_site) = live_uses.iter().find(|live_use| {
        live_use.name == *name
            && argument
                .span
                .as_ref()
                .is_some_and(|span| live_use.start == span.start && live_use.end == span.end)
            && live_use.callee_kind == CalleeKind::DefinitelyLexical
    }) else {
        return false;
    };
    index
        .reaching_definitions(use_site.scope, use_site.use_id)
        .any(|(scope, definition_id)| definition_is_closure(root, index, scope, definition_id))
}

fn reaches_only_closures(root: &RRoot, index: &SemanticIndex, live_use: &LiveUse) -> bool {
    let mut definitions = index
        .reaching_definitions(live_use.scope, live_use.use_id)
        .peekable();
    definitions.peek().is_some()
        && definitions
            .all(|(scope, definition_id)| definition_is_closure(root, index, scope, definition_id))
}

fn definition_is_closure(
    root: &RRoot,
    index: &SemanticIndex,
    scope: ScopeId,
    definition_id: oak_semantic::semantic_index::DefinitionId,
) -> bool {
    let definition = &index.definitions(scope)[definition_id];
    matches!(definition.kind(), DefinitionKind::Assignment(_))
        && assignment_of(root, definition.kind()).is_some_and(|assignment| {
            matches!(assignment.value, AnyRExpression::RFunctionDefinition(_))
        })
}

fn dispatching_syntax_facts(
    translation: Translation<'_>,
    context: &OakParseContext,
    references: &mut Vec<NameRef>,
    live_calls: &mut Vec<CallSite>,
) {
    let Translation {
        source,
        census,
        index,
        scopes,
        declarations,
        ..
    } = translation;
    let mut record = |callee: String, node: &AnyRExpression| {
        let span = ast_span(source, node);
        if declarations.is_inert(span.start) {
            return;
        }
        let (scope, _) = index.scope_at(node.syntax().text_trimmed_range().start());
        let (_, lexical_scope) = scopes.at(index, span.start);
        let callee_kind = if index.resolve(&callee, scope).is_some() {
            CalleeKind::ConditionalFallthrough
        } else {
            CalleeKind::DefinitelyExternal
        };
        if !context.resolves_to_base(&callee) || callee_kind != CalleeKind::DefinitelyExternal {
            references.push(NameRef {
                name: BindingName::from(callee.as_str()),
                kind: if callee_kind == CalleeKind::DefinitelyExternal {
                    NameRefKind::External
                } else {
                    NameRefKind::ConditionalFallthrough
                },
                phase: phase_for_scope(index, scope),
                guards: Vec::new(),
                span: span.clone(),
            });
        }
        live_calls.push(CallSite {
            callee: Atom::from(callee),
            callee_kind,
            qualification: None,
            arguments: Box::default(),
            scope: lexical_scope,
            phase: phase_for_scope(index, scope),
            guards: Vec::new(),
            span,
        });
    };
    for expression in &census.expressions {
        match &expression {
            AnyRExpression::RUnaryExpression(unary) => {
                if let Ok(operator) = unary.operator()
                    && matches!(operator.text_trimmed(), "-" | "+" | "!")
                {
                    record(operator.text_trimmed().to_owned(), expression);
                }
            }
            AnyRExpression::RSubset(_) => record("[".into(), expression),
            AnyRExpression::RSubset2(_) => record("[[".into(), expression),
            AnyRExpression::RExtractExpression(extract) => {
                if let Ok(operator) = extract.operator() {
                    record(operator.text_trimmed().to_owned(), expression);
                }
            }
            AnyRExpression::RBinaryExpression(binary) => {
                let Ok(operator) = binary.operator() else {
                    continue;
                };
                let target = match operator.text_trimmed() {
                    "<-" | "=" | "<<-" => binary.left().ok(),
                    "->" | "->>" => binary.right().ok(),
                    _ => continue,
                };
                let mut target = target;
                while let Some(current) = target.take() {
                    match &current {
                        AnyRExpression::RSubset(subset) => {
                            record("[<-".into(), &current);
                            target = subset.function().ok();
                        }
                        AnyRExpression::RSubset2(subset) => {
                            record("[[<-".into(), &current);
                            target = subset.function().ok();
                        }
                        AnyRExpression::RExtractExpression(extract) => {
                            let Ok(operator) = extract.operator() else {
                                break;
                            };
                            record(format!("{}<-", operator.text_trimmed()), &current);
                            target = extract.left().ok();
                        }
                        AnyRExpression::RCall(call) => {
                            let Some(callee) = identifier_callee(call) else {
                                break;
                            };
                            record(format!("{}<-", callee.as_str()), &current);
                            target = call
                                .arguments()
                                .ok()
                                .and_then(|arguments| {
                                    arguments.items().iter().find_map(std::result::Result::ok)
                                })
                                .and_then(|argument| argument.value());
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
}

fn superassignment_parts(
    root: &RRoot,
    kind: &DefinitionKind,
    target_start: usize,
    target_end: usize,
) -> Option<SuperAssignmentParts> {
    let assignment = assignment_of(root, kind)?;
    let value = node_range(&assignment.value);
    let span = if assignment.value_first {
        TextRange::new(value.start, target_end)
    } else {
        TextRange::new(target_start, value.end)
    };
    let value_symbol = match static_arg_of(&assignment.value) {
        Some(StaticArg::Symbol(name)) => Some((name, value.start, value.end)),
        _ => None,
    };
    Some(SuperAssignmentParts { span, value_symbol })
}

pub(super) fn outer_function(
    expression: &AnyRExpression,
) -> Option<air_r_syntax::RFunctionDefinition> {
    match expression {
        AnyRExpression::RFunctionDefinition(function) => Some(function.clone()),
        AnyRExpression::RBinaryExpression(binary)
            if binary
                .operator()
                .is_ok_and(|operator| operator.text_trimmed() == "<-") =>
        {
            match binary.right().ok()? {
                AnyRExpression::RFunctionDefinition(function) => Some(function),
                _ => None,
            }
        }
        _ => None,
    }
}
