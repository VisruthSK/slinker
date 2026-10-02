use crate::package::{Atom, BindingName};
use crate::syntax::facts::{
    ActiveBindingDef, CallSite, CalleeKind, EvalPhase, LexicalBindingId, LexicalScopeId, NameRef,
    NameRefKind, NamespaceEnumeration, NamespaceInfoRead, NamespaceInfoReceiver, PackageRef,
    ParsedExpression, ParsedRFile, PinnedDefault, ResourcePackage, ResourceRef, SemanticIssue,
    SemanticIssueKind, StaticArg, StaticEnvironment, SyntaxEffect, SyntaxEffectKind,
};
use crate::syntax::source::{SourceId, Span, TextRange};
use air_r_parser::{RParserOptions, parse};
use air_r_syntax::{AnyRExpression, RBinaryExpression, RCall, RRoot};
use biome_rowan::{AstNode, AstNodeList, AstSeparatedList};
use oak_semantic::build_index;
use oak_semantic::semantic_index::{
    DefinitionKind, NamespaceAccessKind, ScopeId, ScopeKind, SemanticDiagnostic, SemanticIndex,
    UseId,
};
use std::collections::{BTreeMap, BTreeSet, HashMap};

mod construction;
mod context;
mod declarations;
mod guards;
mod predicates;
mod proofs;
mod scan;
#[cfg(test)]
mod tests;

pub use context::OakParseContext;
pub(crate) use context::SharedNames;
pub(crate) use context::{NamespaceImportResolution, NamespaceImports};
pub(crate) use proofs::closure_definitely_non_returning;

use construction::{collect_construction, outer_function};
use context::SlinkerImportsResolver;
use declarations::{Declarations, collect_declarations, sole_positional_argument};
use guards::{
    apply_guard_regions_to_calls, apply_guard_regions_to_effects,
    apply_guard_regions_to_package_refs, apply_guard_regions_to_references, hook_guard_regions,
    if_guard_regions,
};
use proofs::{
    ControlRegions, conditional_fallthrough_proven_bound, definition_must_execute_before_position,
    for_body_use_is_bound, formal_default_use_is_bound, is_base_call,
    post_for_use_may_fall_through, recursive_closure_binding_is_initialized,
};
use scan::{
    ForRegion, FunctionRegion, IfRegion, RawArgument, RawCall, argument_spans, call_after_name,
    expression_end, find_for_regions, find_function_regions, find_if_regions, namespace_extent,
    skip_trivia, statement_start, static_arg, static_args, static_symbol_range, trim_end_offset,
    word_boundary_after,
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
struct LiveCall {
    site: CallSite,
    raw: RawCall,
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
        let parsed = parse(text, RParserOptions::default());
        if let Some(error) = parsed.error() {
            return Err(error.to_string());
        }
        let root = parsed.tree();
        let Some(evaluated) = evaluated_quotation_text(text, &root, context) else {
            let index = build_semantic_index(&root, context);
            return Ok(translate_index(source, text, context, &root, &index));
        };
        let parsed = parse(&evaluated, RParserOptions::default());
        if let Some(error) = parsed.error() {
            return Err(error.to_string());
        }
        let root = parsed.tree();
        let index = build_semantic_index(&root, context);
        Ok(translate_index(source, &evaluated, context, &root, &index))
    }
}

fn evaluated_quotation_text(text: &str, root: &RRoot, context: &OakParseContext) -> Option<String> {
    if !text.contains("eval") {
        return None;
    }
    let mut blanks = Vec::new();
    for call in root.syntax().descendants().filter_map(RCall::cast) {
        let Some(callee) = base_callee(&call, context) else {
            continue;
        };
        let callee_range = callee.range.clone();
        let Some(argument) = sole_positional_argument(&call) else {
            continue;
        };
        match callee.name() {
            "evalq" => blanks.push(callee_range),
            "eval" => {
                let AnyRExpression::RCall(quotation) = argument else {
                    continue;
                };
                let Some(quoter) = base_callee(&quotation, context) else {
                    continue;
                };
                let quoter_range = quoter.range.clone();
                if !matches!(quoter.name(), "quote" | "bquote")
                    || sole_positional_argument(&quotation).is_none()
                {
                    continue;
                }
                blanks.extend([callee_range, quoter_range]);
                if quoter.name() == "bquote" {
                    blanks.extend(
                        quotation
                            .syntax()
                            .descendants()
                            .filter_map(RCall::cast)
                            .filter_map(|splice| identifier_callee(&splice))
                            .filter(|callee| callee.name() == ".")
                            .map(|callee| callee.range),
                    );
                }
            }
            _ => {}
        }
    }
    if blanks.is_empty() {
        return None;
    }
    let mut bytes = text.as_bytes().to_vec();
    for range in blanks {
        bytes[range].fill(b' ');
    }
    Some(String::from_utf8(bytes).expect("ASCII identifiers are replaced by ASCII spaces"))
}

fn data_mask_ranges(
    text: &str,
    root: &RRoot,
    context: &OakParseContext,
) -> Vec<std::ops::Range<usize>> {
    if !["with", "subset", "transform"]
        .iter()
        .any(|callee| text.contains(callee))
    {
        return Vec::new();
    }
    root.syntax()
        .descendants()
        .filter_map(RCall::cast)
        .filter(|call| {
            base_callee(call, context).is_some_and(|callee| {
                matches!(callee.name(), "with" | "within" | "subset" | "transform")
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
    text: &str,
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
            let name = ast_text(text, &parameter.name().ok()?);
            let default = parameter.default()?.value().ok()?;
            let Some(StaticArg::String(value)) = static_arg(ast_str(text, &default)) else {
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

fn is_frame_intrinsic(name: &str) -> bool {
    matches!(
        name,
        ".Generic" | ".Class" | ".Method" | ".GenericCallEnv" | ".GenericDefEnv" | ".Group"
    ) || is_dots_element(name)
}

fn is_dots_element(name: &str) -> bool {
    name.strip_prefix("..")
        .is_some_and(|index| !index.is_empty() && index.bytes().all(|byte| byte.is_ascii_digit()))
}

struct Callee {
    token: air_r_syntax::RSyntaxToken,
    range: std::ops::Range<usize>,
}

impl Callee {
    fn name(&self) -> &str {
        self.token.text_trimmed()
    }
}

fn identifier_callee(call: &RCall) -> Option<Callee> {
    let AnyRExpression::RIdentifier(identifier) = call.function().ok()? else {
        return None;
    };
    let range = identifier.syntax().text_trimmed_range();
    Some(Callee {
        token: identifier.name_token().ok()?,
        range: text_offset(range.start())..text_offset(range.end()),
    })
}

fn base_callee(call: &RCall, context: &OakParseContext) -> Option<Callee> {
    identifier_callee(call).filter(|callee| context.resolves_to_base(callee.name()))
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

    fn call_context(
        &self,
        index: &SemanticIndex,
        offset: usize,
        args: &[Option<StaticArg>],
    ) -> (LexicalScopeId, Vec<Option<LexicalBindingId>>) {
        let (scope, lexical) = self.at(index, offset);
        let bindings = args
            .iter()
            .map(|argument| match argument {
                Some(StaticArg::Symbol(name)) => self.binding(index, name, scope),
                Some(StaticArg::String(_)) | None => None,
            })
            .collect();
        (lexical, bindings)
    }
}

pub fn assigned_value_start(text: &str) -> Option<usize> {
    let parsed = parse(text, RParserOptions::default());
    let assignment = parsed
        .tree()
        .syntax()
        .descendants()
        .find_map(RBinaryExpression::cast)?;
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
    build_index(root, SlinkerImportsResolver { context })
}

fn translate_index(
    source: SourceId,
    text: &str,
    context: &OakParseContext,
    root: &RRoot,
    index: &SemanticIndex,
) -> ParsedRFile {
    let scopes = LexicalScopes::new(index);
    let declarations = collect_declarations(&source, text, root, context, index, &scopes);
    let mut live_uses = collect_live_uses(index, &declarations);

    let function_regions = find_function_regions(text);
    let for_regions = find_for_regions(text);
    let if_regions = find_if_regions(text);
    refine_callee_kinds(
        text,
        context,
        index,
        &function_regions,
        &for_regions,
        &if_regions,
        &mut live_uses,
    );

    let mut references = name_references(&source, text, context, root, index, &live_uses);
    let mut live_calls = lexical_calls(&source, text, index, &scopes, &live_uses);
    let translation = Translation {
        source: &source,
        text,
        root,
        index,
        scopes: &scopes,
        declarations: &declarations,
    };
    let mut package_refs = namespace_access_facts(translation, &live_uses, &mut live_calls);
    binary_operator_facts(translation, &mut references, &mut live_calls);
    dispatching_syntax_facts(translation, &mut live_calls);

    deduplicate_calls(&mut live_calls);

    let mut guard_regions = if_guard_regions(text, context, &if_regions, &live_calls);
    apply_guard_regions_to_references(&guard_regions, &mut references);
    apply_guard_regions_to_package_refs(&guard_regions, &mut package_refs);
    apply_guard_regions_to_calls(&guard_regions, &mut live_calls);

    let hook_regions = hook_guard_regions(context, &live_calls);
    apply_guard_regions_to_references(&hook_regions, &mut references);
    apply_guard_regions_to_package_refs(&hook_regions, &mut package_refs);
    apply_guard_regions_to_calls(&hook_regions, &mut live_calls);
    guard_regions.extend(hook_regions);

    let resource_refs = collect_resources(source, context, &live_calls);
    let environment_aliases = collect_environment_aliases(text, context, index, &live_calls);
    let active_bindings = collect_active_bindings(
        source,
        context,
        &live_calls,
        &environment_aliases,
        &if_regions,
    );
    let namespace_enumerations =
        collect_namespace_enumerations(source, context, &live_calls, &environment_aliases);
    let (mut effects, suppressed_reference_spans) = collect_superassignments(
        source,
        text,
        index,
        &function_regions,
        &for_regions,
        &if_regions,
    );
    suppress_superassignment_references(&mut effects, &mut references, &suppressed_reference_spans);
    apply_guard_regions_to_effects(&guard_regions, &mut effects);

    let (parameters, construction) = collect_construction(source, text, root, &live_calls);
    let namespace_info_reads = collect_namespace_info_reads(source, text, root, &declarations);
    let calls: Vec<CallSite> = live_calls.into_iter().map(|call| call.site).collect();
    let mut issues = translate_diagnostics(source, index);
    issues.extend(declarations.issues);

    let used_parameters = used_parameters(index);
    let pinned_defaults = pinned_defaults(text, root, index, &calls, &effects);
    ParsedRFile {
        expressions: vec![ParsedExpression {
            span: Span::new(source, 0, text.len()),
            parameters: parameters.into_iter().map(Atom::from).collect(),
            used_parameters: used_parameters.into_iter().map(Atom::from).collect(),
            pinned_defaults,
            references,
            package_refs,
            resource_refs,
            calls,
            active_bindings,
            effects,
            construction,
            namespace_info_reads,
            namespace_enumerations,
        }],
        issues,
        scope_parents: scopes.parents,
        declarations: declarations.declarations,
    }
}

const NAMESPACE_INFO: &str = ".__NAMESPACE__.";

fn collect_namespace_info_reads(
    source: SourceId,
    text: &str,
    root: &RRoot,
    declarations: &Declarations,
) -> Vec<NamespaceInfoRead> {
    let mut reads = Vec::new();
    for node in root.syntax().descendants() {
        if declarations.is_inert(text_offset(node.text_trimmed_range().start())) {
            continue;
        }
        let Some(expression) = AnyRExpression::cast(node) else {
            continue;
        };
        if let Some((receiver, member)) = member_access(text, &expression)
            && member == NAMESPACE_INFO
        {
            reads.push(NamespaceInfoRead {
                receiver: namespace_info_receiver(text, &receiver),
                field: extracted_field(text, &expression),
                span: ast_span(&source, &expression),
            });
        } else if let AnyRExpression::RIdentifier(_) = &expression
            && unquoted(ast_str(text, &expression)) == NAMESPACE_INFO
            && let Some(field) = extracted_field(text, &expression)
        {
            reads.push(NamespaceInfoRead {
                receiver: NamespaceInfoReceiver::Lexical,
                field: Some(field),
                span: ast_span(&source, &expression),
            });
        }
    }
    reads
}

fn member_access(text: &str, expression: &AnyRExpression) -> Option<(AnyRExpression, String)> {
    match expression {
        AnyRExpression::RExtractExpression(extract)
            if extract.operator().ok()?.text_trimmed() == "$" =>
        {
            Some((
                extract.left().ok()?,
                unquoted(ast_str(text, &extract.right().ok()?)),
            ))
        }
        AnyRExpression::RSubset2(subset) => {
            let mut arguments = subset.arguments().ok()?.items().iter();
            let index = arguments.next()?.ok()?.value()?;
            if arguments.next().is_some() {
                return None;
            }
            match static_arg(ast_str(text, &index).trim())? {
                StaticArg::String(member) => {
                    Some((subset.function().ok()?, member.as_str().to_owned()))
                }
                StaticArg::Symbol(_) => None,
            }
        }
        _ => None,
    }
}

fn extracted_field(text: &str, expression: &AnyRExpression) -> Option<String> {
    let parent = AnyRExpression::cast(expression.syntax().parent()?)?;
    let (receiver, member) = member_access(text, &parent)?;
    (receiver.syntax() == expression.syntax()).then_some(member)
}

fn namespace_info_receiver(text: &str, receiver: &AnyRExpression) -> NamespaceInfoReceiver {
    let AnyRExpression::RCall(call) = receiver else {
        return NamespaceInfoReceiver::Computed;
    };
    let named = call.function().ok().is_some_and(|function| {
        let callee = ast_str(text, &function);
        matches!(
            callee.trim_start_matches("base::"),
            "asNamespace" | "getNamespace"
        )
    });
    match sole_positional_argument(call)
        .and_then(|argument| static_arg(ast_str(text, &argument).trim()))
    {
        Some(StaticArg::String(package)) if named => {
            NamespaceInfoReceiver::Namespace(package.into())
        }
        _ => NamespaceInfoReceiver::Computed,
    }
}

fn unquoted(name: &str) -> String {
    name.trim()
        .trim_matches('`')
        .trim_matches(['"', '\''])
        .to_owned()
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
            if is_frame_intrinsic(&name) {
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
    text: &str,
    context: &OakParseContext,
    index: &SemanticIndex,
    function_regions: &[FunctionRegion],
    for_regions: &[ForRegion],
    if_regions: &[IfRegion],
    live_uses: &mut [LiveUse],
) {
    for live_use in live_uses.iter_mut() {
        if live_use.callee_kind == CalleeKind::DefinitelyLexical
            && post_for_use_may_fall_through(text, index, for_regions, if_regions, live_use)
        {
            live_use.callee_kind = CalleeKind::ConditionalFallthrough;
        }

        if live_use.callee_kind != CalleeKind::DefinitelyLexical
            && (formal_default_use_is_bound(function_regions, live_use)
                || for_body_use_is_bound(index, for_regions, live_use)
                || recursive_closure_binding_is_initialized(
                    text,
                    index,
                    function_regions,
                    live_use,
                ))
        {
            live_use.callee_kind = CalleeKind::DefinitelyLexical;
            continue;
        }
        if live_use.callee_kind == CalleeKind::ConditionalFallthrough
            && conditional_fallthrough_proven_bound(
                text,
                context,
                index,
                for_regions,
                if_regions,
                live_use,
            )
        {
            live_use.callee_kind = CalleeKind::DefinitelyLexical;
        }
    }
}

fn name_references(
    source: &SourceId,
    text: &str,
    context: &OakParseContext,
    root: &RRoot,
    index: &SemanticIndex,
    live_uses: &[LiveUse],
) -> Vec<NameRef> {
    let data_masks = data_mask_ranges(text, root, context);
    live_uses
        .iter()
        .filter_map(|live_use| {
            if is_r_language_constant(&live_use.name) {
                return None;
            }
            let masked = data_masks.iter().any(|mask| mask.contains(&live_use.start));
            let kind = match live_use.callee_kind {
                CalleeKind::DefinitelyLexical
                    if text.as_bytes().get(skip_trivia(text, live_use.end)) == Some(&b'(')
                        && !reaches_only_closures(text, index, live_use) =>
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
            let replaced =
                call_after_name(text, live_use.start, live_use.end).is_some_and(|call| {
                    let rest = &text[skip_trivia(text, call.end)..];
                    rest.starts_with("<-") || rest.starts_with("<<-")
                });
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
    text: &str,
    index: &SemanticIndex,
    scopes: &LexicalScopes,
    live_uses: &[LiveUse],
) -> Vec<LiveCall> {
    let mut live_calls = Vec::new();

    for live_use in live_uses {
        let Some(raw) = call_after_name(text, live_use.start, live_use.end) else {
            continue;
        };
        let args = static_args(&raw);
        let (scope, arg_bindings) = scopes.call_context(index, raw.start, &args);
        live_calls.push(LiveCall {
            site: CallSite {
                callee: Atom::from(live_use.name.as_str()),
                callee_kind: live_use.callee_kind,
                qualified_package: None,
                args,
                arg_names: raw
                    .args
                    .iter()
                    .map(|argument| argument.name.as_deref().map(Atom::from))
                    .collect(),
                arg_spans: argument_spans(source, &raw.args),
                local_closure_args: local_closure_arguments(text, index, live_uses, &raw.args),
                scope,
                arg_bindings,
                phase: live_use.phase,
                guards: Vec::new(),
                span: Span::new(*source, raw.start, raw.end),
            },
            raw,
        });
    }
    live_calls
}

#[derive(Clone, Copy)]
struct Translation<'a> {
    source: &'a SourceId,
    text: &'a str,
    root: &'a RRoot,
    index: &'a SemanticIndex,
    scopes: &'a LexicalScopes,
    declarations: &'a Declarations,
}

fn namespace_access_facts(
    translation: Translation<'_>,
    live_uses: &[LiveUse],
    live_calls: &mut Vec<LiveCall>,
) -> Vec<PackageRef> {
    let Translation {
        source,
        text,
        index,
        scopes,
        declarations,
        ..
    } = translation;
    let mut package_refs = Vec::new();
    for access in index.namespace_accesses() {
        let start = text_offset(access.offset());
        if declarations.is_inert(start) {
            continue;
        }
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
            package: access.package().into(),
            symbol: access.symbol().into(),
            internal,
            guards: Vec::new(),
            span: Span::new(*source, start, access_end),
        });

        if let Some(raw) = call_after_name(text, start, access_end) {
            let (scope, _) = index.scope_at(access.offset());
            let args = static_args(&raw);
            let (lexical_scope, arg_bindings) = scopes.call_context(index, raw.start, &args);
            live_calls.push(LiveCall {
                site: CallSite {
                    callee: Atom::from(access.symbol()),
                    callee_kind: CalleeKind::DefinitelyExternal,
                    qualified_package: Some(Atom::from(access.package())),
                    args,
                    arg_names: raw
                        .args
                        .iter()
                        .map(|argument| argument.name.as_deref().map(Atom::from))
                        .collect(),
                    arg_spans: argument_spans(source, &raw.args),
                    local_closure_args: local_closure_arguments(text, index, live_uses, &raw.args),
                    scope: lexical_scope,
                    arg_bindings,
                    phase: phase_for_scope(index, scope),
                    guards: Vec::new(),
                    span: Span::new(*source, raw.start, raw.end),
                },
                raw,
            });
        }
    }
    package_refs
}

fn binary_operator_facts(
    translation: Translation<'_>,
    references: &mut Vec<NameRef>,
    live_calls: &mut Vec<LiveCall>,
) {
    let Translation {
        source,
        text,
        root,
        index,
        scopes,
        declarations,
    } = translation;
    for binary in root
        .syntax()
        .descendants()
        .filter_map(RBinaryExpression::cast)
    {
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
        let span = ast_span(source, &binary);
        if declarations.is_inert(span.start) {
            continue;
        }
        let (scope, _) = index.scope_at(binary.range().start());
        if operator.len() > 2 && operator.starts_with('%') && operator.ends_with('%') {
            let range = operator_token.text_trimmed_range();
            references.push(NameRef {
                name: BindingName::from(operator.as_str()),
                kind: NameRefKind::External,
                phase: phase_for_scope(index, scope),
                guards: Vec::new(),
                span: Span::new(
                    *source,
                    text_offset(range.start()),
                    text_offset(range.end()),
                ),
            });
        }
        let argument = |expression: &AnyRExpression| {
            let span = ast_span(source, expression);
            (
                static_arg(text.get(span.start..span.end).unwrap_or_default().trim()),
                Some(span),
            )
        };
        let (left_arg, left_span) = argument(&left);
        let (right_arg, right_span) = argument(&right);
        let args = vec![left_arg, right_arg];
        let (lexical_scope, arg_bindings) = scopes.call_context(index, span.start, &args);
        live_calls.push(LiveCall {
            site: CallSite {
                callee: Atom::from(operator.as_str()),
                callee_kind: CalleeKind::DefinitelyExternal,
                qualified_package: (!operator.starts_with('%')).then(|| "base".into()),
                args,
                arg_names: vec![None, None],
                arg_spans: vec![left_span, right_span],
                local_closure_args: vec![false, false],
                scope: lexical_scope,
                arg_bindings,
                phase: phase_for_scope(index, scope),
                guards: Vec::new(),
                span: span.clone(),
            },
            raw: RawCall {
                start: span.start,
                end: span.end,
                args: Vec::new(),
            },
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

fn ast_str<'a>(text: &'a str, node: &impl AstNode<Language = air_r_syntax::RLanguage>) -> &'a str {
    let span = node.syntax().text_trimmed_range();
    &text[text_offset(span.start())..text_offset(span.end())]
}

fn ast_atom(
    text: &str,
    node: &impl AstNode<Language = air_r_syntax::RLanguage>,
) -> crate::package::Atom {
    crate::package::Atom::from(ast_str(text, node))
}

fn ast_text(text: &str, node: &impl AstNode<Language = air_r_syntax::RLanguage>) -> String {
    let span = node.syntax().text_trimmed_range();
    text[text_offset(span.start())..text_offset(span.end())].to_owned()
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

fn is_r_language_constant(name: &str) -> bool {
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
        if !call
            .raw
            .args
            .iter()
            .any(|argument| argument.name.as_deref() == Some("package"))
        {
            continue;
        }
        let package = named_static_string(&call.raw.args, "package").map_or_else(
            || {
                ResourcePackage::Computed(
                    call.site
                        .arg_names
                        .iter()
                        .position(|name| name.as_deref() == Some("package"))
                        .and_then(|index| call.site.arg_bindings.get(index)?.clone()),
                )
            },
            ResourcePackage::Literal,
        );
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
            scope: call.site.scope,
            span: Span::new(source, call.site.span.start, call.site.span.end),
        });
    }
    resources
}

fn collect_active_bindings(
    source: SourceId,
    context: &OakParseContext,
    calls: &[LiveCall],
    aliases: &BTreeMap<String, StaticEnvironment>,
    if_regions: &[IfRegion],
) -> Vec<ActiveBindingDef> {
    let mut bindings = Vec::new();

    for call in calls {
        if call.site.callee != "makeActiveBinding" || !is_base_call(context, &call.site) {
            continue;
        }
        let Some(StaticArg::String(name)) = call
            .raw
            .args
            .first()
            .and_then(|argument| argument.static_arg.as_ref())
        else {
            continue;
        };
        let Some(target_argument) = call.raw.args.get(2) else {
            continue;
        };
        let target = environment_target(context, calls, target_argument, aliases);
        let Some(target) = target else {
            continue;
        };
        let certain = !if_regions.iter().any(|region| {
            region.then_branch.contains_range(call.site.span.range())
                || region
                    .else_branch
                    .is_some_and(|branch| branch.contains_range(call.site.span.range()))
        });
        bindings.push(ActiveBindingDef {
            name: name.clone(),
            target,
            certain,
            guards: call.site.guards.clone(),
            span: Span::new(source, call.site.span.start, call.site.span.end),
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
            let Some((value_start, value_end)) = assignment_rhs_after(text, target_end, "<-")
            else {
                continue;
            };
            let value = TextRange::new(value_start, value_end);
            let Some(target) = calls
                .iter()
                .find(|call| {
                    call.site.callee == "environment"
                        && is_base_call(context, &call.site)
                        && value.contains_range(call.site.span.range())
                })
                .or_else(|| calls.iter().find(|call| call.site.span.range() == value))
                .and_then(|call| static_environment(context, call))
            else {
                continue;
            };
            aliases.insert(name.to_owned(), target);
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
    if let Some(StaticArg::Symbol(name)) = &argument.static_arg
        && let Some(target) = aliases.get(name.as_str())
    {
        return Some(target.clone());
    }

    let nested = calls
        .iter()
        .find(|call| argument.value.contains_range(call.site.span.range()))?;
    static_environment(context, nested)
}

fn static_environment(context: &OakParseContext, call: &LiveCall) -> Option<StaticEnvironment> {
    if !is_base_call(context, &call.site) {
        return None;
    }
    match (
        call.site.callee.as_str(),
        call.raw.args.first()?.static_arg.as_ref()?,
    ) {
        ("asNamespace" | "getNamespace", StaticArg::String(package)) => {
            Some(StaticEnvironment::Namespace(package.into()))
        }
        ("environment", StaticArg::Symbol(binding)) => {
            Some(StaticEnvironment::ClosureBinding(binding.into()))
        }
        _ => None,
    }
}

fn enumerated_environment_formals(callee: &str) -> Option<(&'static [&'static str], &'static str)> {
    match callee {
        "as.list" | "as.list.environment" => Some((&["x"], "x")),
        "mget" => Some((&["x", "envir"], "envir")),
        "eapply" => Some((&["env"], "env")),
        _ => None,
    }
}

fn formal_argument<'a>(
    call: &'a RawCall,
    formals: &[&str],
    target: &str,
) -> Option<&'a RawArgument> {
    if let Some(named) = call
        .args
        .iter()
        .find(|argument| argument.name.as_deref() == Some(target))
    {
        return Some(named);
    }
    let position = formals.iter().position(|formal| *formal == target)?;
    let earlier_named = formals[..position]
        .iter()
        .filter(|formal| {
            call.args
                .iter()
                .any(|argument| argument.name.as_deref() == Some(**formal))
        })
        .count();
    call.args
        .iter()
        .filter(|argument| argument.name.is_none())
        .nth(position - earlier_named)
}

fn collect_namespace_enumerations(
    source: SourceId,
    context: &OakParseContext,
    calls: &[LiveCall],
    aliases: &BTreeMap<String, StaticEnvironment>,
) -> Vec<NamespaceEnumeration> {
    let mut enumerations = Vec::new();
    for call in calls {
        let Some((formals, target)) = enumerated_environment_formals(&call.site.callee) else {
            continue;
        };
        if !is_base_call(context, &call.site) {
            continue;
        }
        let Some(argument) = formal_argument(&call.raw, formals, target) else {
            continue;
        };
        let environment = match &argument.static_arg {
            Some(StaticArg::Symbol(name)) => aliases.get(name.as_str()).cloned(),
            _ => calls
                .iter()
                .find(|nested| nested.site.span.range() == argument.value)
                .and_then(|nested| static_environment(context, nested)),
        };
        if let Some(StaticEnvironment::Namespace(package)) = environment {
            enumerations.push(NamespaceEnumeration {
                package,
                callee: call.site.callee.clone(),
                span: Span::new(source, call.site.span.start, call.site.span.end),
            });
        }
    }
    enumerations
}

fn collect_superassignments(
    source: SourceId,
    text: &str,
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
            let parts = superassignment_parts(text, target_start, target_end);
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
                text,
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
    text: &str,
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
                            text,
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
    match call.args.first()?.as_ref()? {
        StaticArg::String(value) => Some(value),
        StaticArg::Symbol(_) => None,
    }
}

fn named_static_string(arguments: &[RawArgument], name: &str) -> Option<String> {
    arguments.iter().find_map(|argument| {
        (argument.name.as_deref() == Some(name)).then(|| match &argument.static_arg {
            Some(StaticArg::String(value)) => Some(value.as_str().to_owned()),
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

fn superassignment_parts(
    text: &str,
    target_start: usize,
    target_end: usize,
) -> Option<SuperAssignmentParts> {
    let mut cursor = skip_trivia(text, target_end);
    if text.get(cursor..)?.starts_with("<<-") {
        cursor += 3;
        let value_start = skip_trivia(text, cursor);
        let value_end = expression_end(text, value_start);
        return Some(SuperAssignmentParts {
            span: TextRange::new(target_start, value_end),
            value_symbol: static_symbol_range(text, value_start, value_end),
        });
    }

    let statement_start = statement_start(text, target_start);
    let before_target = text.get(statement_start..target_start)?;
    let operator = before_target.rfind("->>")?;
    let value_start = skip_trivia(text, statement_start);
    let value_end = trim_end_offset(text, statement_start + operator);
    Some(SuperAssignmentParts {
        span: TextRange::new(value_start, target_end),
        value_symbol: static_symbol_range(text, value_start, value_end),
    })
}

fn local_closure_arguments(
    text: &str,
    index: &SemanticIndex,
    live_uses: &[LiveUse],
    arguments: &[RawArgument],
) -> Vec<bool> {
    arguments
        .iter()
        .map(|argument| {
            let Some(StaticArg::Symbol(name)) = &argument.static_arg else {
                return false;
            };
            let Some(use_site) = live_uses.iter().find(|live_use| {
                live_use.name == *name
                    && live_use.start == argument.value.start
                    && live_use.end == argument.value.end
                    && live_use.callee_kind == CalleeKind::DefinitelyLexical
            }) else {
                return false;
            };
            index
                .reaching_definitions(use_site.scope, use_site.use_id)
                .any(|(scope, definition_id)| {
                    definition_is_closure(text, index, scope, definition_id)
                })
        })
        .collect()
}

fn reaches_only_closures(text: &str, index: &SemanticIndex, live_use: &LiveUse) -> bool {
    let mut definitions = index
        .reaching_definitions(live_use.scope, live_use.use_id)
        .peekable();
    definitions.peek().is_some()
        && definitions
            .all(|(scope, definition_id)| definition_is_closure(text, index, scope, definition_id))
}

fn definition_is_closure(
    text: &str,
    index: &SemanticIndex,
    scope: ScopeId,
    definition_id: oak_semantic::semantic_index::DefinitionId,
) -> bool {
    let definition = &index.definitions(scope)[definition_id];
    matches!(definition.kind(), DefinitionKind::Assignment(_))
        && assignment_rhs_after(text, text_offset(definition.range().end()), "<-")
            .and_then(|(start, _)| text.get(start..))
            .is_some_and(|rhs| {
                rhs.starts_with("function") && word_boundary_after(rhs, "function".len())
            })
}

fn dispatching_syntax_facts(translation: Translation<'_>, live_calls: &mut Vec<LiveCall>) {
    let Translation {
        source,
        root,
        index,
        scopes,
        declarations,
        ..
    } = translation;
    let mut record = |callee: String, qualified: bool, node: &AnyRExpression| {
        let span = ast_span(source, node);
        if declarations.is_inert(span.start) {
            return;
        }
        let (scope, _) = index.scope_at(node.syntax().text_trimmed_range().start());
        let (lexical_scope, arg_bindings) = scopes.call_context(index, span.start, &[]);
        live_calls.push(LiveCall {
            site: CallSite {
                callee: Atom::from(callee),
                callee_kind: CalleeKind::DefinitelyExternal,
                qualified_package: qualified.then(|| "base".into()),
                args: Vec::new(),
                arg_names: Vec::new(),
                arg_spans: Vec::new(),
                local_closure_args: Vec::new(),
                scope: lexical_scope,
                arg_bindings,
                phase: phase_for_scope(index, scope),
                guards: Vec::new(),
                span: span.clone(),
            },
            raw: RawCall {
                start: span.start,
                end: span.end,
                args: Vec::new(),
            },
        });
    };
    for expression in root.syntax().descendants().filter_map(AnyRExpression::cast) {
        match &expression {
            AnyRExpression::RUnaryExpression(unary) => {
                if let Ok(operator) = unary.operator()
                    && matches!(operator.text_trimmed(), "-" | "+" | "!")
                {
                    record(operator.text_trimmed().to_owned(), true, &expression);
                }
            }
            AnyRExpression::RSubset(_) => record("[".into(), true, &expression),
            AnyRExpression::RSubset2(_) => record("[[".into(), true, &expression),
            AnyRExpression::RExtractExpression(extract) => {
                if let Ok(operator) = extract.operator() {
                    record(operator.text_trimmed().to_owned(), true, &expression);
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
                            record("[<-".into(), true, &current);
                            target = subset.function().ok();
                        }
                        AnyRExpression::RSubset2(subset) => {
                            record("[[<-".into(), true, &current);
                            target = subset.function().ok();
                        }
                        AnyRExpression::RExtractExpression(extract) => {
                            let Ok(operator) = extract.operator() else {
                                break;
                            };
                            record(format!("{}<-", operator.text_trimmed()), true, &current);
                            target = extract.left().ok();
                        }
                        AnyRExpression::RCall(call) => {
                            let Some(callee) = identifier_callee(call) else {
                                break;
                            };
                            record(format!("{}<-", callee.name()), false, &current);
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
