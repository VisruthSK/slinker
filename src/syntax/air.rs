//! Thin Air adapter for R syntax.
//!
//! Air owns parsing. This module extracts the semantic facts the reachability
//! analysis needs without leaking Air nodes into the rest of the crate.

use crate::syntax::source::{SourceId, Span};
use crate::{Error, Result};
use air_r_parser::{parse, RParserOptions};
use air_r_syntax::{
    AnyRArgumentName, AnyRExpression, AnyRParameterName, AnyRSelector, RArgumentList,
    RCall, RParameterList, RStringValue,
};
use biome_rowan::AstNode;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BindingDef {
    pub name: String,
    pub span: Span,
    pub certainty: BindingCertainty,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BindingCertainty {
    Definite,
    Possible,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NameRef {
    pub name: String,
    pub phase: EvalPhase,
    pub guards: Vec<String>,
    pub span: Span,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvalPhase {
    Materialization,
    Runtime,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackageRef {
    pub package: String,
    pub symbol: String,
    pub internal: bool,
    pub guards: Vec<String>,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallSite {
    pub callee: String,
    pub callee_local: bool,
    pub args: Vec<Option<StaticArg>>,
    pub phase: EvalPhase,
    pub guards: Vec<String>,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResourceRef {
    pub package: Option<String>,
    /// Installed-package-relative resource path when every path component is
    /// statically known. An empty string means the package root itself.
    pub path: Option<String>,
    /// `Some(false)` includes the ordinary default. `None` means a supplied
    /// `mustWork` expression is dynamic and cannot be specialized safely.
    pub must_work: Option<bool>,
    pub guards: Vec<String>,
    pub span: Span,
}


#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum StaticArg {
    String(String),
    Symbol(String),
}


#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SyntaxEffectKind {
    /// `<<-` or `->>` can mutate an enclosing environment.
    SuperAssignment,
    /// A package-frame assignment occurs beneath a call / unknown lazy context.
    /// Whether it executes depends on evaluation semantics we do not model.
    IndirectPackageWrite,
    /// An assignment target could not be reduced to a package/local binding.
    UnsupportedAssignmentTarget,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyntaxEffect {
    pub kind: SyntaxEffectKind,
    pub phase: EvalPhase,
    pub guards: Vec<String>,
    pub span: Span,
}

/// Facts owned by one top-level source expression.
///
/// A unit may define more than one package binding. R control-flow constructs
/// do not introduce lexical environments, so e.g. `for (x in xs) y <- x`
/// potentially writes both `x` and `y` into the package frame.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParsedExpression {
    pub span: Span,
    pub definitions: Vec<BindingDef>,
    pub references: Vec<NameRef>,
    pub package_refs: Vec<PackageRef>,
    pub resource_refs: Vec<ResourceRef>,
    pub calls: Vec<CallSite>,
    pub effects: Vec<SyntaxEffect>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParsedRFile {
    pub expressions: Vec<ParsedExpression>,
}

impl ParsedRFile {
    pub fn bindings(&self) -> impl Iterator<Item = &BindingDef> {
        self.expressions.iter().flat_map(|expr| expr.definitions.iter())
    }
}

pub trait RParser {
    fn parse(&self, source: SourceId, text: &str) -> Result<ParsedRFile>;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct AirParser;

impl AirParser {
    pub fn parse_binding(
        &self,
        source: SourceId,
        text: &str,
    ) -> std::result::Result<ParsedRFile, String> {
        let parsed = parse(text, RParserOptions::default());
        if let Some(error) = parsed.error() {
            return Err(error.to_string());
        }

        let mut expressions = Vec::new();
        for expression in &parsed.tree().expressions() {
            let span = node_span(&source, &expression);
            let mut collector = Collector::new(source.clone());
            let mut flow = Flow::default();
            collector
                .visit(&expression, &mut flow, EvalContext::Direct)
                .map_err(|error| error.to_string())?;
            expressions.push(collector.finish(span));
        }
        Ok(ParsedRFile { expressions })
    }
}

impl RParser for AirParser {
    fn parse(&self, source: SourceId, text: &str) -> Result<ParsedRFile> {
        self.parse_binding(source.clone(), text).map_err(|message| Error::Parse {
            path: format!("source:{}", source.0),
            message,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EvalContext {
    /// A package-frame write is unconditionally reached by this structural path.
    Direct,
    /// A package-frame write may execute, for example inside a branch or loop.
    Conditional,
    /// Evaluation is nested below an ordinary call / unknown lazy boundary.
    Deferred,
}

impl EvalContext {
    fn conditional(self) -> Self {
        match self {
            Self::Direct | Self::Conditional => Self::Conditional,
            Self::Deferred => Self::Deferred,
        }
    }
}

/// Definite lexical bindings for active function scopes.
///
/// We only suppress a package reference when a local binding is definitely
/// present. Ambiguous flow intentionally over-retains rather than under-retains.
#[derive(Clone, Default)]
struct Flow {
    functions: Vec<BTreeSet<String>>,
}

impl Flow {
    fn in_function(&self) -> bool {
        !self.functions.is_empty()
    }

    fn phase(&self) -> EvalPhase {
        if self.in_function() {
            EvalPhase::Runtime
        } else {
            EvalPhase::Materialization
        }
    }

    fn is_definitely_local(&self, name: &str) -> bool {
        self.functions.iter().rev().any(|scope| scope.contains(name))
    }

    fn define_local(&mut self, name: String) {
        if let Some(scope) = self.functions.last_mut() {
            scope.insert(name);
        }
    }

    fn push_function(&mut self, parameters: BTreeSet<String>) {
        self.functions.push(parameters);
    }

    fn pop_function(&mut self) {
        self.functions.pop();
    }

    fn merge_if(&mut self, left: &Flow, right: &Flow) {
        debug_assert_eq!(left.functions.len(), right.functions.len());
        debug_assert_eq!(self.functions.len(), left.functions.len());
        for ((dst, lhs), rhs) in self
            .functions
            .iter_mut()
            .zip(&left.functions)
            .zip(&right.functions)
        {
            *dst = lhs.intersection(rhs).cloned().collect();
        }
    }
}

struct Collector {
    source: SourceId,
    definitions: Vec<BindingDef>,
    references: Vec<NameRef>,
    package_refs: Vec<PackageRef>,
    resource_refs: Vec<ResourceRef>,
    calls: Vec<CallSite>,
    effects: Vec<SyntaxEffect>,
    guards: Vec<String>,
}

impl Collector {
    fn new(source: SourceId) -> Self {
        Self {
            source,
            definitions: Vec::new(),
            references: Vec::new(),
            package_refs: Vec::new(),
            resource_refs: Vec::new(),
            calls: Vec::new(),
            effects: Vec::new(),
            guards: Vec::new(),
        }
    }

    fn finish(self, span: Span) -> ParsedExpression {
        ParsedExpression {
            span,
            definitions: self.definitions,
            references: self.references,
            package_refs: self.package_refs,
            resource_refs: self.resource_refs,
            calls: self.calls,
            effects: self.effects,
        }
    }

    fn visit(
        &mut self,
        expression: &AnyRExpression,
        flow: &mut Flow,
        context: EvalContext,
    ) -> Result<()> {
        match expression {
            AnyRExpression::RBinaryExpression(node) => {
                let left = node.left().map_err(|_| malformed("binary left"))?;
                let right = node.right().map_err(|_| malformed("binary right"))?;
                let operator = node
                    .operator()
                    .map_err(|_| malformed("binary operator"))?
                    .text_trimmed()
                    .to_string();

                match operator.as_str() {
                    "<-" | "=" => {
                        // R evaluates the RHS before installing the binding.
                        self.visit(&right, flow, context)?;
                        self.assignment_target(&left, flow, context)?;
                    }
                    "->" => {
                        self.visit(&left, flow, context)?;
                        self.assignment_target(&right, flow, context)?;
                    }
                    "<<-" | "->>" => {
                        // Superassignment never creates a binding in the current
                        // evaluation frame. It searches enclosing environments.
                        let value = if operator == "<<-" { &right } else { &left };
                        self.visit(value, flow, context)?;
                        self.effects.push(SyntaxEffect {
                            kind: SyntaxEffectKind::SuperAssignment,
                            phase: flow.phase(),
                            guards: self.guards.clone(),
                            span: node_span(&self.source, node),
                        });
                    }
                    _ => {
                        // Operators are lexical function references in R. Missing
                        // this edge under-retains imported operators such as `%>%`
                        // and package-local operator overrides.
                        self.reference_name(
                            operator,
                            node_span(&self.source, node),
                            flow,
                        );

                        // Operands may be promises for user-defined infix
                        // operators, so writes nested inside them are not direct.
                        self.visit(&left, flow, EvalContext::Deferred)?;
                        self.visit(&right, flow, EvalContext::Deferred)?;
                    }
                }
            }
            AnyRExpression::RBracedExpressions(node) => {
                for child in &node.expressions() {
                    self.visit(&child, flow, context)?;
                }
            }
            AnyRExpression::RCall(node) => self.visit_call(node, flow)?,
            AnyRExpression::RExtractExpression(node) => {
                let operator = node
                    .operator()
                    .map_err(|_| malformed("extract operator"))?
                    .text_trimmed()
                    .to_string();
                self.reference_name(operator, node_span(&self.source, node), flow);
                self.visit(
                    &node.left().map_err(|_| malformed("extract left"))?,
                    flow,
                    EvalContext::Deferred,
                )?;
            }
            AnyRExpression::RForStatement(node) => {
                self.visit(
                    &node.sequence().map_err(|_| malformed("for sequence"))?,
                    flow,
                    EvalContext::Deferred,
                )?;

                let variable = node.variable().map_err(|_| malformed("for variable"))?;
                let name = identifier_name(&variable)?;

                if flow.in_function() {
                    // `for` does not create a scope, but the sequence can be
                    // empty. The loop variable is therefore local while the body
                    // executes, not definitely local after the loop.
                    let mut body_flow = flow.clone();
                    body_flow.define_local(name);
                    self.visit(
                        &node.body().map_err(|_| malformed("for body"))?,
                        &mut body_flow,
                        context,
                    )?;
                } else {
                    // At package top level the loop variable is a possible write
                    // to the package frame. Reachability treats possible local
                    // bindings as non-exclusive so an imported fallback is also
                    // retained when one exists.
                    let loop_context = context.conditional();
                    self.write_name(
                        name,
                        node_span(&self.source, &variable),
                        flow,
                        loop_context,
                    );
                    self.visit(
                        &node.body().map_err(|_| malformed("for body"))?,
                        flow,
                        loop_context,
                    )?;
                }
            }
            AnyRExpression::RFunctionDefinition(node) => {
                let parameters = node
                    .parameters()
                    .map_err(|_| malformed("function parameters"))?;
                let names = parameter_names(&parameters.items())?;
                flow.push_function(names);

                // All formal bindings exist in the function evaluation frame, so
                // defaults resolve against formals before outer/package bindings.
                for parameter in &parameters.items() {
                    let parameter = parameter.map_err(|_| malformed("parameter"))?;
                    if let Some(default) = parameter.default() {
                        self.visit(
                            &default.value().map_err(|_| malformed("parameter default"))?,
                            flow,
                            EvalContext::Deferred,
                        )?;
                    }
                }
                self.visit(
                    &node.body().map_err(|_| malformed("function body"))?,
                    flow,
                    EvalContext::Direct,
                )?;
                flow.pop_function();
            }
            AnyRExpression::RIdentifier(identifier) => {
                let name = identifier_name(identifier)?;
                self.reference_name(name, node_span(&self.source, identifier), flow);
            }
            AnyRExpression::RIfStatement(node) => {
                let condition = node.condition().map_err(|_| malformed("if condition"))?;
                let optional_guard = if flow.is_definitely_local("requireNamespace") {
                    None
                } else {
                    require_namespace_guard(&condition)
                };
                self.visit(
                    &condition,
                    flow,
                    EvalContext::Deferred,
                )?;

                let before = flow.clone();
                let branch_context = if flow.in_function() {
                    context
                } else {
                    context.conditional()
                };
                let mut yes = before.clone();
                if let Some(package) = optional_guard.as_ref() {
                    self.guards.push(package.clone());
                }
                self.visit(
                    &node.consequence().map_err(|_| malformed("if consequence"))?,
                    &mut yes,
                    branch_context,
                )?;
                if optional_guard.is_some() {
                    self.guards.pop();
                }

                let mut no = before.clone();
                if let Some(clause) = node.else_clause() {
                    self.visit(
                        &clause
                            .alternative()
                            .map_err(|_| malformed("else alternative"))?,
                        &mut no,
                        branch_context,
                    )?;
                }
                flow.merge_if(&yes, &no);
            }
            AnyRExpression::RNamespaceExpression(node) => {
                let package = selector_text(
                    &node.left().map_err(|_| malformed("namespace package"))?,
                );
                let symbol = selector_text(
                    &node.right().map_err(|_| malformed("namespace symbol"))?,
                );
                let operator = node
                    .operator()
                    .map_err(|_| malformed("namespace operator"))?
                    .text_trimmed()
                    .to_string();
                if let (Some(package), Some(symbol)) = (package, symbol) {
                    self.package_refs.push(PackageRef {
                        package,
                        symbol,
                        internal: operator == ":::",
                        guards: self.guards.clone(),
                        span: node_span(&self.source, node),
                    });
                }
            }
            AnyRExpression::RParenthesizedExpression(node) => {
                self.visit(
                    &node.body().map_err(|_| malformed("parenthesized body"))?,
                    flow,
                    context,
                )?;
            }
            AnyRExpression::RRepeatStatement(node) => {
                let before = flow.clone();
                let mut body = before.clone();
                let body_context = if flow.in_function() {
                    context
                } else {
                    context.conditional()
                };
                self.visit(
                    &node.body().map_err(|_| malformed("repeat body"))?,
                    &mut body,
                    body_context,
                )?;
                // Do not assume any new local is definitely bound after the loop:
                // control can break before a particular assignment.
                *flow = before;
            }
            AnyRExpression::RSubset(node) => {
                self.reference_name("[".into(), node_span(&self.source, node), flow);
                self.visit(
                    &node.function().map_err(|_| malformed("subset function"))?,
                    flow,
                    EvalContext::Deferred,
                )?;
                let arguments = node.arguments().map_err(|_| malformed("subset arguments"))?;
                self.visit_arguments(&arguments.items(), flow)?;
            }
            AnyRExpression::RSubset2(node) => {
                self.reference_name("[[".into(), node_span(&self.source, node), flow);
                self.visit(
                    &node.function().map_err(|_| malformed("subset2 function"))?,
                    flow,
                    EvalContext::Deferred,
                )?;
                let arguments = node.arguments().map_err(|_| malformed("subset2 arguments"))?;
                self.visit_arguments(&arguments.items(), flow)?;
            }
            AnyRExpression::RUnaryExpression(node) => {
                let operator = node
                    .operator()
                    .map_err(|_| malformed("unary operator"))?
                    .text_trimmed()
                    .to_string();
                self.reference_name(operator, node_span(&self.source, node), flow);
                self.visit(
                    &node.argument().map_err(|_| malformed("unary argument"))?,
                    flow,
                    EvalContext::Deferred,
                )?;
            }
            AnyRExpression::RWhileStatement(node) => {
                self.visit(
                    &node.condition().map_err(|_| malformed("while condition"))?,
                    flow,
                    EvalContext::Deferred,
                )?;
                let before = flow.clone();
                let mut body = before.clone();
                let body_context = if flow.in_function() {
                    context
                } else {
                    context.conditional()
                };
                self.visit(
                    &node.body().map_err(|_| malformed("while body"))?,
                    &mut body,
                    body_context,
                )?;
                *flow = before;
            }
            AnyRExpression::AnyRValue(_)
            | AnyRExpression::RBogusExpression(_)
            | AnyRExpression::RBreakExpression(_)
            | AnyRExpression::RDotDotI(_)
            | AnyRExpression::RDots(_)
            | AnyRExpression::RFalseExpression(_)
            | AnyRExpression::RInfExpression(_)
            | AnyRExpression::RNaExpression(_)
            | AnyRExpression::RNanExpression(_)
            | AnyRExpression::RNextExpression(_)
            | AnyRExpression::RNullExpression(_)
            | AnyRExpression::RTrueExpression(_) => {}
        }
        Ok(())
    }

    fn visit_call(&mut self, call: &RCall, flow: &mut Flow) -> Result<()> {
        let function = call.function().map_err(|_| malformed("call function"))?;
        let arguments = call.arguments().map_err(|_| malformed("call arguments"))?;

        if let Some(callee) = callable_name(&function) {
            if callee == "system.file" {
                self.resource_refs.push(ResourceRef {
                    package: named_string(&arguments.items(), "package"),
                    path: system_file_path(&arguments.items()),
                    must_work: named_logical_or_default(&arguments.items(), "mustWork", false),
                    guards: self.guards.clone(),
                    span: node_span(&self.source, call),
                });
            }
            self.calls.push(CallSite {
                callee_local: flow.is_definitely_local(&callee),
                callee,
                args: static_call_args(&arguments.items()),
                phase: flow.phase(),
                guards: self.guards.clone(),
                span: node_span(&self.source, call),
            });
        }

        self.visit(&function, flow, EvalContext::Deferred)?;
        self.visit_arguments(&arguments.items(), flow)
    }

    fn visit_arguments(&mut self, arguments: &RArgumentList, flow: &mut Flow) -> Result<()> {
        for argument in arguments {
            let argument = argument.map_err(|_| malformed("argument"))?;
            if let Some(value) = argument.value() {
                self.visit(&value, flow, EvalContext::Deferred)?;
            }
        }
        Ok(())
    }

    fn reference_name(&mut self, name: String, span: Span, flow: &Flow) {
        if !flow.is_definitely_local(&name) {
            self.references.push(NameRef {
                name,
                phase: flow.phase(),
                guards: self.guards.clone(),
                span,
            });
        }
    }

    fn assignment_target(
        &mut self,
        target: &AnyRExpression,
        flow: &mut Flow,
        context: EvalContext,
    ) -> Result<()> {
        if let Some(identifier) = target.as_r_identifier() {
            let name = identifier_name(identifier)?;
            self.write_name(name, node_span(&self.source, identifier), flow, context);
            return Ok(());
        }

        // A replacement assignment is an implicit call to one or more setter
        // functions followed by a rebind of the base object. Record both the
        // expressions that R evaluates to obtain the target and every setter in
        // a nested replacement chain. Missing either class can under-retain a
        // dependency.
        let Some(name) = replacement_base(target)? else {
            self.effects.push(SyntaxEffect {
                kind: SyntaxEffectKind::UnsupportedAssignmentTarget,
                phase: flow.phase(),
                guards: self.guards.clone(),
                span: node_span(&self.source, target),
            });
            return Ok(());
        };

        self.visit_replacement_inputs(target, flow)?;
        self.reference_replacement_setters(target, flow)?;
        self.write_name(name, node_span(&self.source, target), flow, context);
        Ok(())
    }

    fn visit_replacement_inputs(
        &mut self,
        target: &AnyRExpression,
        flow: &mut Flow,
    ) -> Result<()> {
        if let Some(paren) = target.as_r_parenthesized_expression() {
            return self.visit_replacement_inputs(
                &paren.body().map_err(|_| malformed("replacement paren"))?,
                flow,
            );
        }
        if let Some(subset) = target.as_r_subset() {
            self.visit(
                &subset.function().map_err(|_| malformed("replacement subset object"))?,
                flow,
                EvalContext::Deferred,
            )?;
            let arguments = subset
                .arguments()
                .map_err(|_| malformed("replacement subset arguments"))?;
            return self.visit_arguments(&arguments.items(), flow);
        }
        if let Some(subset) = target.as_r_subset2() {
            self.visit(
                &subset
                    .function()
                    .map_err(|_| malformed("replacement subset2 object"))?,
                flow,
                EvalContext::Deferred,
            )?;
            let arguments = subset
                .arguments()
                .map_err(|_| malformed("replacement subset2 arguments"))?;
            return self.visit_arguments(&arguments.items(), flow);
        }
        if let Some(extract) = target.as_r_extract_expression() {
            return self.visit(
                &extract.left().map_err(|_| malformed("replacement extract object"))?,
                flow,
                EvalContext::Deferred,
            );
        }
        if let Some(call) = target.as_r_call() {
            // For `setter(x, i) <- value`, R calls `setter<-`; it does not first
            // call `setter`. Only the target arguments are inputs here. A nested
            // call used as the object of another replacement is visited normally
            // by the outer case above, because there it really is a getter.
            let arguments = call
                .arguments()
                .map_err(|_| malformed("replacement call arguments"))?;
            return self.visit_arguments(&arguments.items(), flow);
        }
        Ok(())
    }

    fn reference_replacement_setters(
        &mut self,
        target: &AnyRExpression,
        flow: &Flow,
    ) -> Result<()> {
        if let Some(paren) = target.as_r_parenthesized_expression() {
            return self.reference_replacement_setters(
                &paren.body().map_err(|_| malformed("replacement paren"))?,
                flow,
            );
        }
        if let Some(subset) = target.as_r_subset() {
            self.reference_name("[<-".into(), node_span(&self.source, subset), flow);
            return self.reference_replacement_setters(
                &subset.function().map_err(|_| malformed("replacement subset object"))?,
                flow,
            );
        }
        if let Some(subset) = target.as_r_subset2() {
            self.reference_name("[[<-".into(), node_span(&self.source, subset), flow);
            return self.reference_replacement_setters(
                &subset
                    .function()
                    .map_err(|_| malformed("replacement subset2 object"))?,
                flow,
            );
        }
        if let Some(extract) = target.as_r_extract_expression() {
            let operator = extract
                .operator()
                .map_err(|_| malformed("replacement extract operator"))?
                .text_trimmed()
                .to_string();
            self.reference_name(
                format!("{operator}<-"),
                node_span(&self.source, extract),
                flow,
            );
            return self.reference_replacement_setters(
                &extract.left().map_err(|_| malformed("replacement extract object"))?,
                flow,
            );
        }
        if let Some(call) = target.as_r_call() {
            let function = call
                .function()
                .map_err(|_| malformed("replacement function"))?;
            let Some(callee) = callable_name(&function) else {
                self.effects.push(SyntaxEffect {
                    kind: SyntaxEffectKind::UnsupportedAssignmentTarget,
                    phase: flow.phase(),
                    guards: self.guards.clone(),
                    span: node_span(&self.source, call),
                });
                return Ok(());
            };
            self.reference_name(
                format!("{callee}<-"),
                node_span(&self.source, call),
                flow,
            );

            let arguments = call
                .arguments()
                .map_err(|_| malformed("replacement call arguments"))?;
            if let Some(first) = arguments.items().into_iter().next() {
                let first = first.map_err(|_| malformed("replacement first argument"))?;
                if let Some(value) = first.value() {
                    return self.reference_replacement_setters(&value, flow);
                }
            }
        }
        Ok(())
    }

    fn write_name(&mut self, name: String, span: Span, flow: &mut Flow, context: EvalContext) {
        if flow.in_function() {
            if context == EvalContext::Direct {
                flow.define_local(name);
            }
            return;
        }

        let certainty = match context {
            EvalContext::Direct => BindingCertainty::Definite,
            EvalContext::Conditional => BindingCertainty::Possible,
            EvalContext::Deferred => {
                self.effects.push(SyntaxEffect {
                    kind: SyntaxEffectKind::IndirectPackageWrite,
                    phase: EvalPhase::Materialization,
                    guards: self.guards.clone(),
                    span,
                });
                return;
            }
        };

        if let Some(existing) = self.definitions.iter_mut().find(|def| def.name == name) {
            if certainty == BindingCertainty::Definite {
                existing.certainty = BindingCertainty::Definite;
            }
            return;
        }

        self.definitions.push(BindingDef {
            name,
            span,
            certainty,
        });
    }
}

fn malformed(site: &'static str) -> Error {
    Error::Parse {
        path: "Air AST".into(),
        message: format!("malformed node at {site}"),
    }
}

fn node_span<N: AstNode>(source: &SourceId, node: &N) -> Span {
    let range = node.syntax().text_trimmed_range();
    Span::new(
        source.clone(),
        usize::from(range.start()),
        usize::from(range.end()),
    )
}

fn identifier_name(identifier: &air_r_syntax::RIdentifier) -> Result<String> {
    let text = identifier
        .name_token()
        .map(|token| token.text_trimmed().to_string())
        .map_err(|_| malformed("identifier"))?;
    normalize_identifier(text.clone()).ok_or_else(|| {
        Error::Analysis(format!(
            "escaped backtick identifier is not supported: {text}"
        ))
    })
}

fn normalize_identifier(text: String) -> Option<String> {
    if text.starts_with('`') && text.ends_with('`') && text.len() >= 2 {
        let inner = &text[1..text.len() - 1];
        return (!inner.contains('\\')).then(|| inner.to_string());
    }
    Some(text)
}

fn parameter_names(parameters: &RParameterList) -> Result<BTreeSet<String>> {
    let mut out = BTreeSet::new();
    for parameter in parameters {
        let parameter = parameter.map_err(|_| malformed("parameter"))?;
        let name = parameter.name().map_err(|_| malformed("parameter name"))?;
        if let AnyRParameterName::RIdentifier(identifier) = name {
            out.insert(identifier_name(&identifier)?);
        }
    }
    Ok(out)
}

fn replacement_base(expression: &AnyRExpression) -> Result<Option<String>> {
    if let Some(identifier) = expression.as_r_identifier() {
        return Ok(Some(identifier_name(identifier)?));
    }
    if let Some(paren) = expression.as_r_parenthesized_expression() {
        return replacement_base(&paren.body().map_err(|_| malformed("replacement paren"))?);
    }
    if let Some(subset) = expression.as_r_subset() {
        return replacement_base(
            &subset.function().map_err(|_| malformed("replacement subset"))?,
        );
    }
    if let Some(subset) = expression.as_r_subset2() {
        return replacement_base(
            &subset
                .function()
                .map_err(|_| malformed("replacement subset2"))?,
        );
    }
    if let Some(extract) = expression.as_r_extract_expression() {
        return replacement_base(&extract.left().map_err(|_| malformed("replacement extract"))?);
    }
    if let Some(call) = expression.as_r_call() {
        // `names(x) <- value` and other replacement functions rewrite the first
        // object argument. Only accept a statically recoverable base binding.
        let args = call.arguments().map_err(|_| malformed("replacement call"))?;
        let Some(first) = args.items().into_iter().next() else {
            return Ok(None);
        };
        let first = first.map_err(|_| malformed("replacement first argument"))?;
        let Some(value) = first.value() else {
            return Ok(None);
        };
        return replacement_base(&value);
    }
    Ok(None)
}

fn require_namespace_guard(expression: &AnyRExpression) -> Option<String> {
    let call = expression.as_r_call()?;
    let function = call.function().ok()?;
    if callable_name(&function).as_deref() != Some("requireNamespace") {
        return None;
    }
    let arguments = call.arguments().ok()?;
    let args = static_call_args(&arguments.items());
    match args.first()?.as_ref()? {
        StaticArg::String(package) | StaticArg::Symbol(package) => Some(package.clone()),
    }
}

fn callable_name(expression: &AnyRExpression) -> Option<String> {
    if let Some(identifier) = expression.as_r_identifier() {
        return identifier
            .name_token()
            .ok()
            .and_then(|token| normalize_identifier(token.text_trimmed().to_string()));
    }

    let namespace = expression.as_r_namespace_expression()?;
    let package = selector_text(&namespace.left().ok()?)?;
    let symbol = selector_text(&namespace.right().ok()?)?;
    (package == "base").then_some(symbol)
}

fn selector_text(selector: &AnyRSelector) -> Option<String> {
    match selector {
        AnyRSelector::RIdentifier(identifier) => identifier
            .name_token()
            .ok()
            .and_then(|token| normalize_identifier(token.text_trimmed().to_string())),
        AnyRSelector::RStringValue(value) => string_value(value),
        AnyRSelector::RDotDotI(_) | AnyRSelector::RDots(_) => None,
    }
}

fn string_value(value: &RStringValue) -> Option<String> {
    let text = value
        .content_token()
        .map(|token| token.text_trimmed().to_string())
        .unwrap_or_default();
    (!text.contains('\\')).then_some(text)
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

fn named_logical_or_default(arguments: &RArgumentList, name: &str, default: bool) -> Option<bool> {
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
        return argument.value().and_then(|value| logical_expression(&value));
    }
    Some(default)
}

fn logical_expression(expression: &AnyRExpression) -> Option<bool> {
    match expression {
        AnyRExpression::RTrueExpression(_) => Some(true),
        AnyRExpression::RFalseExpression(_) => Some(false),
        _ => None,
    }
}

fn argument_name(name: &AnyRArgumentName) -> Option<String> {
    match name {
        AnyRArgumentName::RIdentifier(identifier) => identifier
            .name_token()
            .ok()
            .and_then(|token| normalize_identifier(token.text_trimmed().to_string())),
        AnyRArgumentName::RStringValue(value) => string_value(value),
        AnyRArgumentName::RDotDotI(_)
        | AnyRArgumentName::RDots(_)
        | AnyRArgumentName::RNullExpression(_) => None,
    }
}

fn string_expression(expression: &AnyRExpression) -> Option<String> {
    expression
        .as_any_r_value()?
        .as_r_string_value()
        .and_then(string_value)
}

fn system_file_path(arguments: &RArgumentList) -> Option<String> {
    let mut components = Vec::new();
    for argument in arguments {
        let argument = argument.ok()?;
        if argument.name_clause().is_some() {
            continue;
        }
        let value = argument.value()?;
        components.push(string_expression(&value)?);
    }
    Some(components.join("/"))
}

fn static_call_args(arguments: &RArgumentList) -> Vec<Option<StaticArg>> {
    arguments
        .into_iter()
        .filter_map(std::result::Result::ok)
        .map(|argument| argument.value().and_then(|value| static_expression(&value)))
        .collect()
}

fn static_expression(expression: &AnyRExpression) -> Option<StaticArg> {
    if let Some(identifier) = expression.as_r_identifier() {
        return identifier
            .name_token()
            .ok()
            .and_then(|token| normalize_identifier(token.text_trimmed().to_string()))
            .map(StaticArg::Symbol);
    }
    expression
        .as_any_r_value()?
        .as_r_string_value()
        .and_then(string_value)
        .map(StaticArg::String)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_one(source: &str) -> ParsedExpression {
        AirParser
            .parse(SourceId(0), source)
            .unwrap()
            .expressions
            .into_iter()
            .next()
            .unwrap()
    }

    fn defs(source: &str) -> BTreeSet<String> {
        parse_one(source)
            .definitions
            .into_iter()
            .map(|d| d.name)
            .collect()
    }

    fn refs(source: &str) -> BTreeSet<String> {
        parse_one(source)
            .references
            .into_iter()
            .map(|r| r.name)
            .collect()
    }

    #[test]
    fn top_level_for_variable_is_a_package_binding() {
        assert_eq!(defs("for (x in xs) y <- x"), BTreeSet::from(["x".into(), "y".into()]));
    }

    #[test]
    fn control_flow_does_not_create_a_scope() {
        assert_eq!(
            defs("if (flag) { x <- 1 } else { y <- 2 }"),
            BTreeSet::from(["x".into(), "y".into()])
        );
        assert_eq!(defs("while (flag) { z <- 1 }"), BTreeSet::from(["z".into()]));
        assert_eq!(defs("repeat { q <- 1; break }"), BTreeSet::from(["q".into()]));
    }

    #[test]
    fn top_level_binding_certainty_tracks_control_flow() {
        let direct = parse_one("x <- 1");
        assert_eq!(direct.definitions[0].certainty, BindingCertainty::Definite);

        let conditional = parse_one("if (flag) x <- 1");
        assert_eq!(
            conditional.definitions[0].certainty,
            BindingCertainty::Possible
        );

        let looped = parse_one("for (x in xs) y <- x");
        assert!(looped
            .definitions
            .iter()
            .all(|definition| definition.certainty == BindingCertainty::Possible));
    }

    #[test]
    fn chained_assignments_share_one_top_level_unit() {
        assert_eq!(defs("x <- y <- value"), BTreeSet::from(["x".into(), "y".into()]));
    }

    #[test]
    fn function_locals_are_not_package_bindings() {
        assert_eq!(defs("f <- function(x) { y <- x; for (i in x) z <- i }"), BTreeSet::from(["f".into()]));
    }

    #[test]
    fn function_parameters_shadow_package_bindings() {
        let got = refs("f <- function(x) x + package_value");
        assert!(!got.contains("x"));
        assert!(got.contains("package_value"));
    }

    #[test]
    fn use_before_local_assignment_keeps_outer_reference() {
        let got = refs("f <- function() { print(x); x <- 1 }");
        assert!(got.contains("x"));
    }

    #[test]
    fn replacement_assignment_reads_and_rebinds_base() {
        let parsed = parse_one("x[1] <- value");
        assert!(parsed.definitions.iter().any(|d| d.name == "x"));
        assert!(parsed.references.iter().any(|r| r.name == "x"));
    }

    #[test]
    fn superassignment_is_an_effect_not_a_local_definition() {
        let parsed = parse_one("f <- function() x <<- 1");
        assert_eq!(
            parsed.definitions.iter().map(|d| d.name.as_str()).collect::<Vec<_>>(),
            vec!["f"]
        );
        assert!(parsed.effects.iter().any(|e| e.kind == SyntaxEffectKind::SuperAssignment));
    }

    #[test]
    fn package_write_under_unknown_call_is_rejected_not_missed() {
        let parsed = parse_one("identity(x <- 1)");
        assert!(parsed.definitions.is_empty());
        assert!(parsed.effects.iter().any(|e| e.kind == SyntaxEffectKind::IndirectPackageWrite));
    }

    #[test]
    fn namespace_access_is_not_an_unqualified_reference() {
        let parsed = parse_one("f <- function(x) foo::bar(x)");
        assert!(parsed.package_refs.iter().any(|r| r.package == "foo" && r.symbol == "bar"));
        assert!(!parsed.references.iter().any(|r| r.name == "foo" || r.name == "bar"));
    }

    #[test]
    fn require_namespace_if_consequence_carries_optional_guard() {
        let parsed = parse_one("f <- function() if (requireNamespace(\"foo\")) foo::bar()");
        let reference = parsed.package_refs.iter().find(|r| r.package == "foo" && r.symbol == "bar").unwrap();
        assert_eq!(reference.guards, vec!["foo"]);
        let condition = parsed.calls.iter().find(|call| call.callee == "requireNamespace").unwrap();
        assert!(condition.guards.is_empty());
    }
    #[test]
    fn empty_function_for_loop_does_not_hide_outer_binding() {
        let got = refs("f <- function(xs) { for (x in xs) {}; x }");
        assert!(got.contains("x"));
    }

    #[test]
    fn operators_are_lexical_references() {
        let got = refs("f <- function(x) x %pipe% 1");
        assert!(got.contains("%pipe%"));

        let got = refs("f <- function(x) -x");
        assert!(got.contains("-"));

        assert!(defs("`%pipe%` <- function(x, y) x").contains("%pipe%"));
    }

    #[test]
    fn static_call_arguments_distinguish_strings_from_symbols() {
        let string = parse_one(r#"requireNamespace("foo")"#);
        assert!(matches!(
            string.calls[0].args[0],
            Some(StaticArg::String(ref value)) if value == "foo"
        ));

        let symbol = parse_one("require(foo)");
        assert!(matches!(
            symbol.calls[0].args[0],
            Some(StaticArg::Symbol(ref value)) if value == "foo"
        ));
    }

    #[test]
    fn top_level_and_function_references_have_distinct_phases() {
        let top = parse_one("y <- x");
        assert!(top.references.iter().any(|r| {
            r.name == "x" && r.phase == EvalPhase::Materialization
        }));

        let runtime = parse_one("f <- function() x");
        assert!(runtime.references.iter().any(|r| {
            r.name == "x" && r.phase == EvalPhase::Runtime
        }));
    }

    #[test]
    fn replacement_assignment_retains_inputs_and_setters() {
        let parsed = parse_one("x[index()] <- value");
        let names: BTreeSet<_> = parsed.references.iter().map(|r| r.name.as_str()).collect();
        assert!(names.contains("x"));
        assert!(names.contains("index"));
        assert!(names.contains("[<-"));

        let parsed = parse_one("decorate(x) <- value");
        let names: BTreeSet<_> = parsed.references.iter().map(|r| r.name.as_str()).collect();
        assert!(names.contains("x"));
        assert!(names.contains("decorate<-"));

        let parsed = parse_one("x$a[1] <- value");
        let names: BTreeSet<_> = parsed.references.iter().map(|r| r.name.as_str()).collect();
        assert!(names.contains("x"));
        assert!(names.contains("$"));
        assert!(names.contains("$<-"));
        assert!(names.contains("[<-"));
    }

    #[test]
    fn system_file_records_static_package_resource() {
        let parsed = parse_one(r#"f <- function() system.file("data", "x.json", package = "foo")"#);
        let resource = parsed.resource_refs.first().unwrap();
        assert_eq!(resource.package.as_deref(), Some("foo"));
        assert_eq!(resource.path.as_deref(), Some("data/x.json"));
        assert_eq!(resource.must_work, Some(false));
    }

}
