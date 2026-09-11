//! Linker-facing semantic facts.
//!
//! Oak owns lexical semantics. These are slinker transport types only: they
//! describe semantic uses that Oak has already classified as live and capable
//! of escaping local lexical scopes, plus linker-specific package/resource
//! effects layered on those live sites.

use crate::syntax::source::Span;
use serde::{Deserialize, Serialize};

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

/// Whether an Oak-live use can escape the syntax file's lexical scopes.
///
/// `NameRef` values are emitted only for the two fallthrough cases. A use that
/// Oak proves locally bound never reaches the linker name resolver.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NameRefKind {
    External,
    ConditionalFallthrough,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NameRef {
    pub name: String,
    pub kind: NameRefKind,
    pub phase: EvalPhase,
    pub guards: Vec<PackageGuard>,
    pub span: Span,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvalPhase {
    Materialization,
    Runtime,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "package", rename_all = "snake_case")]
pub enum PackageGuard {
    Available(String),
    Loaded(String),
    Selected(String),
}

impl PackageGuard {
    pub fn package(&self) -> &str {
        match self {
            Self::Available(package) | Self::Loaded(package) | Self::Selected(package) => package,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackageRef {
    pub package: String,
    pub symbol: String,
    pub internal: bool,
    pub guards: Vec<PackageGuard>,
    pub span: Span,
}

/// Oak's lexical classification for a live callee use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CalleeKind {
    DefinitelyLexical,
    DefinitelyExternal,
    ConditionalFallthrough,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallSite {
    pub callee: String,
    pub callee_kind: CalleeKind,
    /// `Some(package)` only for an explicitly namespace-qualified callee. Bare
    /// callees are resolved by the linker against the installed namespace.
    pub qualified_package: Option<String>,
    pub args: Vec<Option<StaticArg>>,
    /// Syntactic argument names aligned with `args`; `None` denotes a positional argument.
    #[serde(default)]
    pub arg_names: Vec<Option<String>>,
    pub phase: EvalPhase,
    pub guards: Vec<PackageGuard>,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResourceRef {
    pub package: Option<String>,
    pub path: Option<String>,
    pub must_work: Option<bool>,
    pub guards: Vec<PackageGuard>,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum StaticArg {
    String(String),
    Symbol(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum StaticEnvironment {
    ClosureBinding(String),
    Namespace(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActiveBindingDef {
    pub name: String,
    pub target: StaticEnvironment,
    pub certain: bool,
    pub guards: Vec<PackageGuard>,
    pub span: Span,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SyntaxEffectKind {
    SuperAssignment,
    IndirectPackageWrite,
    UnsupportedAssignmentTarget,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyntaxEffect {
    pub kind: SyntaxEffectKind,
    pub target: Option<String>,
    pub target_enclosing_local: bool,
    pub value_symbol: Option<String>,
    pub phase: EvalPhase,
    pub guards: Vec<PackageGuard>,
    pub span: Span,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SemanticIssueKind {
    AmbiguousEffect,
    AmbiguousAttachOrder,
    UninstalledPackage,
    SourceCycle,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SemanticIssue {
    pub kind: SemanticIssueKind,
    pub message: String,
    pub span: Option<Span>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParsedExpression {
    pub span: Span,
    pub definitions: Vec<BindingDef>,
    pub references: Vec<NameRef>,
    pub package_refs: Vec<PackageRef>,
    pub resource_refs: Vec<ResourceRef>,
    pub calls: Vec<CallSite>,
    pub active_bindings: Vec<ActiveBindingDef>,
    pub effects: Vec<SyntaxEffect>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParsedRFile {
    pub expressions: Vec<ParsedExpression>,
    #[serde(default)]
    pub issues: Vec<SemanticIssue>,
}

impl ParsedRFile {
    pub fn bindings(&self) -> impl Iterator<Item = &BindingDef> {
        self.expressions
            .iter()
            .flat_map(|expr| expr.definitions.iter())
    }
}
