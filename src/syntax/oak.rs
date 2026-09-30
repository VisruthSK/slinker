//! Oak semantic adapter.
//!
//! Air owns parsing. Oak owns lexical scopes, definitions, uses, use-def
//! relationships, lexical fallthrough, and annotated evaluation/NSE effects.
//! Slinker translates Oak results into the fact types consumed by the existing
//! demand linker. Source-text inspection below recovers static arguments and
//! spans at sites Oak has already classified as semantically live. A narrow
//! refinement can discharge Oak conditional fallthrough only when Oak's own
//! reaching definitions plus stable repeated predicates or terminating branches
//! prove that every surviving path is bound. It does not build a parallel lexical
//! environment or general control-flow evaluator.

use crate::syntax::facts::{
    ActiveBindingDef, BindingDeclaration, CallSite, CalleeKind, ConstructionArgument,
    ConstructionCall, ConstructionExpr, ConstructionExprKind, ConstructionTarget, DeclaredCallable,
    DeclaredDomain, EvalPhase, LexicalBindingId, LexicalScopeId, NameRef, NameRefKind,
    NamespaceEnumeration, NamespaceInfoRead, NamespaceInfoReceiver, PackageGuard, PackageRef,
    ParsedExpression, ParsedRFile, ResourcePackage, ResourceRef, SemanticIssue, SemanticIssueKind,
    StaticArg, StaticEnvironment, SyntaxEffect, SyntaxEffectKind,
};
use crate::syntax::source::{SourceId, Span, TextRange};
use crate::{Error, Result};
use air_r_parser::{RParserOptions, parse};
use air_r_syntax::{
    AnyRExpression, RBinaryExpression, RCall, RFunctionDefinition, RIfStatement, RRoot,
};
use biome_rowan::{AstNode, AstNodeList, AstSeparatedList};
use oak_semantic::semantic_index::{
    DefinitionKind, NamespaceAccessKind, ScopeId, ScopeKind, SemanticDiagnostic, SemanticIndex,
    UseId,
};
use oak_semantic::{EffectsHandlers, ImportsResolver, SourceResolution, build_index};
use std::collections::{BTreeMap, BTreeSet, HashMap};

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

const IMPORT_ALL_EXCLUDED: [&str; 10] = [
    ".__NAMESPACE__.",
    ".__S3MethodsTable__.",
    ".packageName",
    ".First.lib",
    ".Last.lib",
    ".onLoad",
    ".onAttach",
    ".onDetach",
    ".conflicts.OK",
    ".noGenerics",
];

#[derive(Debug, Clone)]
enum NamespaceImport {
    From {
        package: String,
        bindings: Vec<(String, String)>,
    },
    All {
        package: String,
        /// Exported name -> installed binding name. `None` means the imported
        /// namespace was unavailable while constructing the resolver table.
        exports: Option<BTreeMap<String, String>>,
        except: BTreeSet<String>,
    },
}

impl NamespaceImport {
    fn imports_from_all(except: &BTreeSet<String>, name: &str) -> bool {
        !except.contains(name) && !IMPORT_ALL_EXCLUDED.contains(&name)
    }
}

/// The installed binding one imports-environment name is copied from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ImportedBinding {
    pub(crate) package: String,
    pub(crate) binding: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ImportRecord {
    pub(crate) package: String,
    pub(crate) names: Vec<(String, String)>,
}

/// Immutable NAMESPACE import-resolution table.
///
/// Imports keep their installed order and a later import of a name replaces an
/// earlier one, as R fills a namespace's imports environment. A missing
/// import-all only makes a name ambiguous once resolution actually reaches
/// that entry; it does not erase a later package that provides the name.
#[derive(Debug, Default, Clone)]
pub(crate) struct NamespaceImports {
    imports: Vec<NamespaceImport>,
}

impl NamespaceImports {
    pub(crate) fn add_import_from(
        &mut self,
        package: impl Into<String>,
        bindings: impl IntoIterator<Item = (String, String)>,
    ) {
        self.imports.push(NamespaceImport::From {
            package: package.into(),
            bindings: bindings.into_iter().collect(),
        });
    }

    pub(crate) fn add_import_all(
        &mut self,
        package: impl Into<String>,
        exports: Option<BTreeMap<String, String>>,
        except: impl IntoIterator<Item = String>,
    ) {
        self.imports.push(NamespaceImport::All {
            package: package.into(),
            exports,
            except: except.into_iter().collect(),
        });
    }

    pub(crate) fn resolve(&self, name: &str) -> NamespaceImportResolution {
        for import in self.imports.iter().rev() {
            match import {
                NamespaceImport::From { package, bindings } => {
                    if let Some((_, remote)) =
                        bindings.iter().rev().find(|(local, _)| local == name)
                    {
                        return NamespaceImportResolution::Imported {
                            package: package.clone(),
                            binding: remote.clone(),
                            effect_name: remote.clone(),
                        };
                    }
                }
                NamespaceImport::All {
                    package,
                    exports,
                    except,
                } => {
                    if !NamespaceImport::imports_from_all(except, name) {
                        continue;
                    }
                    let Some(exports) = exports else {
                        return NamespaceImportResolution::MissingImportAll {
                            package: package.clone(),
                            binding: name.to_owned(),
                        };
                    };
                    let Some(binding) = exports.get(name) else {
                        continue;
                    };
                    return NamespaceImportResolution::Imported {
                        package: package.clone(),
                        binding: binding.clone(),
                        // Effects are attached to the package's exported function
                        // name, while the linker retains the installed binding name.
                        effect_name: name.to_owned(),
                    };
                }
            }
        }

        NamespaceImportResolution::BaseFallback
    }

    /// Every name the imports environment holds, with the binding `resolve` answers for it,
    /// or the package of an import-all whose exports are unknown.
    pub(crate) fn names(&self) -> std::result::Result<BTreeMap<String, ImportedBinding>, &str> {
        let mut names = BTreeMap::new();
        for import in &self.imports {
            match import {
                NamespaceImport::From { package, bindings } => {
                    for (local, remote) in bindings {
                        names.insert(
                            local.clone(),
                            ImportedBinding {
                                package: package.clone(),
                                binding: remote.clone(),
                            },
                        );
                    }
                }
                NamespaceImport::All {
                    package,
                    exports,
                    except,
                } => {
                    let exports = exports.as_ref().ok_or(package.as_str())?;
                    for (name, binding) in exports {
                        if NamespaceImport::imports_from_all(except, name) {
                            names.insert(
                                name.clone(),
                                ImportedBinding {
                                    package: package.clone(),
                                    binding: binding.clone(),
                                },
                            );
                        }
                    }
                }
            }
        }
        Ok(names)
    }

    pub(crate) fn records(&self) -> std::result::Result<Vec<ImportRecord>, &str> {
        self.imports
            .iter()
            .map(|import| match import {
                NamespaceImport::From { package, bindings } => Ok(ImportRecord {
                    package: package.clone(),
                    names: bindings.clone(),
                }),
                NamespaceImport::All {
                    package,
                    exports,
                    except,
                } => {
                    let exports = exports.as_ref().ok_or(package.as_str())?;
                    Ok(ImportRecord {
                        package: package.clone(),
                        names: exports
                            .keys()
                            .filter(|name| NamespaceImport::imports_from_all(except, name))
                            .map(|name| (name.clone(), name.clone()))
                            .collect(),
                    })
                }
            })
            .collect()
    }
}

/// Installed-namespace facts supplied by slinker to Oak's import/effects
/// resolver. Local lexical state is deliberately absent: Oak owns it.
#[derive(Debug, Default, Clone)]
pub struct OakParseContext {
    shadowed_names: BTreeSet<String>,
    imports: NamespaceImports,
    /// Package-local closures that slinker has conservatively proved cannot
    /// return to their caller. This is used only to refine Oak conditional
    /// fallthrough diagnostics after Oak has identified the live use and its
    /// reaching definitions.
    non_returning_names: BTreeSet<String>,
}

impl OakParseContext {
    pub fn new(shadowed_names: BTreeSet<String>) -> Self {
        Self {
            shadowed_names,
            imports: NamespaceImports::default(),
            non_returning_names: BTreeSet::new(),
        }
    }

    pub(crate) fn with_imports(
        shadowed_names: BTreeSet<String>,
        imports: NamespaceImports,
        non_returning_names: BTreeSet<String>,
    ) -> Self {
        Self {
            shadowed_names,
            imports,
            non_returning_names,
        }
    }

    #[cfg(test)]
    fn add_import_from(
        &mut self,
        local: impl Into<String>,
        package: impl Into<String>,
        remote: impl Into<String>,
    ) {
        self.imports
            .add_import_from(package, [(local.into(), remote.into())]);
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
    scope: ScopeId,
    use_id: UseId,
    callee_kind: CalleeKind,
    phase: EvalPhase,
}

#[derive(Debug, Clone)]
struct RawArgument {
    name: Option<String>,
    value: TextRange,
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

#[derive(Debug, Clone)]
struct FunctionRegion {
    function_start: usize,
    formals: TextRange,
    body: TextRange,
    parameters: BTreeSet<String>,
}

#[derive(Debug, Clone)]
struct ForRegion {
    variable: String,
    variable_start: usize,
    body: TextRange,
}

#[derive(Debug, Clone, Copy)]
struct IfRegion {
    if_start: usize,
    condition: TextRange,
    then_branch: TextRange,
    else_branch: Option<TextRange>,
}

#[derive(Clone, Copy)]
struct ControlRegions<'a> {
    for_regions: &'a [ForRegion],
    if_regions: &'a [IfRegion],
}

#[derive(Clone, Copy)]
struct BindingProofContext<'a> {
    live_use: &'a LiveUse,
    use_assumptions: &'a [BranchAssumption],
    definition_starts: &'a [usize],
    defining_scope: Option<ScopeId>,
}

#[derive(Debug, Clone)]
struct SuperAssignmentParts {
    span: TextRange,
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
    let mut blanks = Vec::new();
    for call in root.syntax().descendants().filter_map(RCall::cast) {
        let Some((callee, callee_range)) = base_callee(&call, context) else {
            continue;
        };
        let Some(argument) = sole_positional_argument(&call) else {
            continue;
        };
        match callee.as_str() {
            "evalq" => blanks.push(callee_range),
            "eval" => {
                let AnyRExpression::RCall(quotation) = argument else {
                    continue;
                };
                let Some((quoter, quoter_range)) = base_callee(&quotation, context) else {
                    continue;
                };
                if !matches!(quoter.as_str(), "quote" | "bquote")
                    || sole_positional_argument(&quotation).is_none()
                {
                    continue;
                }
                blanks.extend([callee_range, quoter_range]);
                if quoter == "bquote" {
                    blanks.extend(
                        quotation
                            .syntax()
                            .descendants()
                            .filter_map(RCall::cast)
                            .filter_map(|splice| identifier_callee(&splice))
                            .filter(|(name, _)| name == ".")
                            .map(|(_, range)| range),
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

fn data_mask_ranges(root: &RRoot, context: &OakParseContext) -> Vec<std::ops::Range<usize>> {
    root.syntax()
        .descendants()
        .filter_map(RCall::cast)
        .filter(|call| {
            base_callee(call, context).is_some_and(|(name, _)| {
                matches!(name.as_str(), "with" | "within" | "subset" | "transform")
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

fn identifier_callee(call: &RCall) -> Option<(String, std::ops::Range<usize>)> {
    let AnyRExpression::RIdentifier(identifier) = call.function().ok()? else {
        return None;
    };
    let range = identifier.syntax().text_trimmed_range();
    Some((
        identifier.syntax().text_trimmed().to_string(),
        text_offset(range.start())..text_offset(range.end()),
    ))
}

fn base_callee(
    call: &RCall,
    context: &OakParseContext,
) -> Option<(String, std::ops::Range<usize>)> {
    identifier_callee(call).filter(|(name, _)| context.resolves_to_base(name))
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
                name: name.to_owned(),
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

struct Declarations {
    declarations: Vec<BindingDeclaration>,
    inert: Vec<std::ops::Range<usize>>,
    issues: Vec<SemanticIssue>,
}

impl Declarations {
    fn is_inert(&self, offset: usize) -> bool {
        self.inert.iter().any(|range| range.contains(&offset))
    }
}

fn collect_declarations(
    source: &SourceId,
    text: &str,
    root: &RRoot,
    context: &OakParseContext,
    index: &SemanticIndex,
    scopes: &LexicalScopes,
) -> Declarations {
    let mut collected = Declarations {
        declarations: Vec::new(),
        inert: Vec::new(),
        issues: Vec::new(),
    };
    for call in root.syntax().descendants().filter_map(RCall::cast) {
        let start = text_offset(call.syntax().text_trimmed_range().start());
        let (scope, lexical) = scopes.at(index, start);
        let Ok(function) = call.function() else {
            continue;
        };
        let is_declare = match &function {
            AnyRExpression::RIdentifier(_) => identifier_callee(&call).is_some_and(|(name, _)| {
                name == "declare"
                    && context.resolves_to_base(&name)
                    && index.resolve(&name, scope).is_none()
            }),
            _ => ast_text(text, &function) == "base::declare",
        };
        let Ok(arguments) = call.arguments() else {
            continue;
        };
        if !is_declare {
            continue;
        }
        for argument in arguments.items().iter().filter_map(std::result::Result::ok) {
            let Some(value) = argument.value() else {
                continue;
            };
            let range = value.syntax().text_trimmed_range();
            collected
                .inert
                .push(text_offset(range.start())..text_offset(range.end()));
            let AnyRExpression::RCall(language) = value else {
                continue;
            };
            if identifier_callee(&language).is_none_or(|(name, _)| name != "slinker") {
                continue;
            }
            collect_slinker_declaration(
                source,
                text,
                index,
                scopes,
                (scope, lexical),
                &language,
                &mut collected,
            );
        }
    }
    collected
}

fn collect_slinker_declaration(
    source: &SourceId,
    text: &str,
    index: &SemanticIndex,
    scopes: &LexicalScopes,
    (scope, lexical): (ScopeId, LexicalScopeId),
    language: &RCall,
    collected: &mut Declarations,
) {
    let Ok(arguments) = language.arguments() else {
        return;
    };
    for argument in arguments.items().iter().filter_map(std::result::Result::ok) {
        let span = ast_span(source, &argument);
        let name = argument
            .name_clause()
            .and_then(|clause| clause.name().ok())
            .map(|name| ast_text(text, &name));
        let domain = argument
            .value()
            .and_then(|value| declared_domain(text, &value));
        let issue = match (name, domain) {
            (Some(name), Some(domain)) => match scopes.binding(index, &name, scope) {
                Some(binding)
                    if collected.declarations.iter().any(|declaration| {
                        declaration.binding == binding
                            && std::mem::discriminant(&declaration.domain)
                                != std::mem::discriminant(&domain)
                    }) =>
                {
                    format!("declarations for `{name}` mix s3(), strings(), and callables()")
                }
                Some(binding) => {
                    collected.declarations.push(BindingDeclaration {
                        declaring_scope: lexical,
                        binding,
                        domain,
                        span,
                    });
                    continue;
                }
                None => format!("declared name `{name}` is not a lexical binding in this function"),
            },
            (None, _) => "slinker() declarations must name the binding they constrain".to_owned(),
            (Some(name), None) => format!(
                "declaration for `{name}` must be s3(\"class\", ...), one_of(s3(...), ...), or strings(\"value\", ...) with literal strings, or callables(pkg::f, g, ...)"
            ),
        };
        collected.issues.push(SemanticIssue {
            kind: SemanticIssueKind::InvalidDeclaration,
            message: issue,
            span: Some(span),
        });
    }
}

fn declared_domain(text: &str, value: &AnyRExpression) -> Option<DeclaredDomain> {
    let (callee, arguments) = declaration_call(value)?;
    match callee.as_str() {
        "strings" => literal_strings(text, &arguments)
            .map(|strings| DeclaredDomain::Strings(strings.into_iter().collect())),
        "s3" | "one_of" => declared_classes(text, value).map(DeclaredDomain::Classes),
        "callables" if !arguments.is_empty() => arguments
            .iter()
            .map(|argument| declared_callable(ast_text(text, argument).trim()))
            .collect::<Option<BTreeSet<_>>>()
            .map(DeclaredDomain::Callables),
        _ => None,
    }
}

fn declared_callable(text: &str) -> Option<DeclaredCallable> {
    let (package, name) = match text.split_once(":::").or_else(|| text.split_once("::")) {
        Some((package, name)) => (Some(package), name),
        None => (None, text),
    };
    let symbol = |candidate: &str| matches!(static_arg(candidate), Some(StaticArg::Symbol(symbol)) if symbol == candidate);
    (package.is_none_or(symbol) && symbol(name)).then(|| DeclaredCallable {
        package: package.map(str::to_owned),
        name: name.to_owned(),
    })
}

fn declared_classes(text: &str, value: &AnyRExpression) -> Option<Vec<Vec<String>>> {
    let (callee, arguments) = declaration_call(value)?;
    match callee.as_str() {
        "s3" => literal_strings(text, &arguments).map(|classes| vec![classes]),
        "one_of" if !arguments.is_empty() => arguments
            .iter()
            .map(|alternative| declared_classes(text, alternative))
            .collect::<Option<Vec<_>>>()
            .map(|alternatives| alternatives.into_iter().flatten().collect()),
        _ => None,
    }
}

fn declaration_call(value: &AnyRExpression) -> Option<(String, Vec<AnyRExpression>)> {
    let AnyRExpression::RCall(call) = value else {
        return None;
    };
    let (callee, _) = identifier_callee(call)?;
    let arguments = call
        .arguments()
        .ok()?
        .items()
        .iter()
        .map(|argument| {
            let argument = argument.ok()?;
            argument.name_clause().is_none().then_some(())?;
            argument.value()
        })
        .collect::<Option<Vec<_>>>()?;
    Some((callee, arguments))
}

fn literal_strings(text: &str, arguments: &[AnyRExpression]) -> Option<Vec<String>> {
    if arguments.is_empty() {
        return None;
    }
    arguments
        .iter()
        .map(
            |argument| match static_arg(ast_text(text, argument).trim()) {
                Some(StaticArg::String(value)) => Some(value),
                Some(StaticArg::Symbol(_)) | None => None,
            },
        )
        .collect()
}

fn sole_positional_argument(call: &RCall) -> Option<AnyRExpression> {
    let arguments = call.arguments().ok()?.items();
    let mut items = arguments.iter();
    let argument = items.next()?.ok()?;
    if items.next().is_some() || argument.name_clause().is_some() {
        return None;
    }
    argument.value()
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
    let calls = live_calls.into_iter().map(|call| call.site).collect();
    let mut issues = translate_diagnostics(source, index);
    issues.extend(declarations.issues);

    let used_parameters = used_parameters(index);
    ParsedRFile {
        expressions: vec![ParsedExpression {
            span: Span::new(source, 0, text.len()),
            parameters,
            used_parameters,
            definitions: Vec::new(),
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

/// Every read of a `.__NAMESPACE__.` information environment through `$` or `[[`, and every
/// lexical `.__NAMESPACE__.` whose field is extracted.
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
            && unquoted(&ast_text(text, &expression)) == NAMESPACE_INFO
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

/// The receiver and member of `receiver$member` or `receiver[["member"]]`.
fn member_access(text: &str, expression: &AnyRExpression) -> Option<(AnyRExpression, String)> {
    match expression {
        AnyRExpression::RExtractExpression(extract)
            if extract.operator().ok()?.text_trimmed() == "$" =>
        {
            Some((
                extract.left().ok()?,
                unquoted(&ast_text(text, &extract.right().ok()?)),
            ))
        }
        AnyRExpression::RSubset2(subset) => {
            let mut arguments = subset.arguments().ok()?.items().iter();
            let index = arguments.next()?.ok()?.value()?;
            if arguments.next().is_some() {
                return None;
            }
            match static_arg(ast_text(text, &index).trim())? {
                StaticArg::String(member) => Some((subset.function().ok()?, member)),
                StaticArg::Symbol(_) => None,
            }
        }
        _ => None,
    }
}

/// The member extracted from `expression` by the expression that immediately contains it.
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
        let callee = ast_text(text, &function);
        matches!(
            callee.trim_start_matches("base::"),
            "asNamespace" | "getNamespace"
        )
    });
    match sole_positional_argument(call)
        .and_then(|argument| static_arg(ast_text(text, &argument).trim()))
    {
        Some(StaticArg::String(package)) if named => NamespaceInfoReceiver::Namespace(package),
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
        // Oak currently treats a `for` induction definition as sufficient to
        // bind a later use. R has a zero-iteration path where that assignment
        // never happens. Put that path back before applying positive slinker
        // refinements. A pre-existing definitely executed binding still makes
        // the post-loop use safe.
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
    let data_masks = data_mask_ranges(root, context);
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
                    format!("{}<-", live_use.name)
                } else {
                    live_use.name.clone()
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
                callee: live_use.name.clone(),
                callee_kind: live_use.callee_kind,
                qualified_package: None,
                args,
                arg_names: raw
                    .args
                    .iter()
                    .map(|argument| argument.name.clone())
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
            package: access.package().to_owned(),
            symbol: access.symbol().to_owned(),
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
                    callee: access.symbol().to_owned(),
                    callee_kind: CalleeKind::DefinitelyExternal,
                    qualified_package: Some(access.package().to_owned()),
                    args,
                    arg_names: raw
                        .args
                        .iter()
                        .map(|argument| argument.name.clone())
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
                name: operator.clone(),
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
                callee: operator.clone(),
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
    suppressed_reference_spans: &[(String, usize, usize)],
) {
    let is_suppressed = |reference: &NameRef, (name, start, end): &(String, usize, usize)| {
        reference.name == *name && reference.span.start == *start && reference.span.end == *end
    };
    for effect in effects.iter_mut() {
        let span = &effect.span;
        let free = |symbol: &String| {
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

fn collect_construction(
    source: SourceId,
    text: &str,
    root: &RRoot,
    calls: &[LiveCall],
) -> (Vec<String>, Vec<ConstructionExpr>) {
    let Some(function) = root
        .expressions()
        .iter()
        .find_map(|expression| outer_function(&expression))
    else {
        return (Vec::new(), Vec::new());
    };
    let parameters = function
        .parameters()
        .ok()
        .into_iter()
        .flat_map(|parameters| parameters.items().iter().collect::<Vec<_>>())
        .filter_map(std::result::Result::ok)
        .filter_map(|parameter| parameter.name().ok())
        .map(|name| ast_text(text, &name))
        .collect();
    let construction = function
        .body()
        .ok()
        .map(|body| construction_statements(&source, text, body, calls))
        .unwrap_or_default();
    (parameters, construction)
}

fn outer_function(expression: &AnyRExpression) -> Option<air_r_syntax::RFunctionDefinition> {
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

fn construction_statements(
    source: &SourceId,
    text: &str,
    expression: AnyRExpression,
    calls: &[LiveCall],
) -> Vec<ConstructionExpr> {
    match expression {
        AnyRExpression::RBracedExpressions(block) => block
            .expressions()
            .iter()
            .filter_map(|expression| construction_expr(source, text, expression, calls))
            .collect(),
        expression => construction_expr(source, text, expression, calls)
            .into_iter()
            .collect(),
    }
}

fn construction_expr(
    source: &SourceId,
    text: &str,
    expression: AnyRExpression,
    calls: &[LiveCall],
) -> Option<ConstructionExpr> {
    let span = ast_span(source, &expression);
    let kind = match expression {
        AnyRExpression::RNullExpression(_) => ConstructionExprKind::Null,
        AnyRExpression::RTrueExpression(_) => ConstructionExprKind::Logical { value: true },
        AnyRExpression::RFalseExpression(_) => ConstructionExprKind::Logical { value: false },
        AnyRExpression::RIdentifier(identifier) => ConstructionExprKind::Symbol {
            name: identifier.name_token().ok()?.text_trimmed().to_owned(),
        },
        AnyRExpression::AnyRValue(_) => construction_value(text.get(span.start..span.end)?.trim()),
        AnyRExpression::RBracedExpressions(block) => ConstructionExprKind::Sequence {
            expressions: block
                .expressions()
                .iter()
                .filter_map(|expression| construction_expr(source, text, expression, calls))
                .collect(),
        },
        AnyRExpression::RParenthesizedExpression(parenthesized) => {
            return construction_expr(source, text, parenthesized.body().ok()?, calls);
        }
        AnyRExpression::RBinaryExpression(binary) => {
            construction_binary(source, text, &binary, calls)?
        }
        AnyRExpression::RCall(call) => {
            return construction_call(source, text, &call, span, calls);
        }
        AnyRExpression::RExtractExpression(extract) => ConstructionExprKind::Member {
            object: Box::new(construction_expr(
                source,
                text,
                extract.left().ok()?,
                calls,
            )?),
            name: extract.right().ok().map(|name| ast_text(text, &name)),
        },
        AnyRExpression::RSubset2(subset) => {
            let index = subset
                .arguments()
                .ok()?
                .items()
                .iter()
                .find_map(std::result::Result::ok)?
                .value()?;
            construction_index(source, text, subset.function().ok()?, index, calls)?
        }
        AnyRExpression::RSubset(subset) => {
            let index = subset
                .arguments()
                .ok()?
                .items()
                .iter()
                .find_map(std::result::Result::ok)?
                .value()?;
            construction_index(source, text, subset.function().ok()?, index, calls)?
        }
        AnyRExpression::RUnaryExpression(unary) => ConstructionExprKind::Call {
            call: ConstructionCall {
                callee: unary.operator().ok()?.text_trimmed().to_owned(),
                callee_kind: CalleeKind::DefinitelyExternal,
                qualified_package: Some("base".into()),
                arguments: vec![ConstructionArgument {
                    name: None,
                    value: construction_expr(source, text, unary.argument().ok()?, calls),
                }],
            },
        },
        AnyRExpression::RIfStatement(statement) => {
            construction_if(source, text, &statement, calls)?
        }
        AnyRExpression::RFunctionDefinition(function) => {
            construction_function(source, text, &function, calls)?
        }
        _ => ConstructionExprKind::Unknown,
    };
    Some(ConstructionExpr { kind, span })
}

fn construction_value(value: &str) -> ConstructionExprKind {
    if let Some(StaticArg::String(value)) = static_arg(value) {
        ConstructionExprKind::String { value }
    } else if let Ok(integer) = value.strip_suffix('L').unwrap_or(value).parse::<i64>() {
        ConstructionExprKind::Integer { value: integer }
    } else {
        ConstructionExprKind::Double {
            value: value.to_owned(),
        }
    }
}

fn construction_binary(
    source: &SourceId,
    text: &str,
    binary: &RBinaryExpression,
    calls: &[LiveCall],
) -> Option<ConstructionExprKind> {
    let operator = binary.operator().ok()?.text_trimmed().to_owned();
    let left = binary.left().ok()?;
    let right = binary.right().ok()?;
    Some(if operator == "<-" || operator == "=" {
        ConstructionExprKind::Assign {
            target: construction_target(source, text, left, calls),
            value: Box::new(construction_expr(source, text, right, calls)?),
        }
    } else {
        ConstructionExprKind::Call {
            call: ConstructionCall {
                qualified_package: (!operator.starts_with('%')).then(|| "base".into()),
                callee: operator,
                callee_kind: CalleeKind::DefinitelyExternal,
                arguments: vec![
                    ConstructionArgument {
                        name: None,
                        value: construction_expr(source, text, left, calls),
                    },
                    ConstructionArgument {
                        name: None,
                        value: construction_expr(source, text, right, calls),
                    },
                ],
            },
        }
    })
}

fn construction_call(
    source: &SourceId,
    text: &str,
    call: &RCall,
    span: Span,
    calls: &[LiveCall],
) -> Option<ConstructionExpr> {
    let site = calls
        .iter()
        .find(|candidate| candidate.site.span == span)
        .map(|call| &call.site);
    let function = call.function().ok()?;
    if site.is_none() && matches!(function, AnyRExpression::RExtractExpression(_)) {
        return Some(ConstructionExpr {
            kind: ConstructionExprKind::Sequence {
                expressions: std::iter::once(function)
                    .chain(
                        call.arguments()
                            .ok()?
                            .items()
                            .iter()
                            .filter_map(|argument| argument.ok()?.value()),
                    )
                    .filter_map(|expression| construction_expr(source, text, expression, calls))
                    .collect(),
            },
            span,
        });
    }
    let callee = site.map_or_else(|| ast_text(text, &function), |site| site.callee.clone());
    let kind = ConstructionExprKind::Call {
        call: ConstructionCall {
            callee,
            callee_kind: site.map_or(CalleeKind::DefinitelyLexical, |site| site.callee_kind),
            qualified_package: site.and_then(|site| site.qualified_package.clone()),
            arguments: call
                .arguments()
                .ok()?
                .items()
                .iter()
                .filter_map(std::result::Result::ok)
                .map(|argument| ConstructionArgument {
                    name: argument
                        .name_clause()
                        .and_then(|clause| clause.name().ok())
                        .map(|name| ast_text(text, &name)),
                    value: argument
                        .value()
                        .and_then(|value| construction_expr(source, text, value, calls)),
                })
                .collect(),
        },
    };
    Some(ConstructionExpr { kind, span })
}

fn construction_index(
    source: &SourceId,
    text: &str,
    object: AnyRExpression,
    index: AnyRExpression,
    calls: &[LiveCall],
) -> Option<ConstructionExprKind> {
    Some(ConstructionExprKind::Index {
        object: Box::new(construction_expr(source, text, object, calls)?),
        index: Box::new(construction_expr(source, text, index, calls)?),
    })
}

fn construction_if(
    source: &SourceId,
    text: &str,
    statement: &RIfStatement,
    calls: &[LiveCall],
) -> Option<ConstructionExprKind> {
    Some(ConstructionExprKind::If {
        condition: Box::new(construction_expr(
            source,
            text,
            statement.condition().ok()?,
            calls,
        )?),
        consequence: Box::new(construction_expr(
            source,
            text,
            statement.consequence().ok()?,
            calls,
        )?),
        alternative: statement
            .else_clause()
            .and_then(|clause| clause.alternative().ok())
            .and_then(|alternative| construction_expr(source, text, alternative, calls))
            .map(Box::new),
    })
}

fn construction_function(
    source: &SourceId,
    text: &str,
    function: &RFunctionDefinition,
    calls: &[LiveCall],
) -> Option<ConstructionExprKind> {
    Some(ConstructionExprKind::Function {
        parameters: function
            .parameters()
            .ok()?
            .items()
            .iter()
            .filter_map(std::result::Result::ok)
            .filter_map(|parameter| parameter.name().ok())
            .map(|name| ast_text(text, &name))
            .collect(),
        body: Box::new(construction_expr(
            source,
            text,
            function.body().ok()?,
            calls,
        )?),
    })
}

fn construction_target(
    source: &SourceId,
    text: &str,
    target: AnyRExpression,
    calls: &[LiveCall],
) -> ConstructionTarget {
    match target {
        AnyRExpression::RIdentifier(identifier) => {
            identifier
                .name_token()
                .ok()
                .map_or(ConstructionTarget::Unknown, |name| {
                    ConstructionTarget::Local {
                        name: name.text_trimmed().to_owned(),
                    }
                })
        }
        AnyRExpression::RExtractExpression(extract) => ConstructionTarget::Member {
            object: Box::new(
                extract
                    .left()
                    .ok()
                    .and_then(|object| construction_expr(source, text, object, calls))
                    .unwrap_or_else(|| unknown_construction(source, &extract)),
            ),
            name: extract.right().ok().map(|name| ast_text(text, &name)),
        },
        AnyRExpression::RSubset2(subset) => {
            let name = subset
                .arguments()
                .ok()
                .and_then(|arguments| {
                    arguments
                        .items()
                        .iter()
                        .filter_map(std::result::Result::ok)
                        .next()
                })
                .and_then(|argument| argument.value())
                .and_then(|value| construction_expr(source, text, value, calls))
                .and_then(|value| match value.kind {
                    ConstructionExprKind::String { value } => Some(value),
                    _ => None,
                });
            ConstructionTarget::Member {
                object: Box::new(
                    subset
                        .function()
                        .ok()
                        .and_then(|object| construction_expr(source, text, object, calls))
                        .unwrap_or_else(|| unknown_construction(source, &subset)),
                ),
                name,
            }
        }
        AnyRExpression::RCall(call) => {
            let callee = call.function().ok().map(|callee| ast_text(text, &callee));
            if callee.as_deref() != Some("environment") {
                return ConstructionTarget::Unknown;
            }
            let closure = call
                .arguments()
                .ok()
                .and_then(|arguments| {
                    arguments
                        .items()
                        .iter()
                        .filter_map(std::result::Result::ok)
                        .next()
                })
                .and_then(|argument| argument.value())
                .and_then(|closure| construction_expr(source, text, closure, calls));
            closure.map_or(ConstructionTarget::Unknown, |closure| {
                ConstructionTarget::ClosureEnvironment {
                    closure: Box::new(closure),
                }
            })
        }
        _ => ConstructionTarget::Unknown,
    }
}

fn ast_span(source: &SourceId, node: &impl AstNode<Language = air_r_syntax::RLanguage>) -> Span {
    let range = node.syntax().text_trimmed_range();
    Span::new(
        *source,
        text_offset(range.start()),
        text_offset(range.end()),
    )
}

fn ast_text(text: &str, node: &impl AstNode<Language = air_r_syntax::RLanguage>) -> String {
    let span = node.syntax().text_trimmed_range();
    text[text_offset(span.start())..text_offset(span.end())].to_owned()
}

fn unknown_construction(
    source: &SourceId,
    node: &impl AstNode<Language = air_r_syntax::RLanguage>,
) -> ConstructionExpr {
    ConstructionExpr {
        kind: ConstructionExprKind::Unknown,
        span: ast_span(source, node),
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

fn formal_default_use_is_bound(regions: &[FunctionRegion], live_use: &LiveUse) -> bool {
    let mut containing = regions
        .iter()
        .filter(|region| {
            region
                .formals
                .contains_range(TextRange::new(live_use.start, live_use.end))
        })
        .collect::<Vec<_>>();
    containing.sort_by_key(|region| region.formals.end.saturating_sub(region.formals.start));
    containing
        .into_iter()
        .any(|region| region.parameters.contains(&live_use.name))
}

fn for_body_use_is_bound(index: &SemanticIndex, regions: &[ForRegion], live_use: &LiveUse) -> bool {
    if !regions.iter().any(|region| {
        region.variable == live_use.name
            && region
                .body
                .contains_range(TextRange::new(live_use.start, live_use.end))
    }) {
        return false;
    }

    index
        .reaching_definitions(live_use.scope, live_use.use_id)
        .any(|(scope, definition_id)| {
            let definition = &index.definitions(scope)[definition_id];
            let symbol = index.symbols(scope).symbol(definition.symbol());
            symbol.name() == live_use.name
                && matches!(definition.kind(), DefinitionKind::ForVariable(_))
        })
}

fn recursive_closure_binding_is_initialized(
    text: &str,
    index: &SemanticIndex,
    functions: &[FunctionRegion],
    live_use: &LiveUse,
) -> bool {
    let Some(function) = innermost_function_region(functions, live_use.start) else {
        return false;
    };
    index
        .reaching_definitions(live_use.scope, live_use.use_id)
        .filter(|(scope, _)| *scope != live_use.scope)
        .any(|(scope, definition_id)| {
            let definition = &index.definitions(scope)[definition_id];
            if !matches!(definition.kind(), DefinitionKind::Assignment(_)) {
                return false;
            }
            let symbol = index.symbols(scope).symbol(definition.symbol());
            if symbol.name() != live_use.name {
                return false;
            }
            let target_end = text_offset(definition.range().end());
            assignment_rhs_after(text, target_end, "<-")
                .is_some_and(|(start, _)| start == function.function_start)
        })
}

/// Whether a use after a `for` loop must retain the loop's zero-iteration
/// fallthrough. Oak's current use-def map can report the induction variable as
/// definitely bound after the loop, but R assigns it only when an iteration
/// actually begins.
fn post_for_use_may_fall_through(
    text: &str,
    index: &SemanticIndex,
    regions: &[ForRegion],
    if_regions: &[IfRegion],
    live_use: &LiveUse,
) -> bool {
    for region in regions.iter().rev() {
        if region.variable != live_use.name || region.body.end > live_use.start {
            continue;
        }

        let has_reaching_for_definition = index
            .reaching_definitions(live_use.scope, live_use.use_id)
            .any(|(scope, definition_id)| {
                if scope != live_use.scope {
                    return false;
                }
                let definition = &index.definitions(scope)[definition_id];
                let symbol = index.symbols(scope).symbol(definition.symbol());
                symbol.name() == live_use.name
                    && matches!(definition.kind(), DefinitionKind::ForVariable(_))
            });
        if !has_reaching_for_definition {
            continue;
        }

        if scope_has_definite_binding_before(
            text,
            index,
            live_use.scope,
            &live_use.name,
            region.variable_start,
            live_use.start,
            ControlRegions {
                for_regions: regions,
                if_regions,
            },
        ) {
            return false;
        }

        return true;
    }

    false
}

fn scope_has_definite_binding_before(
    text: &str,
    index: &SemanticIndex,
    scope: ScopeId,
    name: &str,
    before: usize,
    use_position: usize,
    regions: ControlRegions<'_>,
) -> bool {
    let Some(symbol_id) = index.symbols(scope).id(name) else {
        return false;
    };

    index
        .definitions(scope)
        .iter()
        .any(|(_definition_id, definition)| {
            if definition.symbol() != symbol_id {
                return false;
            }
            match definition.kind() {
                DefinitionKind::Parameter(_) => true,
                DefinitionKind::Assignment(_) | DefinitionKind::Assign { .. } => {
                    let definition_start = text_offset(definition.range().start());
                    definition_start < before
                        && definition_must_execute_before_position(
                            text,
                            definition_start,
                            use_position,
                            regions.for_regions,
                            regions.if_regions,
                        )
                }
                DefinitionKind::ForVariable(_)
                | DefinitionKind::SuperAssignment(_)
                | DefinitionKind::Import { .. } => false,
            }
        })
}

/// A deliberately structural dominance check used only for local bindings that
/// Oak has already identified. It does not discover names. A definition is
/// accepted when every enclosing branch/loop containing the definition also
/// contains the later position, so reaching the later position implies that
/// the definition's control region was entered.
fn definition_must_execute_before_position(
    _text: &str,
    definition_start: usize,
    position: usize,
    for_regions: &[ForRegion],
    if_regions: &[IfRegion],
) -> bool {
    if definition_start >= position {
        return false;
    }

    for region in for_regions {
        if definition_start >= region.body.start
            && definition_start < region.body.end
            && !(position >= region.body.start && position < region.body.end)
        {
            return false;
        }
    }

    for region in if_regions {
        let in_then = definition_start >= region.then_branch.start
            && definition_start < region.then_branch.end;
        if in_then && !(position >= region.then_branch.start && position < region.then_branch.end) {
            return false;
        }
        if let Some(TextRange {
            start: else_start,
            end: else_end,
        }) = region.else_branch
        {
            let in_else = definition_start >= else_start && definition_start < else_end;
            if in_else && !(position >= else_start && position < else_end) {
                return false;
            }
        }
    }

    true
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BranchAssumption {
    condition: String,
    truth: bool,
    symbols: BTreeSet<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum PredicateValue {
    String(String),
    Logical(bool),
    Number(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum SimplePredicate {
    Eq {
        symbol: String,
        value: PredicateValue,
    },
    Ne {
        symbol: String,
        value: PredicateValue,
    },
    IsNull {
        value: String,
        is_null: bool,
    },
    Static(bool),
}

/// Refine one of Oak's conservative conditional fallthroughs only when the
/// source proves that every path reaching this use has already executed a
/// reaching local definition. Oak still supplies the live use and the exact
/// reaching-definition set. This pass adds a deliberately small path domain
/// for stable scalar predicates plus definitely terminating branches.
fn conditional_fallthrough_proven_bound(
    text: &str,
    context: &OakParseContext,
    index: &SemanticIndex,
    for_regions: &[ForRegion],
    regions: &[IfRegion],
    live_use: &LiveUse,
) -> bool {
    let reaching = index
        .reaching_definitions(live_use.scope, live_use.use_id)
        .filter_map(|(scope, definition_id)| {
            let definition = &index.definitions(scope)[definition_id];
            let symbol = index.symbols(scope).symbol(definition.symbol());
            let definition_start = text_offset(definition.range().start());
            if symbol.name() != live_use.name
                || !matches!(definition.kind(), DefinitionKind::Assignment(_))
            {
                return None;
            }
            if scope != live_use.scope {
                let assumptions = branch_assumptions_at(text, regions, definition_start);
                let symbols = assumption_symbols(&assumptions);
                if !captured_condition_symbols_stable(
                    text,
                    index,
                    scope,
                    definition_start,
                    text.len(),
                    &symbols,
                ) {
                    return None;
                }
            }
            Some((definition, definition_start, scope))
        })
        .filter(|(_, start, _)| *start < live_use.start)
        .collect::<Vec<_>>();

    if reaching.is_empty() {
        return false;
    }

    if reaching.iter().any(|(_, definition_start, _)| {
        definition_must_execute_before_position(
            text,
            *definition_start,
            live_use.start,
            for_regions,
            regions,
        )
    }) {
        return true;
    }

    let mut use_assumptions = branch_assumptions_at(text, regions, live_use.start);
    expand_boolean_alias_assumptions(
        text,
        index,
        live_use.scope,
        live_use.start,
        &mut use_assumptions,
    );

    // Correlate a repeated stable predicate, including equivalent negations
    // such as an `else` of `x == "a"` and a later `x != "a"` branch.
    for (_definition, definition_start, defining_scope) in &reaching {
        if !definition_is_direct_in_branch(text, regions, *definition_start) {
            continue;
        }
        let definition_assumptions = branch_assumptions_at(text, regions, *definition_start);
        if definition_assumptions.is_empty()
            || !assumptions_imply(&use_assumptions, &definition_assumptions)
            || !assumptions_are_repeatable(context, index, live_use.scope, &definition_assumptions)
        {
            continue;
        }
        let symbols = assumption_symbols(&definition_assumptions);
        if condition_facts_stable(
            text,
            index,
            live_use.scope,
            *defining_scope,
            *definition_start,
            live_use.start,
            &symbols,
        ) {
            return true;
        }
    }

    let definition_starts = reaching
        .iter()
        .map(|(_, start, _)| *start)
        .collect::<Vec<_>>();
    let defining_scope = reaching.first().map(|(_, _, scope)| *scope);

    // Prove an earlier exhaustive dispatch. A chain is sufficient only when
    // each path that can continue after it has a direct reaching assignment to
    // this name; a path may omit the assignment only if it exits the function
    // or invokes a callee already proved non-returning.
    for region in regions {
        let Some(TextRange { end: chain_end, .. }) = region.else_branch else {
            continue;
        };
        if chain_end > live_use.start || region.if_start >= live_use.start {
            continue;
        }

        let chain_assumptions = branch_assumptions_at(text, regions, region.if_start);
        if !assumptions_imply(&use_assumptions, &chain_assumptions) {
            continue;
        }
        let symbols = assumption_symbols(&chain_assumptions);
        if !defining_scope.is_some_and(|scope| {
            condition_facts_stable(
                text,
                index,
                live_use.scope,
                scope,
                region.if_start,
                live_use.start,
                &symbols,
            )
        }) {
            continue;
        }

        if if_chain_all_returning_paths_bind(
            text,
            context,
            index,
            regions,
            region,
            &definition_starts,
        ) {
            return true;
        }
    }

    // Preserve the finite alternatives of an earlier exhaustive equality
    // dispatch instead of collapsing them to one generic "maybe assigned"
    // state. This is the ISCAM shape where a later branch re-tests the same
    // unchanged selector and therefore rules out earlier arms.
    for region in regions {
        let Some(TextRange { end: chain_end, .. }) = region.else_branch else {
            continue;
        };
        if chain_end > live_use.start || region.if_start >= live_use.start {
            continue;
        }
        if exhaustive_equality_dispatch_proves_binding(
            text,
            context,
            index,
            regions,
            region,
            BindingProofContext {
                live_use,
                use_assumptions: &use_assumptions,
                definition_starts: &definition_starts,
                defining_scope,
            },
        ) {
            return true;
        }
    }

    false
}

fn branch_assumptions_at(
    text: &str,
    regions: &[IfRegion],
    position: usize,
) -> Vec<BranchAssumption> {
    let mut assumptions = Vec::new();
    for region in regions {
        let truth = if position >= region.then_branch.start && position < region.then_branch.end {
            Some(true)
        } else if let Some(TextRange { start, end }) = region.else_branch {
            (position >= start && position < end).then_some(false)
        } else {
            None
        };
        let Some(truth) = truth else {
            continue;
        };
        assumptions.push(BranchAssumption {
            condition: canonical_condition(text, region.condition.start, region.condition.end),
            truth,
            symbols: condition_symbols(text, region.condition.start, region.condition.end),
        });
    }
    assumptions
}

fn assumptions_imply(known: &[BranchAssumption], required: &[BranchAssumption]) -> bool {
    required.iter().all(|required| {
        let Some(required) = effective_predicate(required) else {
            return false;
        };
        known
            .iter()
            .filter_map(effective_predicate)
            .any(|known| known == required)
    })
}

fn assumptions_are_repeatable(
    context: &OakParseContext,
    index: &SemanticIndex,
    scope: ScopeId,
    assumptions: &[BranchAssumption],
) -> bool {
    assumptions.iter().all(|assumption| {
        let Some(predicate) = effective_predicate(assumption) else {
            return false;
        };
        predicate_is_repeatable(context, index, scope, &predicate)
    })
}

fn effective_predicate(assumption: &BranchAssumption) -> Option<SimplePredicate> {
    let predicate = parse_simple_predicate(&assumption.condition)?;
    if assumption.truth {
        Some(predicate)
    } else {
        Some(negate_predicate(predicate))
    }
}

fn expand_boolean_alias_assumptions(
    text: &str,
    index: &SemanticIndex,
    scope: ScopeId,
    position: usize,
    assumptions: &mut Vec<BranchAssumption>,
) {
    let aliases = assumptions
        .iter()
        .filter_map(effective_predicate)
        .filter_map(|predicate| match predicate {
            SimplePredicate::Eq {
                symbol,
                value: PredicateValue::Logical(value),
            } => Some((symbol, value)),
            _ => None,
        })
        .collect::<Vec<_>>();
    for (symbol, truth) in aliases {
        let Some(symbol_id) = index.symbols(scope).id(&symbol) else {
            continue;
        };
        let rhs = index
            .definitions(scope)
            .iter()
            .filter(|(_, definition)| {
                definition.symbol() == symbol_id
                    && matches!(definition.kind(), DefinitionKind::Assignment(_))
                    && text_offset(definition.range().start()) < position
            })
            .filter_map(|(_, definition)| {
                assignment_rhs_after(text, text_offset(definition.range().end()), "<-")
            })
            .max_by_key(|(start, _)| *start)
            .and_then(|(start, end)| text.get(start..end));
        let Some(rhs) = rhs.filter(|rhs| parse_simple_predicate(rhs).is_some()) else {
            continue;
        };
        assumptions.push(BranchAssumption {
            condition: rhs.to_owned(),
            truth,
            symbols: condition_symbols(rhs, 0, rhs.len()),
        });
    }
}

fn predicate_is_repeatable(
    context: &OakParseContext,
    index: &SemanticIndex,
    scope: ScopeId,
    predicate: &SimplePredicate,
) -> bool {
    match predicate {
        SimplePredicate::Eq { .. } | SimplePredicate::Ne { .. } | SimplePredicate::Static(_) => {
            true
        }
        SimplePredicate::IsNull { .. } => {
            context.resolves_to_base("is.null") && index.resolve("is.null", scope).is_none()
        }
    }
}

fn negate_predicate(predicate: SimplePredicate) -> SimplePredicate {
    match predicate {
        SimplePredicate::Eq { symbol, value } => SimplePredicate::Ne { symbol, value },
        SimplePredicate::Ne { symbol, value } => SimplePredicate::Eq { symbol, value },
        SimplePredicate::IsNull { value, is_null } => SimplePredicate::IsNull {
            value,
            is_null: !is_null,
        },
        SimplePredicate::Static(value) => SimplePredicate::Static(!value),
    }
}

fn parse_simple_predicate(condition: &str) -> Option<SimplePredicate> {
    let condition = strip_outer_parentheses(condition);
    if condition == "TRUE" {
        return Some(SimplePredicate::Static(true));
    }
    if condition == "FALSE" {
        return Some(SimplePredicate::Static(false));
    }
    if let Some(symbol) = condition.strip_prefix('!').and_then(static_symbol) {
        return Some(SimplePredicate::Eq {
            symbol,
            value: PredicateValue::Logical(false),
        });
    }
    if let Some(inner) = condition
        .strip_prefix("!is.null(")
        .and_then(|rest| rest.strip_suffix(')'))
    {
        let value = strip_outer_parentheses(inner.trim());
        return (!value.is_empty()).then(|| SimplePredicate::IsNull {
            value: value.to_owned(),
            is_null: false,
        });
    }
    if let Some(inner) = condition
        .strip_prefix("is.null(")
        .and_then(|rest| rest.strip_suffix(')'))
    {
        let value = strip_outer_parentheses(inner.trim());
        return (!value.is_empty()).then(|| SimplePredicate::IsNull {
            value: value.to_owned(),
            is_null: true,
        });
    }
    if let Some((left, right)) = split_top_level_operator(condition, "==") {
        let (symbol, value) = symbol_constant_pair(left, right)?;
        return Some(SimplePredicate::Eq { symbol, value });
    }
    if let Some((left, right)) = split_top_level_operator(condition, "!=") {
        let (symbol, value) = symbol_constant_pair(left, right)?;
        return Some(SimplePredicate::Ne { symbol, value });
    }
    static_symbol(condition).map(|symbol| SimplePredicate::Eq {
        symbol,
        value: PredicateValue::Logical(true),
    })
}

fn strip_outer_parentheses(mut value: &str) -> &str {
    loop {
        if !value.starts_with('(') || !value.ends_with(')') {
            return value;
        }
        let Some(close) = matching_delimiter(value, 0) else {
            return value;
        };
        if close + 1 != value.len() {
            return value;
        }
        value = &value[1..value.len() - 1];
    }
}

fn split_top_level_operator<'a>(value: &'a str, operator: &str) -> Option<(&'a str, &'a str)> {
    let bytes = value.as_bytes();
    let operator_bytes = operator.as_bytes();
    let mut cursor = 0usize;
    let mut depth = 0usize;
    let mut quote = None;
    while cursor < bytes.len() {
        let byte = bytes[cursor];
        if let Some(delimiter) = quote {
            if byte == b'\\' {
                cursor = (cursor + 2).min(bytes.len());
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
            b'(' => {
                depth += 1;
                cursor += 1;
            }
            b')' => {
                depth = depth.saturating_sub(1);
                cursor += 1;
            }
            _ if depth == 0
                && cursor + operator_bytes.len() <= bytes.len()
                && &bytes[cursor..cursor + operator_bytes.len()] == operator_bytes =>
            {
                return Some((&value[..cursor], &value[cursor + operator_bytes.len()..]));
            }
            _ => cursor += 1,
        }
    }
    None
}

fn symbol_constant_pair(left: &str, right: &str) -> Option<(String, PredicateValue)> {
    if let (Some(symbol), Some(value)) = (static_symbol(left), predicate_constant(right)) {
        return Some((symbol, value));
    }
    if let (Some(value), Some(symbol)) = (predicate_constant(left), static_symbol(right)) {
        return Some((symbol, value));
    }
    None
}

fn predicate_constant(value: &str) -> Option<PredicateValue> {
    if let Some(value) = static_string(value) {
        return Some(PredicateValue::String(value));
    }
    match value {
        "TRUE" => Some(PredicateValue::Logical(true)),
        "FALSE" => Some(PredicateValue::Logical(false)),
        _ if simple_numeric_literal(value) => Some(PredicateValue::Number(value.to_owned())),
        _ => None,
    }
}

fn simple_numeric_literal(value: &str) -> bool {
    let value = value.strip_suffix('L').unwrap_or(value);
    !value.is_empty() && value.parse::<f64>().is_ok()
}

fn exhaustive_equality_dispatch_proves_binding(
    text: &str,
    context: &OakParseContext,
    index: &SemanticIndex,
    regions: &[IfRegion],
    region: &IfRegion,
    proof: BindingProofContext<'_>,
) -> bool {
    let BindingProofContext {
        live_use,
        use_assumptions,
        definition_starts,
        defining_scope,
    } = proof;
    let mut current = region;
    let mut selector = None::<String>;
    let mut cases = Vec::<(PredicateValue, usize, usize)>::new();
    let final_else = loop {
        let condition = canonical_condition(text, current.condition.start, current.condition.end);
        let Some(SimplePredicate::Eq { symbol, value }) = parse_simple_predicate(&condition) else {
            return false;
        };
        if selector
            .as_ref()
            .is_some_and(|existing| existing != &symbol)
        {
            return false;
        }
        selector.get_or_insert(symbol);
        if cases.iter().any(|(existing, _, _)| existing == &value) {
            return false;
        }
        cases.push((value, current.then_branch.start, current.then_branch.end));

        let Some(TextRange {
            start: else_start,
            end: else_end,
        }) = current.else_branch
        else {
            break None;
        };
        if let Some(nested) = regions
            .iter()
            .find(|candidate| candidate.if_start == else_start)
        {
            current = nested;
            continue;
        }
        break Some((else_start, else_end));
    };

    let selector = selector.expect("equality chain has at least one selector");
    match final_else {
        Some((start, end)) => {
            if !branch_exits_current_function(text, context, index, regions, start, end) {
                return false;
            }
        }
        None => {
            let Some(allowed) = prior_membership_guard_values(
                text,
                context,
                index,
                regions,
                region.if_start,
                &selector,
            ) else {
                return false;
            };
            let covered = cases
                .iter()
                .filter_map(|(value, _, _)| match value {
                    PredicateValue::String(value) => Some(value.clone()),
                    PredicateValue::Logical(_) | PredicateValue::Number(_) => None,
                })
                .collect::<BTreeSet<_>>();
            if covered != allowed {
                return false;
            }
        }
    }

    if !defining_scope.is_some_and(|scope| {
        condition_facts_stable(
            text,
            index,
            live_use.scope,
            scope,
            region.if_start,
            live_use.start,
            &BTreeSet::from([selector.clone()]),
        )
    }) {
        return false;
    }

    let known = use_assumptions
        .iter()
        .filter_map(effective_predicate)
        .collect::<Vec<_>>();
    let mut possible_case = false;
    for (value, branch_start, branch_end) in cases {
        if !case_is_consistent_with(&selector, &value, &known) {
            continue;
        }
        possible_case = true;
        if !branch_all_paths_bind_or_exit(
            text,
            context,
            index,
            regions,
            branch_start,
            branch_end,
            definition_starts,
        ) {
            return false;
        }
    }
    possible_case
}

fn branch_all_paths_bind_or_exit(
    text: &str,
    context: &OakParseContext,
    index: &SemanticIndex,
    regions: &[IfRegion],
    start: usize,
    end: usize,
    definition_starts: &[usize],
) -> bool {
    branch_binds_reaching_definition(text, regions, start, end, definition_starts)
        || branch_exits_current_function(text, context, index, regions, start, end)
        || regions.iter().any(|nested| {
            nested.if_start >= start
                && nested.if_start < end
                && definition_is_top_level_in_branch(text, start, end, nested.if_start)
                && if_chain_all_returning_paths_bind(
                    text,
                    context,
                    index,
                    regions,
                    nested,
                    definition_starts,
                )
        })
}

fn prior_membership_guard_values(
    text: &str,
    context: &OakParseContext,
    index: &SemanticIndex,
    regions: &[IfRegion],
    before: usize,
    selector: &str,
) -> Option<BTreeSet<String>> {
    regions
        .iter()
        .filter(|region| region.if_start < before && region.then_branch.end <= before)
        .rev()
        .find_map(|region| {
            if !branch_exits_current_function(
                text,
                context,
                index,
                regions,
                region.then_branch.start,
                region.then_branch.end,
            ) {
                return None;
            }
            let condition = canonical_condition(text, region.condition.start, region.condition.end);
            let marker = format!("!({selector}%in%c(");
            let marker_start = condition.find(&marker)?;
            let open = marker_start + marker.len() - 1;
            let close = matching_delimiter(&condition, open)?;
            let values = split_arguments(&condition, open + 1, close)
                .into_iter()
                .map(|argument| match argument.static_arg {
                    Some(StaticArg::String(value)) => Some(value),
                    Some(StaticArg::Symbol(_)) | None => None,
                })
                .collect::<Option<BTreeSet<_>>>()?;
            (!values.is_empty()).then_some(values)
        })
}

fn case_is_consistent_with(
    selector: &str,
    value: &PredicateValue,
    predicates: &[SimplePredicate],
) -> bool {
    for predicate in predicates {
        match predicate {
            SimplePredicate::Eq {
                symbol,
                value: required,
            } if symbol == selector => {
                if required != value {
                    return false;
                }
            }
            SimplePredicate::Ne {
                symbol,
                value: excluded,
            } if symbol == selector => {
                if excluded == value {
                    return false;
                }
            }
            SimplePredicate::Static(false) => return false,
            _ => {}
        }
    }
    true
}

fn assumption_symbols(assumptions: &[BranchAssumption]) -> BTreeSet<String> {
    assumptions
        .iter()
        .flat_map(|assumption| assumption.symbols.iter().cloned())
        .collect()
}

fn canonical_condition(text: &str, start: usize, end: usize) -> String {
    let bytes = text.as_bytes();
    let mut output = String::new();
    let mut cursor = start;
    let mut quote = None;
    while cursor < end {
        let byte = bytes[cursor];
        if let Some(delimiter) = quote {
            output.push(byte as char);
            if byte == b'\\' {
                if cursor + 1 < end {
                    cursor += 1;
                    output.push(bytes[cursor] as char);
                }
            } else if byte == delimiter {
                quote = None;
            }
            cursor += 1;
            continue;
        }
        match byte {
            b'\'' | b'"' | b'`' => {
                quote = Some(byte);
                output.push(byte as char);
                cursor += 1;
            }
            b'#' => cursor = skip_comment(text, cursor, end),
            byte if byte.is_ascii_whitespace() => cursor += 1,
            _ => {
                let character = text[cursor..].chars().next().expect("valid UTF-8 source");
                output.push(character);
                cursor += character.len_utf8();
            }
        }
    }
    output
}

fn condition_symbols(text: &str, start: usize, end: usize) -> BTreeSet<String> {
    let mut symbols = BTreeSet::new();
    let bytes = text.as_bytes();
    let mut cursor = start;
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
            b'#' => cursor = skip_comment(text, cursor, end),
            _ => {
                let Some(character) = text[cursor..].chars().next() else {
                    break;
                };
                if character.is_alphabetic() || character == '.' || character == '_' {
                    let token_start = cursor;
                    cursor += character.len_utf8();
                    while cursor < end {
                        let Some(next) = text[cursor..].chars().next() else {
                            break;
                        };
                        if !(next.is_alphanumeric() || next == '.' || next == '_') {
                            break;
                        }
                        cursor += next.len_utf8();
                    }
                    if let Some(name) = text.get(token_start..cursor) {
                        symbols.insert(name.to_owned());
                    }
                } else {
                    cursor += character.len_utf8();
                }
            }
        }
    }
    symbols
}

fn condition_symbols_stable(
    text: &str,
    index: &SemanticIndex,
    start: usize,
    end: usize,
    symbols: &BTreeSet<String>,
) -> bool {
    if start >= end || symbols.is_empty() {
        return true;
    }

    // Invalidate a predicate fact if any syntactic mutation of one of its
    // symbols appears between the two sites. Checking descendant scopes as
    // well is conservative: defining a nested closure does not itself execute
    // its assignments, but refusing a proof is safer than assuming the closure
    // cannot run and mutate a captured binding before the repeated predicate.
    for definition_scope in index.scope_ids() {
        for (_, definition) in index.definitions(definition_scope).iter() {
            if !matches!(
                definition.kind(),
                DefinitionKind::Assignment(_)
                    | DefinitionKind::SuperAssignment(_)
                    | DefinitionKind::ForVariable(_)
                    | DefinitionKind::Assign { .. }
            ) {
                continue;
            }
            let definition_start = text_offset(definition.range().start());
            if definition_start <= start || definition_start >= end {
                continue;
            }
            let name = index
                .symbols(definition_scope)
                .symbol(definition.symbol())
                .name();
            if symbols.contains(name) {
                return false;
            }
        }
    }

    // `rm()` / `remove()` can destroy a binding without creating an Oak
    // definition. Refuse the correlation proof if either appears between the
    // two sites.
    let segment = text.get(start..end).unwrap_or_default();
    !contains_call_named(segment, "rm") && !contains_call_named(segment, "remove")
}

fn condition_facts_stable(
    text: &str,
    index: &SemanticIndex,
    use_scope: ScopeId,
    defining_scope: ScopeId,
    start: usize,
    end: usize,
    symbols: &BTreeSet<String>,
) -> bool {
    if use_scope == defining_scope {
        condition_symbols_stable(text, index, start, end, symbols)
    } else {
        captured_condition_symbols_stable(text, index, defining_scope, start, end, symbols)
    }
}

fn captured_condition_symbols_stable(
    text: &str,
    index: &SemanticIndex,
    defining_scope: ScopeId,
    start: usize,
    end: usize,
    symbols: &BTreeSet<String>,
) -> bool {
    if start >= end || symbols.is_empty() {
        return true;
    }
    for scope in index.scope_ids() {
        for (_, definition) in index.definitions(scope).iter() {
            let mutates_capture = matches!(definition.kind(), DefinitionKind::SuperAssignment(_))
                || (scope == defining_scope
                    && matches!(
                        definition.kind(),
                        DefinitionKind::Assignment(_)
                            | DefinitionKind::ForVariable(_)
                            | DefinitionKind::Assign { .. }
                    ));
            if !mutates_capture {
                continue;
            }
            let position = text_offset(definition.range().start());
            if position <= start || position >= end {
                continue;
            }
            let name = index.symbols(scope).symbol(definition.symbol()).name();
            if symbols.contains(name) {
                return false;
            }
        }
    }
    let segment = text.get(start..end).unwrap_or_default();
    !contains_call_named(segment, "rm") && !contains_call_named(segment, "remove")
}

fn contains_call_named(text: &str, name: &str) -> bool {
    let mut offset = 0;
    while let Some(relative) = text[offset..].find(name) {
        let start = offset + relative;
        let end = start + name.len();
        if word_boundary_before(text, start)
            && word_boundary_after(text, end)
            && text.as_bytes().get(skip_trivia(text, end)).copied() == Some(b'(')
        {
            return true;
        }
        offset = end;
        if offset >= text.len() {
            break;
        }
    }
    false
}

fn definition_is_direct_in_branch(
    text: &str,
    regions: &[IfRegion],
    definition_start: usize,
) -> bool {
    regions
        .iter()
        .filter_map(|region| {
            if definition_start >= region.then_branch.start
                && definition_start < region.then_branch.end
            {
                Some((region.then_branch.start, region.then_branch.end))
            } else if let Some(TextRange { start, end }) = region.else_branch {
                (definition_start >= start && definition_start < end).then_some((start, end))
            } else {
                None
            }
        })
        .min_by_key(|(start, end)| end.saturating_sub(*start))
        .is_some_and(|(start, end)| {
            definition_is_top_level_in_branch(text, start, end, definition_start)
        })
}

fn if_chain_all_returning_paths_bind(
    text: &str,
    context: &OakParseContext,
    index: &SemanticIndex,
    regions: &[IfRegion],
    region: &IfRegion,
    definition_starts: &[usize],
) -> bool {
    if !branch_binds_reaching_definition(
        text,
        regions,
        region.then_branch.start,
        region.then_branch.end,
        definition_starts,
    ) && !branch_exits_current_function(
        text,
        context,
        index,
        regions,
        region.then_branch.start,
        region.then_branch.end,
    ) {
        return false;
    }

    let Some(TextRange {
        start: else_start,
        end: else_end,
    }) = region.else_branch
    else {
        return false;
    };

    if let Some(nested) = regions
        .iter()
        .find(|candidate| candidate.if_start == else_start)
    {
        return if_chain_all_returning_paths_bind(
            text,
            context,
            index,
            regions,
            nested,
            definition_starts,
        );
    }

    branch_binds_reaching_definition(text, regions, else_start, else_end, definition_starts)
        || branch_exits_current_function(text, context, index, regions, else_start, else_end)
}

fn branch_binds_reaching_definition(
    text: &str,
    regions: &[IfRegion],
    branch_start: usize,
    branch_end: usize,
    definition_starts: &[usize],
) -> bool {
    definition_starts.iter().copied().any(|definition_start| {
        definition_start >= branch_start
            && definition_start < branch_end
            && definition_is_top_level_in_branch(text, branch_start, branch_end, definition_start)
            && !regions.iter().any(|nested| {
                nested.if_start >= branch_start
                    && nested.if_start < branch_end
                    && nested.if_start != branch_start
                    && ((definition_start >= nested.then_branch.start && definition_start < nested.then_branch.end)
                        || matches!(nested.else_branch, Some(TextRange { start, end }) if definition_start >= start && definition_start < end))
            })
    })
}

fn definition_is_top_level_in_branch(
    text: &str,
    branch_start: usize,
    branch_end: usize,
    definition_start: usize,
) -> bool {
    let start = skip_trivia_bounded(text, branch_start, branch_end);
    if text.as_bytes().get(start).copied() != Some(b'{') {
        return start == definition_start;
    }
    let Some(close) = matching_delimiter(text, start) else {
        return false;
    };
    if definition_start <= start || definition_start >= close {
        return false;
    }

    let statement = skip_trivia_bounded(
        text,
        statement_start(text, definition_start).max(start + 1),
        definition_start,
    );
    if statement != definition_start {
        return false;
    }

    let bytes = text.as_bytes();
    let mut cursor = start + 1;
    let mut brace_depth = 0usize;
    let mut quote = None;
    while cursor < definition_start {
        let byte = bytes[cursor];
        if let Some(delimiter) = quote {
            if byte == b'\\' {
                cursor = (cursor + 2).min(definition_start);
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
            b'#' => cursor = skip_comment(text, cursor, definition_start),
            b'{' => {
                brace_depth += 1;
                cursor += 1;
            }
            b'}' => {
                brace_depth = brace_depth.saturating_sub(1);
                cursor += 1;
            }
            _ => cursor += 1,
        }
    }
    brace_depth == 0
}

fn branch_exits_current_function(
    text: &str,
    context: &OakParseContext,
    index: &SemanticIndex,
    regions: &[IfRegion],
    start: usize,
    end: usize,
) -> bool {
    let Some((expression_start, expression_end)) = last_top_level_expression(text, start, end)
    else {
        return false;
    };

    if let Some(region) = regions.iter().find(|region| {
        region.if_start == expression_start
            && region
                .else_branch
                .is_some_and(|branch| branch.end <= expression_end)
    }) {
        return if_chain_all_paths_exit(text, context, index, regions, region);
    }

    let Some((package, callee, callee_start)) =
        direct_call_expression(text, expression_start, expression_end)
    else {
        return false;
    };
    if package.is_none() && callee == "return" {
        return true;
    }
    call_is_non_returning(context, index, package.as_deref(), &callee, callee_start)
}

fn if_chain_all_paths_exit(
    text: &str,
    context: &OakParseContext,
    index: &SemanticIndex,
    regions: &[IfRegion],
    region: &IfRegion,
) -> bool {
    if !branch_exits_current_function(
        text,
        context,
        index,
        regions,
        region.then_branch.start,
        region.then_branch.end,
    ) {
        return false;
    }
    let Some(TextRange {
        start: else_start,
        end: else_end,
    }) = region.else_branch
    else {
        return false;
    };
    if let Some(nested) = regions
        .iter()
        .find(|candidate| candidate.if_start == else_start)
    {
        if_chain_all_paths_exit(text, context, index, regions, nested)
    } else {
        branch_exits_current_function(text, context, index, regions, else_start, else_end)
    }
}

fn bare_call_is_external(index: &SemanticIndex, callee: &str, start: usize) -> bool {
    for scope in index.scope_ids() {
        for (use_id, use_site) in index.uses(scope).iter() {
            if text_offset(use_site.range().start()) != start {
                continue;
            }
            let symbol = index.symbols(scope).symbol(use_site.symbol());
            if symbol.name() != callee {
                continue;
            }
            return !index.use_is_bound(scope, use_id)
                && index.reaching_definitions(scope, use_id).next().is_none();
        }
    }
    false
}

fn call_is_non_returning(
    context: &OakParseContext,
    index: &SemanticIndex,
    package: Option<&str>,
    callee: &str,
    callee_start: usize,
) -> bool {
    match package {
        Some("base") => matches!(callee, "stop" | "q" | "quit"),
        Some(_) => false,
        None if !bare_call_is_external(index, callee, callee_start) => false,
        None if matches!(callee, "stop" | "q" | "quit") => context.resolves_to_base(callee),
        None => context.non_returning_names.contains(callee),
    }
}

fn last_top_level_expression(text: &str, start: usize, end: usize) -> Option<(usize, usize)> {
    let start = skip_trivia_bounded(text, start, end);
    let (mut cursor, limit) = if text.as_bytes().get(start).copied() == Some(b'{') {
        let close = matching_delimiter(text, start)?;
        (start + 1, close.min(end))
    } else {
        (start, end)
    };

    let mut last = None;
    while cursor < limit {
        cursor = skip_trivia_bounded(text, cursor, limit);
        while cursor < limit && text.as_bytes()[cursor] == b';' {
            cursor += 1;
            cursor = skip_trivia_bounded(text, cursor, limit);
        }
        if cursor >= limit {
            break;
        }
        let expression_start = cursor;
        let expression_end = expression_end(text, expression_start).min(limit);
        if expression_end <= expression_start {
            break;
        }
        last = Some((expression_start, expression_end));
        cursor = expression_end;
        while cursor < limit && matches!(text.as_bytes()[cursor], b';' | b'\n' | b'\r') {
            cursor += 1;
        }
    }
    last
}

fn direct_call_expression(
    text: &str,
    start: usize,
    end: usize,
) -> Option<(Option<String>, String, usize)> {
    let start = skip_trivia_bounded(text, start, end);
    let first_end = name_token_end(text, start)?;
    let first = static_symbol(text.get(start..first_end)?)?;
    let mut cursor = skip_trivia_bounded(text, first_end, end);

    let (package, callee, callee_start) = if text
        .get(cursor..end)
        .is_some_and(|rest| rest.starts_with(":::"))
    {
        cursor += 3;
        cursor = skip_trivia_bounded(text, cursor, end);
        let callee_start = cursor;
        let callee_end = name_token_end(text, cursor)?;
        let callee = static_symbol(text.get(cursor..callee_end)?)?;
        cursor = callee_end;
        (Some(first), callee, callee_start)
    } else if text
        .get(cursor..end)
        .is_some_and(|rest| rest.starts_with("::"))
    {
        cursor += 2;
        cursor = skip_trivia_bounded(text, cursor, end);
        let callee_start = cursor;
        let callee_end = name_token_end(text, cursor)?;
        let callee = static_symbol(text.get(cursor..callee_end)?)?;
        cursor = callee_end;
        (Some(first), callee, callee_start)
    } else {
        (None, first, start)
    };

    cursor = skip_trivia_bounded(text, cursor, end);
    if text.as_bytes().get(cursor).copied() != Some(b'(') {
        return None;
    }
    let close = matching_delimiter(text, cursor)?;
    let trailing = skip_trivia_bounded(text, close + 1, end);
    (trailing >= end).then_some((package, callee, callee_start))
}

/// Conservative package-local summary used by the linker when constructing an
/// Oak parse context. This summary is intentionally small: it recognizes only
/// control flow whose final expression is provably non-returning through base
/// termination primitives, an exhaustive `if`/`else`, or another package-local
/// helper already carrying the same summary. Any explicit `return()` elsewhere
/// in the closure prevents the summary.
pub(crate) fn closure_definitely_non_returning(text: &str, context: &OakParseContext) -> bool {
    let Some((body_start, body_end)) = function_body_range(text) else {
        return false;
    };
    if contains_call_named(text.get(body_start..body_end).unwrap_or_default(), "return") {
        return false;
    }
    expression_definitely_non_returning(text, context, body_start, body_end)
}

fn expression_definitely_non_returning(
    text: &str,
    context: &OakParseContext,
    start: usize,
    end: usize,
) -> bool {
    let Some((expression_start, expression_end)) = last_top_level_expression(text, start, end)
    else {
        return false;
    };

    let regions = find_if_regions(text);
    if let Some(region) = regions.iter().find(|region| {
        region.if_start == expression_start
            && region
                .else_branch
                .is_some_and(|branch| branch.end <= expression_end)
    }) {
        let Some(TextRange {
            start: else_start,
            end: else_end,
        }) = region.else_branch
        else {
            return false;
        };
        return expression_definitely_non_returning(
            text,
            context,
            region.then_branch.start,
            region.then_branch.end,
        ) && expression_definitely_non_returning(text, context, else_start, else_end);
    }

    let Some((package, callee, callee_start)) =
        direct_call_expression(text, expression_start, expression_end)
    else {
        return false;
    };

    match package.as_deref() {
        Some("base") => matches!(callee.as_str(), "stop" | "q" | "quit"),
        Some(_) => false,
        None if matches!(callee.as_str(), "stop" | "q" | "quit") => {
            context.resolves_to_base(&callee)
                && !identifier_occurs_before(text, &callee, callee_start)
        }
        None => {
            context.non_returning_names.contains(&callee)
                && !identifier_occurs_before(text, &callee, callee_start)
        }
    }
}

fn identifier_occurs_before(text: &str, name: &str, end: usize) -> bool {
    let bytes = text.as_bytes();
    let mut cursor = 0;
    let mut quote = None;
    while cursor < end.min(bytes.len()) {
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
            b'#' => cursor = skip_comment(text, cursor, end),
            _ if text
                .get(cursor..end)
                .is_some_and(|rest| rest.starts_with(name))
                && word_boundary_before(text, cursor)
                && word_boundary_after(text, cursor + name.len()) =>
            {
                return true;
            }
            _ => cursor += text[cursor..].chars().next().map_or(1, char::len_utf8),
        }
    }
    false
}

fn function_body_range(text: &str) -> Option<(usize, usize)> {
    let bytes = text.as_bytes();
    let mut cursor = 0;
    let mut quote = None;
    while cursor < bytes.len() {
        let byte = bytes[cursor];
        if let Some(delimiter) = quote {
            if byte == b'\\' {
                cursor = (cursor + 2).min(bytes.len());
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
            b'f' if text
                .get(cursor..)
                .is_some_and(|rest| rest.starts_with("function"))
                && word_boundary_before(text, cursor)
                && word_boundary_after(text, cursor + "function".len()) =>
            {
                let open = skip_trivia(text, cursor + "function".len());
                if bytes.get(open).copied() != Some(b'(') {
                    cursor += "function".len();
                    continue;
                }
                let close = matching_delimiter(text, open)?;
                let body_start = skip_trivia(text, close + 1);
                if bytes.get(body_start).copied() == Some(b'{') {
                    let body_close = matching_delimiter(text, body_start)?;
                    return Some((body_start, body_close + 1));
                }
                let body_end = expression_end(text, body_start);
                return Some((body_start, body_end));
            }
            _ => cursor += 1,
        }
    }
    None
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
        && let Some(target) = aliases.get(name)
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
            Some(StaticEnvironment::Namespace(package.clone()))
        }
        ("environment", StaticArg::Symbol(binding)) => {
            Some(StaticEnvironment::ClosureBinding(binding.clone()))
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
            Some(StaticArg::Symbol(name)) => aliases.get(name).cloned(),
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
) -> (Vec<SyntaxEffect>, Vec<(String, usize, usize)>) {
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
                target: Some(target),
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
    // `<<-` skips the current function environment and searches lexical
    // parents. Oak records the superassignment at its lexical site, but the
    // index does not expose a point-in-time query for a definition target.
    // Use Oak to identify bindings, and syntax regions only to identify which
    // function activation owns those bindings and whether their assignment
    // must have executed before the nested closure is created.
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
        // Formal bindings exist in the activation frame from function entry.
        if ancestor.parameters.contains(target) {
            return true;
        }

        // The direct child function on the path to the superassignment is the
        // conservative closure-creation boundary for assignments in this
        // ancestor. Assignments after that point may still be safe if the
        // closure is invoked later, but proving that requires call-context
        // analysis and is deliberately left unresolved here.
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

fn apply_guard_regions_to_references(
    regions: &[(TextRange, PackageGuard)],
    references: &mut [NameRef],
) {
    for (range, guard) in regions {
        for reference in references.iter_mut() {
            if range.contains_range(reference.span.range()) {
                push_guard(&mut reference.guards, guard.clone());
            }
        }
    }
}

fn apply_guard_regions_to_package_refs(
    regions: &[(TextRange, PackageGuard)],
    references: &mut [PackageRef],
) {
    for (range, guard) in regions {
        for reference in references.iter_mut() {
            if range.contains_range(reference.span.range()) {
                push_guard(&mut reference.guards, guard.clone());
            }
        }
    }
}

fn apply_guard_regions_to_calls(regions: &[(TextRange, PackageGuard)], calls: &mut [LiveCall]) {
    for (range, guard) in regions {
        for call in calls.iter_mut() {
            if range.contains_range(call.site.span.range()) {
                push_guard(&mut call.site.guards, guard.clone());
            }
        }
    }
}

fn apply_guard_regions_to_effects(
    regions: &[(TextRange, PackageGuard)],
    effects: &mut [SyntaxEffect],
) {
    for (range, guard) in regions {
        for effect in effects.iter_mut() {
            if range.contains_range(effect.span.range()) {
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
) -> Vec<(TextRange, PackageGuard)> {
    let mut guards = Vec::new();
    for region in regions {
        if condition_contains_or(text, region.condition.start, region.condition.end) {
            continue;
        }
        for call in calls {
            if !region.condition.contains_range(call.site.span.range())
                || !is_base_call(context, &call.site)
                || directly_negated(text, region.condition.start, call.site.span.start)
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
                guards.push((region.then_branch, guard));
            }
        }
    }
    guards
}

fn hook_guard_regions(
    context: &OakParseContext,
    calls: &[LiveCall],
) -> Vec<(TextRange, PackageGuard)> {
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
                && event_argument
                    .value
                    .contains_range(nested.site.span.range())
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
                callback_argument.value,
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

fn static_symbol_range(text: &str, start: usize, end: usize) -> Option<(String, usize, usize)> {
    let start = skip_trivia(text, start);
    let end = trim_end_offset(text, end);
    let value = text.get(start..end)?;
    match static_arg(value) {
        Some(StaticArg::Symbol(name)) => Some((name, start, end)),
        _ => None,
    }
}

fn static_args(raw: &RawCall) -> Vec<Option<StaticArg>> {
    raw.args
        .iter()
        .map(|argument| argument.static_arg.clone())
        .collect()
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
                        value: TextRange::new(cursor, cursor),
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
                value: TextRange::new(end, end),
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
        value: TextRange::new(value_start, value_end),
        static_arg,
    })
}

fn argument_spans(source: &SourceId, arguments: &[RawArgument]) -> Vec<Option<Span>> {
    arguments
        .iter()
        .map(|argument| {
            (argument.value.start < argument.value.end)
                .then(|| Span::new(*source, argument.value.start, argument.value.end))
        })
        .collect()
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
                let previous = cursor
                    .checked_sub(1)
                    .and_then(|index| bytes.get(index))
                    .copied();
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

fn find_function_regions(text: &str) -> Vec<FunctionRegion> {
    let mut regions = Vec::new();
    let bytes = text.as_bytes();
    let mut cursor = 0;
    let mut quote = None;

    while cursor < bytes.len() {
        let byte = bytes[cursor];
        if let Some(delimiter) = quote {
            if byte == b'\\' {
                cursor = (cursor + 2).min(bytes.len());
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
            b'f' if text
                .get(cursor..)
                .is_some_and(|rest| rest.starts_with("function"))
                && word_boundary_before(text, cursor)
                && word_boundary_after(text, cursor + "function".len()) =>
            {
                let open = skip_trivia(text, cursor + "function".len());
                if bytes.get(open).copied() != Some(b'(') {
                    cursor += "function".len();
                    continue;
                }
                if let Some(region) = function_region_after_open(text, cursor, open) {
                    regions.push(region);
                }
                cursor += "function".len();
            }
            b'\\' => {
                let open = skip_trivia(text, cursor + 1);
                if bytes.get(open).copied() == Some(b'(')
                    && let Some(region) = function_region_after_open(text, cursor, open)
                {
                    regions.push(region);
                }
                cursor += 1;
            }
            _ => cursor += text[cursor..].chars().next().map_or(1, char::len_utf8),
        }
    }

    regions
}

fn function_region_after_open(
    text: &str,
    function_start: usize,
    open: usize,
) -> Option<FunctionRegion> {
    let close = matching_delimiter(text, open)?;
    let body_start = skip_trivia(text, close + 1);
    if body_start >= text.len() {
        return None;
    }
    let body_end = expression_end(text, body_start);
    let parameters = split_arguments(text, open + 1, close)
        .into_iter()
        .filter_map(|argument| {
            let RawArgument { name, value, .. } = argument;
            name.or_else(|| text.get(value.start..value.end).and_then(static_symbol))
        })
        .collect();

    Some(FunctionRegion {
        function_start,
        formals: TextRange::new(open + 1, close),
        body: TextRange::new(body_start, body_end),
        parameters,
    })
}

fn find_for_regions(text: &str) -> Vec<ForRegion> {
    let mut regions = Vec::new();
    let bytes = text.as_bytes();
    let mut cursor = 0;
    let mut quote = None;

    while cursor < bytes.len() {
        let byte = bytes[cursor];
        if let Some(delimiter) = quote {
            if byte == b'\\' {
                cursor = (cursor + 2).min(bytes.len());
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
            b'f' if text
                .get(cursor..)
                .is_some_and(|rest| rest.starts_with("for"))
                && word_boundary_before(text, cursor)
                && word_boundary_after(text, cursor + 3) =>
            {
                let open = skip_trivia(text, cursor + 3);
                if bytes.get(open).copied() != Some(b'(') {
                    cursor += 3;
                    continue;
                }
                let Some(close) = matching_delimiter(text, open) else {
                    cursor += 3;
                    continue;
                };
                let variable_start = skip_trivia_bounded(text, open + 1, close);
                let Some(variable_end) = name_token_end(text, variable_start) else {
                    cursor += 3;
                    continue;
                };
                let Some(variable) = text
                    .get(variable_start..variable_end)
                    .and_then(static_symbol)
                else {
                    cursor += 3;
                    continue;
                };
                let in_start = skip_trivia_bounded(text, variable_end, close);
                if !text
                    .get(in_start..close)
                    .is_some_and(|rest| rest.starts_with("in"))
                    || !word_boundary_after(text, in_start + 2)
                {
                    cursor += 3;
                    continue;
                }
                let body_start = skip_trivia(text, close + 1);
                let body_end = expression_end(text, body_start);
                regions.push(ForRegion {
                    variable,
                    variable_start,
                    body: TextRange::new(body_start, body_end),
                });
                cursor += 3;
            }
            _ => cursor += text[cursor..].chars().next().map_or(1, char::len_utf8),
        }
    }

    regions
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
            b'i' if text
                .get(cursor..)
                .is_some_and(|rest| rest.starts_with("if"))
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
                let else_branch = if text
                    .get(after_then..)
                    .is_some_and(|rest| rest.starts_with("else"))
                    && word_boundary_after(text, after_then + 4)
                {
                    let start = skip_trivia(text, after_then + 4);
                    let end = expression_end(text, start);
                    Some(TextRange::new(start, end))
                } else {
                    None
                };
                regions.push(IfRegion {
                    if_start: cursor,
                    condition: TextRange::new(open + 1, close),
                    then_branch: TextRange::new(then_start, then_end),
                    else_branch,
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
            return start
                + region
                    .else_branch
                    .map_or(region.then_branch.end, |branch| branch.end);
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
            b'e' if stack.is_empty()
                && text
                    .get(cursor..)
                    .is_some_and(|rest| rest.starts_with("else"))
                && word_boundary_before(text, cursor)
                && word_boundary_after(text, cursor + 4) =>
            {
                return trim_end_offset(text, cursor);
            }
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
            .is_none_or(|character| {
                !(character.is_alphanumeric() || character == '.' || character == '_')
            })
}

fn word_boundary_after(text: &str, position: usize) -> bool {
    text.get(position..)
        .and_then(|suffix| suffix.chars().next())
        .is_none_or(|character| {
            !(character.is_alphanumeric() || character == '.' || character == '_')
        })
}

impl RParser for OakParser {
    fn parse(&self, source: SourceId, text: &str) -> Result<ParsedRFile> {
        self.parse_binding(source, text)
            .map_err(|message| Error::Parse {
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
    fn evaluated_quotation_is_live_code_in_the_calling_frame() {
        let parsed =
            parse_source("f <- function(root) eval(bquote(function(...) path(.(root), ...)))");
        let names = reference_names(&parsed);
        assert!(names.contains(&"path"));
        assert!(!names.contains(&"root"));
        assert!(
            reference_names(&parse_source("f <- function() eval(quote(helper()))"))
                .contains(&"helper")
        );
        assert!(
            reference_names(&parse_source("f <- function() evalq(helper())")).contains(&"helper")
        );
    }

    #[test]
    fn callee_shadowed_by_a_non_closure_local_can_reach_the_enclosing_function() {
        let kind = |source| {
            parse_source(source).expressions[0]
                .references
                .iter()
                .find(|reference| reference.name == "path")
                .map(|reference| reference.kind)
        };
        assert_eq!(
            kind("f <- function(path) path(path, 'x')"),
            Some(NameRefKind::MaybeLocal)
        );
        assert_eq!(
            kind("f <- function() { path <- function() 1; path() }"),
            None
        );
    }

    #[test]
    fn dots_elements_are_never_free_names() {
        let parsed = parse_source("f <- function(...) if (!missing(..1)) ..12 else ..x");
        let names = reference_names(&parsed);
        assert!(!names.contains(&"..1"));
        assert!(!names.contains(&"..12"));
        assert!(names.contains(&"..x"));
    }

    #[test]
    fn replacement_call_references_the_replacement_function() {
        let parsed = parse_source("f <- function(x) { substr2(x, 1, 2) <- 'a'; x }");
        let names = reference_names(&parsed);
        assert!(names.contains(&"substr2<-"));
        assert!(!names.contains(&"substr2"));
    }

    #[test]
    fn assigned_value_start_is_the_outer_assignment_value() {
        let source = "`.onLoad` <- function(libname, pkgname) { x <- 1 }";
        let start = assigned_value_start(source).unwrap();
        assert!(source[start..].starts_with("function(libname"));
        assert_eq!(assigned_value_start("f(1)"), None);
    }

    #[test]
    fn slinker_declaration_is_an_inert_lexical_contract() {
        let parsed = parse_source(
            "f <- function(x) { print(x); declare(slinker(x = one_of(s3('foo'), s3('bar', 'parent')))) }",
        );
        let names = reference_names(&parsed);
        for inert in ["slinker", "one_of", "s3"] {
            assert!(!names.contains(&inert), "{inert}");
        }
        assert!(
            parsed.expressions[0]
                .calls
                .iter()
                .all(|call| !["slinker", "s3", "one_of"].contains(&call.callee.as_str()))
        );
        let [declaration] = parsed.declarations.as_slice() else {
            panic!("{:?}", parsed.declarations);
        };
        assert_eq!(declaration.binding.name, "x");
        assert_eq!(
            declaration.domain,
            DeclaredDomain::Classes(vec![
                vec!["foo".into()],
                vec!["bar".into(), "parent".into()]
            ])
        );
        let print = parsed.expressions[0]
            .calls
            .iter()
            .find(|call| call.callee == "print")
            .unwrap();
        assert_eq!(
            parsed.class_domain_for(print.arg_bindings[0].as_ref().unwrap(), print.scope),
            Some(vec![
                vec!["foo".into()],
                vec!["bar".into(), "parent".into()]
            ])
        );
    }

    #[test]
    fn string_declarations_narrow_and_never_mix_with_classes() {
        let parsed = parse_source(
            "f <- function(name) { declare(slinker(name = strings('alpha', 'beta'))); g <- function() { declare(slinker(name = strings('beta', 'gamma'))); print(name) } }",
        );
        assert!(parsed.issues.is_empty(), "{:?}", parsed.issues);
        let print = parsed.expressions[0]
            .calls
            .iter()
            .find(|call| call.callee == "print")
            .unwrap();
        let binding = print.arg_bindings[0].as_ref().unwrap();
        assert_eq!(
            parsed.string_domain_for(binding, print.scope),
            Some(["beta".to_owned()].into_iter().collect())
        );
        assert_eq!(parsed.class_domain_for(binding, print.scope), None);
        assert!(names_inert(&parsed, "strings"));

        let mixed = parse_source(
            "f <- function(x) { declare(slinker(x = strings('a'))); declare(slinker(x = s3('foo'))) }",
        );
        assert_eq!(mixed.declarations.len(), 1);
        assert!(
            mixed
                .issues
                .iter()
                .any(|issue| issue.kind == SemanticIssueKind::InvalidDeclaration)
        );
        for malformed in [
            "f <- function(x) declare(slinker(x = strings()))",
            "f <- function(x) declare(slinker(x = strings(y)))",
            "f <- function(x) declare(slinker(x = one_of(strings('a'))))",
        ] {
            let parsed = parse_source(malformed);
            assert!(parsed.declarations.is_empty(), "{malformed}");
            assert!(
                parsed
                    .issues
                    .iter()
                    .any(|issue| issue.kind == SemanticIssueKind::InvalidDeclaration),
                "{malformed}"
            );
        }
    }

    #[test]
    fn callable_declarations_name_exact_functions() {
        let parsed = parse_source(
            "f <- function(fun) { declare(slinker(fun = callables(pkg::g, pkg:::h, local_fn))); print(fun) }",
        );
        assert!(parsed.issues.is_empty(), "{:?}", parsed.issues);
        let print = parsed.expressions[0]
            .calls
            .iter()
            .find(|call| call.callee == "print")
            .unwrap();
        let callable = |package: Option<&str>, name: &str| DeclaredCallable {
            package: package.map(str::to_owned),
            name: name.to_owned(),
        };
        assert_eq!(
            parsed.callable_domain_for(print.arg_bindings[0].as_ref().unwrap(), print.scope),
            Some(
                [
                    callable(Some("pkg"), "g"),
                    callable(Some("pkg"), "h"),
                    callable(None, "local_fn"),
                ]
                .into_iter()
                .collect()
            )
        );
        assert!(names_inert(&parsed, "callables"));
        for malformed in [
            "f <- function(x) declare(slinker(x = callables()))",
            "f <- function(x) declare(slinker(x = callables('g')))",
            "f <- function(x) declare(slinker(x = callables(g(1))))",
        ] {
            let parsed = parse_source(malformed);
            assert!(parsed.declarations.is_empty(), "{malformed}");
        }
    }

    fn names_inert(parsed: &ParsedRFile, name: &str) -> bool {
        !reference_names(parsed).contains(&name)
            && parsed.expressions[0]
                .calls
                .iter()
                .all(|call| call.callee != name)
    }

    #[test]
    fn nested_declaration_narrows_the_captured_binding() {
        let parsed = parse_source(
            "f <- function(x) { declare(slinker(x = one_of(s3('foo'), s3('bar')))); g <- function() { declare(slinker(x = s3('foo'))); print(x) } }",
        );
        let print = parsed.expressions[0]
            .calls
            .iter()
            .find(|call| call.callee == "print")
            .unwrap();
        assert_eq!(
            parsed.class_domain_for(print.arg_bindings[0].as_ref().unwrap(), print.scope),
            Some(vec![vec!["foo".into()]])
        );
    }

    #[test]
    fn shadowed_or_malformed_declarations_are_not_contracts() {
        let shadowed = parse_source(
            "f <- function(x) { declare <- function(...) NULL; declare(slinker(x = s3('foo'))) }",
        );
        assert!(shadowed.declarations.is_empty());
        let malformed = parse_source("f <- function(x) declare(slinker(x = s3(klass)))");
        assert!(malformed.declarations.is_empty());
        assert!(
            malformed
                .issues
                .iter()
                .any(|issue| issue.kind == SemanticIssueKind::InvalidDeclaration)
        );
    }

    #[test]
    fn custom_infix_operator_is_a_name_reference() {
        let parsed = parse_source("f <- function(a, b) a %R% (b %in% a)");
        let names = reference_names(&parsed);
        assert!(names.contains(&"%R%"));
        assert!(names.contains(&"%in%"));
    }

    #[test]
    fn data_masked_names_may_be_columns() {
        let parsed = parse_source("f <- function(d) with(d, middle + helper(x))");
        let kinds = parsed.expressions[0]
            .references
            .iter()
            .map(|reference| (reference.name.as_str(), reference.kind))
            .collect::<Vec<_>>();
        assert!(kinds.contains(&("middle", NameRefKind::MaybeLocal)));
        assert!(kinds.contains(&("helper", NameRefKind::MaybeLocal)));
    }

    #[test]
    fn dispatch_frame_variables_are_never_free_names() {
        let parsed = parse_source("Ops.poly <- function(e1, e2) switch(.Generic, `+` = .Class)");
        let names = reference_names(&parsed);
        assert!(!names.contains(&".Generic"));
        assert!(!names.contains(&".Class"));
    }

    #[test]
    fn quotation_evaluated_elsewhere_stays_inert() {
        let parsed = parse_source("f <- function(env) eval(quote(helper()), env)");
        assert!(!reference_names(&parsed).contains(&"helper"));
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
    fn superassigned_value_is_a_dependency_only_when_free() {
        let values = |text| {
            parse_source(text).expressions[0]
                .effects
                .iter()
                .map(|effect| (effect.kind, effect.value_symbol.clone()))
                .collect::<Vec<_>>()
        };

        let local = values("set <- function(x) value <<- x");
        let free = values("set <- function() value <<- other");

        assert!(!local.is_empty() && !free.is_empty());
        assert!(
            local
                .iter()
                .all(|effect| *effect == (SyntaxEffectKind::SuperAssignment, None))
        );
        assert!(free.iter().all(|effect| {
            *effect == (SyntaxEffectKind::SuperAssignment, Some("other".to_owned()))
        }));
    }

    #[test]
    fn quoted_namespace_access_is_inert() {
        let parsed = parse_source("f <- function() quote(foo::bar)");
        assert!(parsed.expressions[0].package_refs.is_empty());
    }

    #[test]
    fn evaluated_namespace_access_is_recorded() {
        let parsed = parse_source("f <- function() foo::bar()");
        assert!(
            parsed.expressions[0]
                .package_refs
                .iter()
                .any(|reference| reference.package == "foo" && reference.symbol == "bar")
        );
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
        assert!(
            resolver
                .resolve_effects("quote", &["some_attached_package".to_owned()])
                .is_none()
        );
    }

    #[test]
    fn definite_local_assignment_never_becomes_external_reference() {
        let parsed = parse_source("f <- function() { x <- 1; x }");
        assert!(!reference_names(&parsed).contains(&"x"));
    }

    #[test]
    fn repeated_predicate_proves_local_binding() {
        let parsed = parse_source(
            "f <- function(alternative) { if (!is.null(alternative)) { tvalue <- 1 }; if (!is.null(alternative)) tvalue }",
        );
        assert!(!reference_names(&parsed).contains(&"tvalue"));
    }

    #[test]
    fn repeated_else_if_predicates_preserve_branch_specific_bindings() {
        let parsed = parse_source(
            r#"f <- function(alternative, prob2) {
                if (alternative == "less") {
                    rr <- 1
                } else if (alternative == "greater") {
                    rr <- 2
                } else if (alternative == "two.sided") {
                    lowerrr <- 3
                    upperrr <- 4
                } else {
                    stop("bad alternative")
                }
                if (!is.null(prob2)) {
                    if (alternative == "less") rr
                    else if (alternative == "greater") rr
                    else if (alternative == "two.sided") lowerrr + upperrr
                }
            }"#,
        );
        let names = reference_names(&parsed);
        assert!(!names.contains(&"rr"));
        assert!(!names.contains(&"lowerrr"));
        assert!(!names.contains(&"upperrr"));
    }

    #[test]
    fn impure_repeated_predicate_does_not_prove_local_binding() {
        let parsed =
            parse_source("f <- function() { if (predicate()) x <- 1; if (predicate()) x }");
        let reference = parsed.expressions[0]
            .references
            .iter()
            .find(|reference| reference.name == "x")
            .expect("impure repeated predicate must retain fallthrough");
        assert_eq!(reference.kind, NameRefKind::ConditionalFallthrough);
    }

    #[test]
    fn rebound_predicate_symbol_does_not_prove_local_binding() {
        let parsed =
            parse_source("f <- function(flag) { if (flag) x <- 1; flag <- !flag; if (flag) x }");
        let reference = parsed.expressions[0]
            .references
            .iter()
            .find(|reference| reference.name == "x")
            .expect("rebinding predicate input must retain fallthrough");
        assert_eq!(reference.kind, NameRefKind::ConditionalFallthrough);
    }

    #[test]
    fn broader_later_guard_does_not_hide_real_fallthrough() {
        let parsed = parse_source(
            r#"f <- function(alternative) {
                if (!is.null(alternative)) {
                    if (alternative == "less") pvalue <- 1
                    else if (alternative == "greater") pvalue <- 2
                    else if (alternative == "two.sided") pvalue <- 3
                }
                if (!is.null(alternative)) pvalue
            }"#,
        );
        let reference = parsed.expressions[0]
            .references
            .iter()
            .find(|reference| reference.name == "pvalue")
            .expect("invalid alternative must retain fallthrough");
        assert_eq!(reference.kind, NameRefKind::ConditionalFallthrough);
    }

    #[test]
    fn exhaustive_dispatch_with_stop_proves_local_binding() {
        let parsed = parse_source(
            r#"f <- function(direction) {
                if (direction == "below") showprob <- 1
                else if (direction == "above") showprob <- 2
                else if (direction == "between") showprob <- 3
                else if (direction == "outside") showprob <- 4
                else stop("bad direction")
                showprob
            }"#,
        );
        assert!(!reference_names(&parsed).contains(&"showprob"));
    }

    #[test]
    fn package_local_non_returning_helper_proves_local_binding() {
        let mut context = OakParseContext::default();
        context.non_returning_names.insert(".die".to_owned());
        let parsed = OakParser
            .parse_binding_with_context(
                SourceId(0),
                r#"f <- function(direction) {
                    if (direction == "below") showprob <- 1
                    else if (direction == "above") showprob <- 2
                    else .die()
                    showprob
                }"#,
                &context,
            )
            .unwrap();
        assert!(!reference_names(&parsed).contains(&"showprob"));
    }

    #[test]
    fn missing_final_else_keeps_real_fallthrough() {
        let parsed = parse_source(
            r#"f <- function(direction) {
                if (direction == "below") showprob <- 1
                else if (direction == "above") showprob <- 2
                showprob
            }"#,
        );
        let reference = parsed.expressions[0]
            .references
            .iter()
            .find(|reference| reference.name == "showprob")
            .expect("unhandled direction must remain a fallthrough");
        assert_eq!(reference.kind, NameRefKind::ConditionalFallthrough);
    }

    #[test]
    fn zero_iteration_loop_keeps_real_fallthrough() {
        let parsed =
            parse_source("f <- function(xs) { for (i in seq_along(xs)) sephat <- i; sephat }");
        let reference = parsed.expressions[0]
            .references
            .iter()
            .find(|reference| reference.name == "sephat")
            .expect("zero-iteration loop must retain fallthrough");
        assert_eq!(reference.kind, NameRefKind::ConditionalFallthrough);
    }

    #[test]
    fn locally_shadowed_non_returning_helper_does_not_discharge_fallthrough() {
        let mut context = OakParseContext::default();
        context.non_returning_names.insert(".die".to_owned());
        let parsed = OakParser
            .parse_binding_with_context(
                SourceId(0),
                "f <- function(flag, .die) { if (flag) x <- 1 else .die(); x }",
                &context,
            )
            .unwrap();
        let reference = parsed.expressions[0]
            .references
            .iter()
            .find(|reference| reference.name == "x")
            .expect("shadowed helper may return, so x must still fall through");
        assert_eq!(reference.kind, NameRefKind::ConditionalFallthrough);
    }

    #[test]
    fn locally_shadowed_stop_is_not_summarized_as_non_returning() {
        let context = OakParseContext::default();
        assert!(!closure_definitely_non_returning(
            "function(stop) stop('not base stop')",
            &context,
        ));
    }

    #[test]
    fn non_returning_summary_requires_terminal_stop() {
        let context = OakParseContext::default();
        assert!(closure_definitely_non_returning(
            "function(message) { stop(message) }",
            &context,
        ));
        assert!(!closure_definitely_non_returning(
            "function(flag) { if (flag) stop('bad'); 1 }",
            &context,
        ));
    }

    #[test]
    fn later_formal_is_bound_in_earlier_default() {
        let parsed = parse_source("f <- function(x = y, y = 1) x");
        assert!(!reference_names(&parsed).contains(&"y"));
    }

    #[test]
    fn missing_later_formal_is_still_bound_in_default_environment() {
        let parsed = parse_source("f <- function(x = y, y) x");
        assert!(!reference_names(&parsed).contains(&"y"));
    }

    #[test]
    fn self_referential_default_is_not_global_fallthrough() {
        let parsed = parse_source("f <- function(x = x) x");
        assert!(!reference_names(&parsed).contains(&"x"));
    }

    #[test]
    fn for_variable_is_definitely_bound_inside_body() {
        let parsed = parse_source("f <- function(xs) { for (x in xs) print(x) }");
        assert!(!reference_names(&parsed).contains(&"x"));
    }

    #[test]
    fn for_variable_may_be_unbound_after_zero_iterations() {
        let parsed = parse_source("f <- function(xs) { for (x in xs) {}; print(x) }");
        let reference = parsed.expressions[0]
            .references
            .iter()
            .find(|reference| reference.name == "x")
            .expect("post-loop use must preserve the zero-iteration fallthrough");
        assert_eq!(reference.kind, NameRefKind::ConditionalFallthrough);
    }

    #[test]
    fn preexisting_binding_keeps_post_for_use_bound() {
        let parsed = parse_source("f <- function(xs) { x <- 0; for (x in xs) {}; print(x) }");
        assert!(!reference_names(&parsed).contains(&"x"));
    }

    #[test]
    fn nested_for_variables_are_bound_across_next_paths() {
        let parsed = parse_source(
            "f <- function(xs, ys, flag) { for (x in xs) { for (y in ys) { if (flag) next; print(x + y) } } }",
        );
        let names = reference_names(&parsed);
        assert!(!names.contains(&"x"));
        assert!(!names.contains(&"y"));
    }

    #[test]
    fn captured_activation_superassignment_is_resolved() {
        let parsed = parse_source("outer <- function() { x <- 1; function() { x <<- x + 1 } }");
        let effect = parsed.expressions[0]
            .effects
            .iter()
            .find(|effect| effect.target.as_deref() == Some("x"))
            .expect("superassignment effect");
        assert!(effect.target_enclosing_local);
    }

    #[test]
    fn conditional_outer_assignment_does_not_fake_capture() {
        let parsed =
            parse_source("outer <- function(flag) { if (flag) x <- 1; function() x <<- 2 }");
        let effect = parsed.expressions[0]
            .effects
            .iter()
            .find(|effect| effect.target.as_deref() == Some("x"))
            .expect("superassignment effect");
        assert!(!effect.target_enclosing_local);
    }

    #[test]
    fn uncaptured_superassignment_remains_unresolved() {
        let parsed = parse_source("outer <- function() { function() x <<- 1 }");
        let effect = parsed.expressions[0]
            .effects
            .iter()
            .find(|effect| effect.target.as_deref() == Some("x"))
            .expect("superassignment effect");
        assert!(!effect.target_enclosing_local);
    }

    #[test]
    fn current_function_local_does_not_satisfy_superassignment() {
        let parsed = parse_source("outer <- function() { x <- 1; x <<- 2 }");
        let effect = parsed.expressions[0]
            .effects
            .iter()
            .find(|effect| effect.target.as_deref() == Some("x"))
            .expect("superassignment effect");
        assert!(!effect.target_enclosing_local);
    }

    #[test]
    fn current_function_parameter_does_not_satisfy_superassignment() {
        let parsed = parse_source("outer <- function(x) { x <<- 2 }");
        let effect = parsed.expressions[0]
            .effects
            .iter()
            .find(|effect| effect.target.as_deref() == Some("x"))
            .expect("superassignment effect");
        assert!(!effect.target_enclosing_local);
    }

    #[test]
    fn enclosing_function_parameter_satisfies_superassignment() {
        let parsed = parse_source("outer <- function(x) { function() x <<- 2 }");
        let effect = parsed.expressions[0]
            .effects
            .iter()
            .find(|effect| effect.target.as_deref() == Some("x"))
            .expect("superassignment effect");
        assert!(effect.target_enclosing_local);
    }

    #[test]
    fn sibling_function_binding_does_not_satisfy_superassignment() {
        let parsed = parse_source(
            "outer <- function() { sibling <- function() { x <- 1 }; function() x <<- 2 }",
        );
        let effect = parsed.expressions[0]
            .effects
            .iter()
            .find(|effect| effect.target.as_deref() == Some("x"))
            .expect("superassignment effect");
        assert!(!effect.target_enclosing_local);
    }

    #[test]
    fn dominating_branch_binding_satisfies_nested_superassignment() {
        let parsed =
            parse_source("outer <- function(flag) { if (flag) { x <- 1; function() x <<- 2 } }");
        let effect = parsed.expressions[0]
            .effects
            .iter()
            .find(|effect| effect.target.as_deref() == Some("x"))
            .expect("superassignment effect");
        assert!(effect.target_enclosing_local);
    }

    #[test]
    fn assignment_dominates_later_use_inside_same_branch() {
        let parsed =
            parse_source("f <- function(flag) { if (flag) { helper <- function() 1; helper() } }");
        assert!(!reference_names(&parsed).contains(&"helper"));
    }

    #[test]
    fn branch_assignment_does_not_dominate_use_after_branch() {
        let parsed =
            parse_source("f <- function(flag) { if (flag) helper <- function() 1; helper() }");
        assert!(reference_names(&parsed).contains(&"helper"));
    }

    #[test]
    fn local_recursive_closure_sees_its_completed_binding() {
        let parsed = parse_source(
            "f <- function(flag) { if (flag) { recurse <- function(x) if (x) recurse(FALSE); recurse(TRUE) } }",
        );
        assert!(!reference_names(&parsed).contains(&"recurse"));
    }

    #[test]
    fn repeated_boolean_guard_preserves_exhaustive_inner_assignment() {
        let parsed = parse_source(
            "f <- function(enabled, choose_first) { if (enabled) { if (choose_first) value <- 1 else value <- 2 }; if (enabled) print(value) }",
        );
        assert!(!reference_names(&parsed).contains(&"value"));
    }

    #[test]
    fn boolean_alias_correlates_equivalent_null_guard() {
        let parsed = parse_source(
            "f <- function(obj) { present <- !is.null(obj$field); if (!is.null(obj$field)) value <- 1; if (present) print(value) }",
        );
        assert!(!reference_names(&parsed).contains(&"value"));
    }

    #[test]
    fn captured_conditional_binding_is_safe_under_same_stable_guard() {
        let parsed = parse_source(
            "f <- function(enabled, choose_first) { if (enabled) { if (choose_first) callback <- function() 1 else callback <- function() 2 }; invoke <- function() { if (enabled) callback() }; invoke() }",
        );
        assert!(!reference_names(&parsed).contains(&"callback"));
    }

    #[test]
    fn captured_exhaustive_binding_is_safe_under_same_outer_guard() {
        let parsed = parse_source(
            "f <- function(deep, choose_first) { if (deep) { if (choose_first) callback <- function() 1 else callback <- function() 2 }; invoke <- function() { if (deep) callback() }; invoke() }",
        );
        assert!(!reference_names(&parsed).contains(&"callback"));
    }

    #[test]
    fn captured_exhaustive_binding_handles_compound_inner_condition() {
        let parsed = parse_source(
            "f <- function(deep, has_private, candidate) { if (deep) { if (has_private && is.function(candidate)) callback <- candidate else callback <- function() 2 }; invoke <- function() { if (deep) mapply(callback, 1) }; invoke() }",
        );
        assert!(!reference_names(&parsed).contains(&"callback"));
    }

    #[test]
    fn mutated_guard_does_not_validate_captured_conditional_binding() {
        let parsed = parse_source(
            "f <- function(enabled) { if (enabled) callback <- function() 1; invoke <- function() { if (enabled) callback() }; enabled <- !enabled; invoke() }",
        );
        assert!(reference_names(&parsed).contains(&"callback"));
    }

    #[test]
    fn descendant_local_shadow_does_not_mutate_captured_guard() {
        let parsed = parse_source(
            "f <- function(enabled) { if (enabled) callback <- function() 1; shadow <- function() enabled <- FALSE; invoke <- function() { if (enabled) callback() }; invoke() }",
        );
        assert!(!reference_names(&parsed).contains(&"callback"));
    }

    #[test]
    fn rejecting_guard_makes_following_membership_dispatch_exhaustive() {
        let parsed = parse_source(
            r#"f <- function(which, function_value) {
                if (is.null(which) || !(which %in% c("public", "private", "active"))) stop("bad")
                if (which == "public") group <- "public_methods"
                else if (which == "private") group <- "private_methods"
                else if (which == "active") {
                    if (function_value) group <- "active" else stop("bad")
                }
                print(group)
            }"#,
        );
        assert!(!reference_names(&parsed).contains(&"group"));
    }

    #[test]
    fn rejecting_guard_handles_value_assignments_in_membership_dispatch() {
        let parsed = parse_source(
            r#"f <- function(which, value) {
                if (is.null(which) || !(which %in% c("public", "private", "active"))) stop("bad")
                if (which == "public") {
                    group <- if (is.function(value)) "public_methods" else "public_fields"
                } else if (which == "private") {
                    group <- if (is.function(value)) "private_methods" else "private_fields"
                } else if (which == "active") {
                    if (is.function(value)) group <- "active" else stop("bad")
                }
                print(group)
            }"#,
        );
        assert!(!reference_names(&parsed).contains(&"group"));
    }

    #[test]
    fn three_level_capture_resolves_outer_activation_binding() {
        let parsed = parse_source(
            "a <- function() { x <- 1; b <- function() { c <- function() x <<- 2; c }; b() }",
        );
        let effect = parsed.expressions[0]
            .effects
            .iter()
            .find(|effect| effect.target.as_deref() == Some("x"))
            .expect("superassignment effect");
        assert!(effect.target_enclosing_local);
    }

    #[test]
    fn language_constants_do_not_become_external_bindings() {
        let parsed = parse_source(
            "f <- function() list(NULL, TRUE, FALSE, NA, NaN, Inf, NA_integer_, NA_real_, NA_complex_, NA_character_)",
        );
        let names = reference_names(&parsed);
        for constant in [
            "NULL",
            "TRUE",
            "FALSE",
            "NA",
            "NaN",
            "Inf",
            "NA_integer_",
            "NA_real_",
            "NA_complex_",
            "NA_character_",
        ] {
            assert!(
                !names.contains(&constant),
                "language constant {constant} leaked as a reference"
            );
        }
    }

    #[test]
    fn call_argument_span_matches_selector_name_reference() {
        let parsed = parse_source("f <- function(x) .Call(.NAME = croot_f, x)");
        let expression = &parsed.expressions[0];
        let reference = expression
            .references
            .iter()
            .find(|reference| reference.name == "croot_f")
            .expect("selector reference");
        let call = expression
            .calls
            .iter()
            .find(|call| call.callee == ".Call")
            .expect("native call");
        let selector = call
            .arg_names
            .iter()
            .position(|name| name.as_deref() == Some(".NAME"))
            .expect("named selector");

        assert_eq!(
            call.args[selector],
            Some(StaticArg::Symbol("croot_f".into()))
        );
        assert_eq!(call.arg_spans[selector].as_ref(), Some(&reference.span));
    }

    #[test]
    fn call_argument_records_definite_local_closure_identity() {
        let parsed = parse_source(
            "f <- function() { callback <- function(x) x; .Call(native_call, 1, callback) }",
        );
        let call = parsed.expressions[0]
            .calls
            .iter()
            .find(|call| call.callee == ".Call")
            .expect("native call");

        assert_eq!(call.local_closure_args, [false, false, true]);
    }

    #[test]
    fn construction_facts_preserve_order_and_target_shapes() {
        let parsed = parse_source(
            "f <- function(template, parent) { env <- new.env(parent = parent); env$self <- env; environment(template) <- env; list2env(template, envir = env) }",
        );
        let construction = &parsed.expressions[0].construction;

        assert_eq!(construction.len(), 4);
        assert!(matches!(
            &construction[0].kind,
            ConstructionExprKind::Assign {
                target: ConstructionTarget::Local { name },
                value,
            } if name == "env" && matches!(
                &value.kind,
                ConstructionExprKind::Call { call } if call.callee == "new.env"
            )
        ));
        assert!(matches!(
            &construction[1].kind,
            ConstructionExprKind::Assign {
                target: ConstructionTarget::Member { name: Some(name), .. },
                ..
            } if name == "self"
        ));
        assert!(matches!(
            &construction[2].kind,
            ConstructionExprKind::Assign {
                target: ConstructionTarget::ClosureEnvironment { .. },
                ..
            }
        ));
        assert!(matches!(
            &construction[3].kind,
            ConstructionExprKind::Call { call } if call.callee == "list2env"
        ));
    }

    #[test]
    fn reenclosure_helper_construction_shape() {
        let parsed = parse_source(
            "assign_func_envs <- function(objs, target_env) { if (is.null(target_env)) return(objs); lapply(objs, function(x) { if (is.function(x)) environment(x) <- target_env; x }) }",
        );
        assert!(matches!(
            &parsed.expressions[0].construction[1].kind,
            ConstructionExprKind::Call { call }
                if call.callee == "lapply"
                    && matches!(
                        call.arguments.get(1).and_then(|argument| argument.value.as_ref()).map(|value| &value.kind),
                        Some(ConstructionExprKind::Function { .. })
                    )
        ));
    }

    #[test]
    fn exhaustive_equality_chain_correlates_later_else_branch() {
        let parsed = parse_source(
            r#"f <- function(alternative) {
                if (alternative == "less") {
                    rr <- 1
                } else if (alternative == "greater") {
                    rr <- 2
                } else if (alternative == "two.sided") {
                    lowerrr <- 3
                    upperrr <- 4
                } else {
                    stop("bad alternative")
                }
                if (alternative == "less") rr
                else if (alternative == "greater") rr
                else lowerrr + upperrr
            }"#,
        );
        let names = reference_names(&parsed);
        assert!(!names.contains(&"rr"));
        assert!(!names.contains(&"lowerrr"));
        assert!(!names.contains(&"upperrr"));
    }

    #[test]
    fn non_returning_summary_handles_multi_statement_terminal_stop() {
        let context = OakParseContext::default();
        assert!(closure_definitely_non_returning(
            "function(x) { message <- x; stop(message) }",
            &context,
        ));
    }

    #[test]
    fn non_returning_summary_handles_exhaustive_terminating_if() {
        let context = OakParseContext::default();
        assert!(closure_definitely_non_returning(
            "function(flag) { if (flag) stop('a') else base::stop('b') }",
            &context,
        ));
    }

    #[test]
    fn explicit_return_prevents_never_returns_summary() {
        let context = OakParseContext::default();
        assert!(!closure_definitely_non_returning(
            "function(flag) { if (flag) return(1); stop('otherwise') }",
            &context,
        ));
    }

    #[test]
    fn later_import_from_replaces_an_earlier_one() {
        let mut imports = NamespaceImports::default();
        imports.add_import_from("first", [("target".to_owned(), "first_target".to_owned())]);
        imports.add_import_from(
            "second",
            [("target".to_owned(), "second_target".to_owned())],
        );

        assert_eq!(
            imports.resolve("target"),
            NamespaceImportResolution::Imported {
                package: "second".to_owned(),
                binding: "second_target".to_owned(),
                effect_name: "second_target".to_owned(),
            }
        );
    }

    #[test]
    fn later_import_all_replaces_an_earlier_import_from() {
        let mut imports = NamespaceImports::default();
        imports.add_import_from("first", [("target".to_owned(), "target".to_owned())]);
        imports.add_import_all(
            "later",
            Some(BTreeMap::from([
                ("target".to_owned(), "target_impl".to_owned()),
                (".onLoad".to_owned(), ".onLoad".to_owned()),
            ])),
            Vec::<String>::new(),
        );

        assert_eq!(
            imports.resolve("target"),
            NamespaceImportResolution::Imported {
                package: "later".to_owned(),
                binding: "target_impl".to_owned(),
                effect_name: "target".to_owned(),
            }
        );
        assert_eq!(
            imports.resolve(".onLoad"),
            NamespaceImportResolution::BaseFallback
        );
        assert_eq!(
            imports
                .names()
                .map(|names| names.into_keys().collect::<Vec<_>>()),
            Ok(vec!["target".to_owned()])
        );
    }

    #[test]
    fn missing_import_all_blocks_only_names_no_later_import_provides() {
        let mut imports = NamespaceImports::default();
        imports.add_import_all("missing", None, Vec::<String>::new());
        imports.add_import_all(
            "later",
            Some(BTreeMap::from([("target".to_owned(), "target".to_owned())])),
            Vec::<String>::new(),
        );

        assert_eq!(
            imports.resolve("target"),
            NamespaceImportResolution::Imported {
                package: "later".to_owned(),
                binding: "target".to_owned(),
                effect_name: "target".to_owned(),
            }
        );
        assert_eq!(
            imports.resolve("other"),
            NamespaceImportResolution::MissingImportAll {
                package: "missing".to_owned(),
                binding: "other".to_owned(),
            }
        );
        assert_eq!(imports.names(), Err("missing"));
    }
}
