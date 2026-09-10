//! Thin Air front-end.
//!
//! This module extracts syntax facts only. It does not decide package ownership,
//! lexical resolution, reachability, or whether a transformation is legal.

use std::fmt;
use std::fs;
use std::path::PathBuf;

use crate::ConfiguredSourceView;

use air_r_parser::{RParserOptions, parse};
use biome_rowan::AstNode;
use air_r_syntax::{
    AnyRArgumentName, AnyRExpression, AnyRSelector, RArgumentList, RCall, RParameterList,
    RStringValue,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SourceFact {
    NamespaceAccess {
        package: String,
        symbol: String,
        internal: bool,
    },
    DynamicPackageLookup {
        function: String,
        package: Option<String>,
    },
    SystemFile {
        package: Option<String>,
    },
    SyntaxObservation {
        function: String,
    },
}


#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ByteSpan {
    pub start: usize,
    pub end: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocatedSourceFact {
    pub span: ByteSpan,
    pub fact: SourceFact,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ScanError {
    Parse(String),
    MalformedAst(&'static str),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceFileFacts {
    pub path: PathBuf,
    pub facts: Vec<SourceFact>,
    pub located: Vec<LocatedSourceFact>,
}

#[derive(Debug)]
pub enum PackageScanError {
    Io { path: PathBuf, source: std::io::Error },
    Scan { path: PathBuf, source: ScanError },
}

/// Parse the configured source view, never the original pre-configure tree.
/// This is the package-level Air entrypoint used after target-toolchain staging.
pub fn scan_configured(view: &ConfiguredSourceView) -> Result<Vec<SourceFileFacts>, PackageScanError> {
    let mut files = Vec::new();
    for relative in &view.files {
        let mut components = relative.components();
        if components.next().and_then(|part| part.as_os_str().to_str()) != Some("R") {
            continue;
        }
        let extension = relative.extension().and_then(|value| value.to_str()).unwrap_or_default();
        if !matches!(extension.to_ascii_lowercase().as_str(), "r" | "s" | "q") {
            continue;
        }
        let path = view.root.join(relative);
        let source = fs::read_to_string(&path).map_err(|source| PackageScanError::Io { path: relative.clone(), source })?;
        let located = scan_located(&source).map_err(|source| PackageScanError::Scan { path: relative.clone(), source })?;
        let facts = located.iter().map(|located| located.fact.clone()).collect();
        files.push(SourceFileFacts { path: relative.clone(), facts, located });
    }
    Ok(files)
}

pub fn scan(source: &str) -> Result<Vec<SourceFact>, ScanError> {
    Ok(scan_located(source)?
        .into_iter()
        .map(|located| located.fact)
        .collect())
}

pub fn scan_located(source: &str) -> Result<Vec<LocatedSourceFact>, ScanError> {
    let parsed = parse(source, RParserOptions::default());
    if let Some(error) = parsed.error() {
        return Err(ScanError::Parse(error.to_string()));
    }

    let mut scanner = Scanner::default();
    for expression in &parsed.tree().expressions() {
        scanner.visit(&expression)?;
    }
    Ok(scanner.facts)
}

#[derive(Default)]
struct Scanner {
    facts: Vec<LocatedSourceFact>,
}

impl Scanner {
    fn visit(&mut self, expression: &AnyRExpression) -> Result<(), ScanError> {
        match expression {
            AnyRExpression::RBinaryExpression(node) => {
                self.visit(&node.left().map_err(|_| ScanError::MalformedAst("binary left"))?)?;
                self.visit(&node.right().map_err(|_| ScanError::MalformedAst("binary right"))?)?;
            }
            AnyRExpression::RBracedExpressions(node) => {
                for expression in &node.expressions() {
                    self.visit(&expression)?;
                }
            }
            AnyRExpression::RCall(node) => self.visit_call(node)?,
            AnyRExpression::RExtractExpression(node) => {
                self.visit(&node.left().map_err(|_| ScanError::MalformedAst("extract left"))?)?;
            }
            AnyRExpression::RForStatement(node) => {
                self.visit(
                    &node.sequence().map_err(|_| ScanError::MalformedAst("for sequence"))?,
                )?;
                self.visit(&node.body().map_err(|_| ScanError::MalformedAst("for body"))?)?;
            }
            AnyRExpression::RFunctionDefinition(node) => {
                let parameters = node
                    .parameters()
                    .map_err(|_| ScanError::MalformedAst("function parameters"))?;
                self.visit_parameters(&parameters.items())?;
                self.visit(
                    &node.body().map_err(|_| ScanError::MalformedAst("function body"))?,
                )?;
            }
            AnyRExpression::RIfStatement(node) => {
                self.visit(
                    &node.condition().map_err(|_| ScanError::MalformedAst("if condition"))?,
                )?;
                self.visit(
                    &node
                        .consequence()
                        .map_err(|_| ScanError::MalformedAst("if consequence"))?,
                )?;
                if let Some(clause) = node.else_clause() {
                    self.visit(
                        &clause
                            .alternative()
                            .map_err(|_| ScanError::MalformedAst("else alternative"))?,
                    )?;
                }
            }
            AnyRExpression::RNamespaceExpression(node) => {
                let package = selector_text(
                    &node.left().map_err(|_| ScanError::MalformedAst("namespace package"))?,
                );
                let symbol = selector_text(
                    &node
                        .right()
                        .map_err(|_| ScanError::MalformedAst("namespace symbol"))?,
                );
                let operator = node
                    .operator()
                    .map_err(|_| ScanError::MalformedAst("namespace operator"))?;

                if let (Some(package), Some(symbol)) = (package, symbol) {
                    self.push(
                        byte_span(node),
                        SourceFact::NamespaceAccess {
                            package,
                            symbol,
                            internal: operator.text_trimmed().to_string() == ":::",
                        },
                    );
                }
            }
            AnyRExpression::RParenthesizedExpression(node) => {
                self.visit(
                    &node.body().map_err(|_| ScanError::MalformedAst("parenthesized body"))?,
                )?;
            }
            AnyRExpression::RRepeatStatement(node) => {
                self.visit(&node.body().map_err(|_| ScanError::MalformedAst("repeat body"))?)?;
            }
            AnyRExpression::RSubset(node) => {
                self.visit(
                    &node.function().map_err(|_| ScanError::MalformedAst("subset function"))?,
                )?;
                let arguments = node
                    .arguments()
                    .map_err(|_| ScanError::MalformedAst("subset arguments"))?;
                self.visit_arguments(&arguments.items())?;
            }
            AnyRExpression::RSubset2(node) => {
                self.visit(
                    &node
                        .function()
                        .map_err(|_| ScanError::MalformedAst("subset2 function"))?,
                )?;
                let arguments = node
                    .arguments()
                    .map_err(|_| ScanError::MalformedAst("subset2 arguments"))?;
                self.visit_arguments(&arguments.items())?;
            }
            AnyRExpression::RUnaryExpression(node) => {
                self.visit(
                    &node.argument().map_err(|_| ScanError::MalformedAst("unary argument"))?,
                )?;
            }
            AnyRExpression::RWhileStatement(node) => {
                self.visit(
                    &node
                        .condition()
                        .map_err(|_| ScanError::MalformedAst("while condition"))?,
                )?;
                self.visit(
                    &node.body().map_err(|_| ScanError::MalformedAst("while body"))?,
                )?;
            }
            AnyRExpression::AnyRValue(_)
            | AnyRExpression::RBogusExpression(_)
            | AnyRExpression::RBreakExpression(_)
            | AnyRExpression::RDotDotI(_)
            | AnyRExpression::RDots(_)
            | AnyRExpression::RFalseExpression(_)
            | AnyRExpression::RIdentifier(_)
            | AnyRExpression::RInfExpression(_)
            | AnyRExpression::RNaExpression(_)
            | AnyRExpression::RNanExpression(_)
            | AnyRExpression::RNextExpression(_)
            | AnyRExpression::RNullExpression(_)
            | AnyRExpression::RTrueExpression(_) => {}
        }
        Ok(())
    }

    fn visit_call(&mut self, call: &RCall) -> Result<(), ScanError> {
        let function = call
            .function()
            .map_err(|_| ScanError::MalformedAst("call function"))?;
        let arguments = call
            .arguments()
            .map_err(|_| ScanError::MalformedAst("call arguments"))?;

        if let Some(name) = callable_name(&function) {
            if is_syntax_observer(&name) {
                self.push(
                    byte_span(call),
                    SourceFact::SyntaxObservation { function: name.clone() },
                );
            }

            if is_dynamic_package_lookup(&name) {
                self.push(
                    byte_span(call),
                    SourceFact::DynamicPackageLookup {
                        function: name.clone(),
                        package: package_argument(&name, &arguments.items()),
                    },
                );
            }

            if name == "system.file" {
                self.push(
                    byte_span(call),
                    SourceFact::SystemFile {
                        package: named_string(&arguments.items(), "package"),
                    },
                );
            }
        }

        self.visit(&function)?;
        self.visit_arguments(&arguments.items())
    }

    fn push(&mut self, span: ByteSpan, fact: SourceFact) {
        self.facts.push(LocatedSourceFact { span, fact });
    }

    fn visit_arguments(&mut self, arguments: &RArgumentList) -> Result<(), ScanError> {
        for argument in arguments {
            let argument = argument.map_err(|_| ScanError::MalformedAst("argument"))?;
            if let Some(value) = argument.value() {
                self.visit(&value)?;
            }
        }
        Ok(())
    }

    fn visit_parameters(&mut self, parameters: &RParameterList) -> Result<(), ScanError> {
        for parameter in parameters {
            let parameter = parameter.map_err(|_| ScanError::MalformedAst("parameter"))?;
            if let Some(default) = parameter.default() {
                self.visit(
                    &default
                        .value()
                        .map_err(|_| ScanError::MalformedAst("parameter default"))?,
                )?;
            }
        }
        Ok(())
    }
}

fn byte_span(node: &impl AstNode) -> ByteSpan {
    let range = node.syntax().text_trimmed_range();
    ByteSpan {
        start: usize::from(range.start()),
        end: usize::from(range.end()),
    }
}

fn callable_name(expression: &AnyRExpression) -> Option<String> {
    if let Some(identifier) = expression.as_r_identifier() {
        return Some(identifier.name_token().ok()?.text_trimmed().to_string());
    }

    let namespace = expression.as_r_namespace_expression()?;
    let package = selector_text(&namespace.left().ok()?)?;
    if package != "base" {
        return None;
    }
    selector_text(&namespace.right().ok()?)
}

fn selector_text(selector: &AnyRSelector) -> Option<String> {
    match selector {
        AnyRSelector::RIdentifier(identifier) => {
            Some(identifier.name_token().ok()?.text_trimmed().to_string())
        }
        AnyRSelector::RStringValue(value) => string_value(value),
        AnyRSelector::RDotDotI(_) | AnyRSelector::RDots(_) => None,
    }
}

fn string_value(value: &RStringValue) -> Option<String> {
    let text = value
        .content_token()
        .map(|token| token.text_trimmed().to_string())
        .unwrap_or_default();
    if text.contains('\\') {
        return None;
    }
    Some(text)
}

fn package_argument(function: &str, arguments: &RArgumentList) -> Option<String> {
    let argument = arguments.into_iter().next()?.ok()?;
    let value = argument.value()?;
    if let Some(package) = string_expression(&value) {
        return Some(package);
    }
    if matches!(function, "library" | "require") {
        return value
            .as_r_identifier()
            .and_then(|identifier| identifier.name_token().ok())
            .map(|token| token.text_trimmed().to_string());
    }
    None
}

fn named_string(arguments: &RArgumentList, name: &str) -> Option<String> {
    for argument in arguments {
        let argument = argument.ok()?;
        let Some(clause) = argument.name_clause() else {
            continue;
        };
        let Some(argument_name) = clause.name().ok().and_then(|name| argument_name(&name)) else {
            continue;
        };
        if argument_name != name {
            continue;
        }
        return argument.value().and_then(|value| string_expression(&value));
    }
    None
}

fn argument_name(name: &AnyRArgumentName) -> Option<String> {
    match name {
        AnyRArgumentName::RIdentifier(identifier) => {
            Some(identifier.name_token().ok()?.text_trimmed().to_string())
        }
        AnyRArgumentName::RStringValue(value) => string_value(value),
        AnyRArgumentName::RDotDotI(_)
        | AnyRArgumentName::RDots(_)
        | AnyRArgumentName::RNullExpression(_) => None,
    }
}

fn string_expression(expression: &AnyRExpression) -> Option<String> {
    let value = expression.as_any_r_value()?.as_r_string_value()?;
    string_value(value)
}

fn is_syntax_observer(name: &str) -> bool {
    matches!(name, "substitute" | "formals" | "body" | "deparse")
}

fn is_dynamic_package_lookup(name: &str) -> bool {
    matches!(
        name,
        "requireNamespace" | "require" | "library" | "packageVersion" | "find.package"
    )
}

impl fmt::Display for ScanError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Parse(error) => write!(f, "R parse error: {error}"),
            Self::MalformedAst(site) => write!(f, "malformed Air AST at {site}"),
        }
    }
}

impl std::error::Error for ScanError {}

impl fmt::Display for PackageScanError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, source } => write!(f, "failed to read configured R source {}: {source}", path.display()),
            Self::Scan { path, source } => write!(f, "failed to scan configured R source {}: {source}", path.display()),
        }
    }
}

impl std::error::Error for PackageScanError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Scan { source, .. } => Some(source),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{SourceFact, scan};

    #[test]
    fn finds_namespace_accesses_and_sensitive_calls() {
        let facts = scan(
            r#"
            f <- function(x) {
                foo::bar(x)
                foo:::private(x)
                system.file("data", package = "foo")
                deparse(substitute(x))
                requireNamespace("bar")
            }
            "#,
        )
        .unwrap();

        assert!(facts.contains(&SourceFact::NamespaceAccess {
            package: "foo".into(),
            symbol: "bar".into(),
            internal: false,
        }));
        assert!(facts.contains(&SourceFact::NamespaceAccess {
            package: "foo".into(),
            symbol: "private".into(),
            internal: true,
        }));
        assert!(facts.contains(&SourceFact::SystemFile {
            package: Some("foo".into()),
        }));
        assert!(facts.contains(&SourceFact::DynamicPackageLookup {
            function: "requireNamespace".into(),
            package: Some("bar".into()),
        }));
        assert!(facts.contains(&SourceFact::SyntaxObservation {
            function: "deparse".into(),
        }));
        assert!(facts.contains(&SourceFact::SyntaxObservation {
            function: "substitute".into(),
        }));
    }
}

#[cfg(test)]
mod located_tests {
    use super::{SourceFact, scan_located};

    #[test]
    fn facts_keep_air_owned_byte_ranges() {
        let source = "f <- function() foo::bar()\n";
        let facts = scan_located(source).unwrap();
        let namespace = facts
            .iter()
            .find(|fact| matches!(fact.fact, SourceFact::NamespaceAccess { .. }))
            .unwrap();
        assert_eq!(&source[namespace.span.start..namespace.span.end], "foo::bar");
    }
}
