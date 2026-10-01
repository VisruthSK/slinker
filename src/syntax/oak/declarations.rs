use super::context::OakParseContext;
use super::scan::static_arg;
use super::{LexicalScopes, ast_span, ast_text, identifier_callee, text_offset};
use crate::syntax::facts::{
    BindingDeclaration, DeclaredCallable, DeclaredDomain, LexicalScopeId, SemanticIssue,
    SemanticIssueKind, StaticArg,
};
use crate::syntax::source::SourceId;
use air_r_syntax::{AnyRExpression, RCall, RRoot};
use biome_rowan::{AstNode, AstSeparatedList};
use oak_semantic::semantic_index::{ScopeId, SemanticIndex};
use std::collections::BTreeSet;

pub(super) struct Declarations {
    pub(super) declarations: Vec<BindingDeclaration>,
    pub(super) inert: Vec<std::ops::Range<usize>>,
    pub(super) issues: Vec<SemanticIssue>,
}

impl Declarations {
    pub(super) fn is_inert(&self, offset: usize) -> bool {
        self.inert.iter().any(|range| range.contains(&offset))
    }
}

pub(super) fn collect_declarations(
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

pub(super) fn collect_slinker_declaration(
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

pub(super) fn declared_domain(text: &str, value: &AnyRExpression) -> Option<DeclaredDomain> {
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

pub(super) fn declared_callable(text: &str) -> Option<DeclaredCallable> {
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

pub(super) fn declared_classes(text: &str, value: &AnyRExpression) -> Option<Vec<Vec<String>>> {
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

pub(super) fn declaration_call(value: &AnyRExpression) -> Option<(String, Vec<AnyRExpression>)> {
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

pub(super) fn literal_strings(text: &str, arguments: &[AnyRExpression]) -> Option<Vec<String>> {
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

pub(super) fn sole_positional_argument(call: &RCall) -> Option<AnyRExpression> {
    let arguments = call.arguments().ok()?.items();
    let mut items = arguments.iter();
    let argument = items.next()?.ok()?;
    if items.next().is_some() || argument.name_clause().is_some() {
        return None;
    }
    argument.value()
}
