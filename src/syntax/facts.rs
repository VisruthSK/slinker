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
    MaybeLocal,
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
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
    /// Source extents aligned with `args`; omitted arguments have no extent.
    #[serde(default)]
    pub arg_spans: Vec<Option<Span>>,
    /// Whether Oak proves the aligned symbol argument is a locally assigned closure.
    #[serde(default)]
    pub local_closure_args: Vec<bool>,
    #[serde(default)]
    pub scope: LexicalScopeId,
    #[serde(default)]
    pub arg_bindings: Vec<Option<LexicalBindingId>>,
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

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ConstructionExpr {
    pub kind: ConstructionExprKind,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ConstructionExprKind {
    Unknown,
    Null,
    Logical {
        value: bool,
    },
    Integer {
        value: i64,
    },
    Double {
        value: String,
    },
    String {
        value: String,
    },
    Symbol {
        name: String,
    },
    Sequence {
        expressions: Vec<ConstructionExpr>,
    },
    Call {
        call: ConstructionCall,
    },
    Member {
        object: Box<ConstructionExpr>,
        name: Option<String>,
    },
    Index {
        object: Box<ConstructionExpr>,
        index: Box<ConstructionExpr>,
    },
    Assign {
        target: ConstructionTarget,
        value: Box<ConstructionExpr>,
    },
    If {
        condition: Box<ConstructionExpr>,
        consequence: Box<ConstructionExpr>,
        alternative: Option<Box<ConstructionExpr>>,
    },
    Function {
        parameters: Vec<String>,
        body: Box<ConstructionExpr>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ConstructionCall {
    pub callee: String,
    pub callee_kind: CalleeKind,
    pub qualified_package: Option<String>,
    pub arguments: Vec<ConstructionArgument>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ConstructionArgument {
    pub name: Option<String>,
    pub value: Option<ConstructionExpr>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ConstructionTarget {
    Local {
        name: String,
    },
    Member {
        object: Box<ConstructionExpr>,
        name: Option<String>,
    },
    ClosureEnvironment {
        closure: Box<ConstructionExpr>,
    },
    Unknown,
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
    InvalidDeclaration,
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
    #[serde(default)]
    pub parameters: Vec<String>,
    #[serde(default)]
    pub used_parameters: Vec<String>,
    pub definitions: Vec<BindingDef>,
    pub references: Vec<NameRef>,
    pub package_refs: Vec<PackageRef>,
    pub resource_refs: Vec<ResourceRef>,
    pub calls: Vec<CallSite>,
    pub active_bindings: Vec<ActiveBindingDef>,
    pub effects: Vec<SyntaxEffect>,
    #[serde(default)]
    pub construction: Vec<ConstructionExpr>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParsedRFile {
    pub expressions: Vec<ParsedExpression>,
    #[serde(default)]
    pub issues: Vec<SemanticIssue>,
    #[serde(default)]
    pub scope_parents: Vec<Option<LexicalScopeId>>,
    #[serde(default)]
    pub declarations: Vec<BindingDeclaration>,
}

impl ParsedRFile {
    pub fn bindings(&self) -> impl Iterator<Item = &BindingDef> {
        self.expressions
            .iter()
            .flat_map(|expr| expr.definitions.iter())
    }
}

#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize,
)]
pub struct LexicalScopeId(pub u32);

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct LexicalBindingId {
    pub defining_scope: LexicalScopeId,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BindingDeclaration {
    pub declaring_scope: LexicalScopeId,
    pub binding: LexicalBindingId,
    pub domain: DeclaredDomain,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeclaredDomain {
    Exact(Vec<DeclaredValue>),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DeclaredValue {
    S3Class(Vec<String>),
}

impl ParsedRFile {
    pub fn scope_is_within(&self, scope: LexicalScopeId, ancestor: LexicalScopeId) -> bool {
        let mut current = Some(scope);
        while let Some(candidate) = current {
            if candidate == ancestor {
                return true;
            }
            current = self
                .scope_parents
                .get(candidate.0 as usize)
                .copied()
                .flatten();
        }
        false
    }

    pub fn domain_for(
        &self,
        binding: &LexicalBindingId,
        use_scope: LexicalScopeId,
    ) -> Option<Vec<DeclaredValue>> {
        self.declarations
            .iter()
            .filter(|declaration| {
                &declaration.binding == binding
                    && self.scope_is_within(use_scope, declaration.declaring_scope)
            })
            .map(|declaration| match &declaration.domain {
                DeclaredDomain::Exact(values) => values.clone(),
            })
            .reduce(|domain, values| {
                domain
                    .into_iter()
                    .filter(|value| values.contains(value))
                    .collect()
            })
    }
}
