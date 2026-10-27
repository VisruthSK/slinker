use super::census::{Census, argument_name, node_name, static_arg_of};
use super::context::OakParseContext;
use super::{LexicalScopes, ast_span, identifier_callee, text_offset};
use crate::package::{BindingName, PackageName};
use crate::syntax::facts::{
    BindingDeclaration, DeclaredCallable, DeclaredDomain, LexicalScopeId, SemanticIssue,
    SemanticIssueKind, StaticArg,
};
use crate::syntax::source::SourceId;
use air_r_syntax::{AnyRExpression, RCall};
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
    census: &Census,
    context: &OakParseContext,
    index: &SemanticIndex,
    scopes: &LexicalScopes,
) -> Declarations {
    let mut collected = Declarations {
        declarations: Vec::new(),
        inert: Vec::new(),
        issues: Vec::new(),
    };
    for call in &census.calls {
        let start = text_offset(call.syntax().text_trimmed_range().start());
        let (scope, lexical) = scopes.at(index, start);
        let Ok(function) = call.function() else {
            continue;
        };
        let is_declare = match &function {
            AnyRExpression::RIdentifier(_) => identifier_callee(call).is_some_and(|callee| {
                callee.as_str() == "declare"
                    && context.resolves_to_base(callee.as_str())
                    && index.resolve(callee.as_str(), scope).is_none()
            }),
            AnyRExpression::RNamespaceExpression(namespace) => {
                namespace
                    .left()
                    .ok()
                    .and_then(|name| node_name(&name))
                    .as_deref()
                    == Some("base")
                    && namespace
                        .right()
                        .ok()
                        .and_then(|name| node_name(&name))
                        .as_deref()
                        == Some("declare")
            }
            _ => false,
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
            if identifier_callee(&language).is_none_or(|callee| callee.as_str() != "slinker") {
                continue;
            }
            collect_slinker_declaration(
                source,
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
            .and_then(|name| argument_name(&name));
        let domain = argument.value().and_then(|value| declared_domain(&value));
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

pub(super) fn declared_domain(value: &AnyRExpression) -> Option<DeclaredDomain> {
    let (callee, arguments) = declaration_call(value)?;
    match callee.as_str() {
        "strings" => literal_strings(&arguments)
            .map(|strings| DeclaredDomain::Strings(strings.into_iter().collect())),
        "s3" | "one_of" => declared_classes(value).map(DeclaredDomain::Classes),
        "callables" if !arguments.is_empty() => arguments
            .iter()
            .map(declared_callable)
            .collect::<Option<BTreeSet<_>>>()
            .map(DeclaredDomain::Callables),
        _ => None,
    }
}

pub(super) fn declared_callable(value: &AnyRExpression) -> Option<DeclaredCallable> {
    let (package, name) = match value {
        AnyRExpression::RNamespaceExpression(namespace) => (
            Some(node_name(&namespace.left().ok()?)?),
            node_name(&namespace.right().ok()?)?,
        ),
        AnyRExpression::RIdentifier(identifier) => (None, node_name(identifier)?),
        _ => return None,
    };
    Some(DeclaredCallable {
        package: package.map(PackageName::from),
        name: BindingName::from(name),
    })
}

pub(super) fn declared_classes(value: &AnyRExpression) -> Option<Vec<Vec<String>>> {
    let (callee, arguments) = declaration_call(value)?;
    match callee.as_str() {
        "s3" => literal_strings(&arguments).map(|classes| vec![classes]),
        "one_of" if !arguments.is_empty() => arguments
            .iter()
            .map(declared_classes)
            .collect::<Option<Vec<_>>>()
            .map(|alternatives| alternatives.into_iter().flatten().collect()),
        _ => None,
    }
}

pub(super) fn declaration_call(value: &AnyRExpression) -> Option<(String, Vec<AnyRExpression>)> {
    let AnyRExpression::RCall(call) = value else {
        return None;
    };
    let callee = identifier_callee(call)?;
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

pub(super) fn literal_strings(arguments: &[AnyRExpression]) -> Option<Vec<String>> {
    if arguments.is_empty() {
        return None;
    }
    arguments
        .iter()
        .map(|argument| match static_arg_of(argument) {
            Some(StaticArg::String(value)) => Some(value.as_str().to_owned()),
            Some(StaticArg::Symbol(_) | StaticArg::Logical(_)) | None => None,
        })
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
