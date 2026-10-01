use crate::syntax::source::Span;

use std::collections::BTreeSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NameRefKind {
    External,
    ConditionalFallthrough,
    MaybeLocal,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NameRef {
    pub name: String,
    pub kind: NameRefKind,
    pub phase: EvalPhase,
    pub guards: Vec<PackageGuard>,
    pub span: Span,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvalPhase {
    Materialization,
    Runtime,
}

#[derive(Debug, Clone, PartialEq, Eq)]
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageRef {
    pub package: String,
    pub symbol: String,
    pub internal: bool,
    pub guards: Vec<PackageGuard>,
    pub span: Span,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CalleeKind {
    DefinitelyLexical,
    DefinitelyExternal,
    ConditionalFallthrough,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallSite {
    pub callee: String,
    pub callee_kind: CalleeKind,
    pub qualified_package: Option<String>,
    pub args: Vec<Option<StaticArg>>,
    pub arg_names: Vec<Option<String>>,
    pub arg_spans: Vec<Option<Span>>,
    pub local_closure_args: Vec<bool>,
    pub scope: LexicalScopeId,
    pub arg_bindings: Vec<Option<LexicalBindingId>>,
    pub phase: EvalPhase,
    pub guards: Vec<PackageGuard>,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceRef {
    pub package: ResourcePackage,
    pub path: Option<String>,
    pub must_work: Option<bool>,
    pub guards: Vec<PackageGuard>,
    pub scope: LexicalScopeId,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResourcePackage {
    Literal(String),
    Computed(Option<LexicalBindingId>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StaticArg {
    String(String),
    Symbol(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StaticEnvironment {
    ClosureBinding(String),
    Namespace(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ConstructionExpr {
    pub kind: ConstructionExprKind,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
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

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ConstructionCall {
    pub callee: String,
    pub callee_kind: CalleeKind,
    pub qualified_package: Option<String>,
    pub arguments: Vec<ConstructionArgument>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ConstructionArgument {
    pub name: Option<String>,
    pub value: Option<ConstructionExpr>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveBindingDef {
    pub name: String,
    pub target: StaticEnvironment,
    pub certain: bool,
    pub guards: Vec<PackageGuard>,
    pub span: Span,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyntaxEffectKind {
    SuperAssignment,
    IndirectPackageWrite,
    UnsupportedAssignmentTarget,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyntaxEffect {
    pub kind: SyntaxEffectKind,
    pub target: Option<String>,
    pub target_enclosing_local: bool,
    pub value_symbol: Option<String>,
    pub phase: EvalPhase,
    pub guards: Vec<PackageGuard>,
    pub span: Span,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SemanticIssueKind {
    AmbiguousEffect,
    AmbiguousAttachOrder,
    UninstalledPackage,
    SourceCycle,
    InvalidDeclaration,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticIssue {
    pub kind: SemanticIssueKind,
    pub message: String,
    pub span: Option<Span>,
}

#[derive(Debug, Clone)]
pub struct ParsedExpression {
    pub span: Span,
    pub parameters: Vec<String>,
    pub used_parameters: Vec<String>,
    pub pinned_defaults: Vec<PinnedDefault>,
    pub references: Vec<NameRef>,
    pub package_refs: Vec<PackageRef>,
    pub resource_refs: Vec<ResourceRef>,
    pub calls: Vec<CallSite>,
    pub active_bindings: Vec<ActiveBindingDef>,
    pub effects: Vec<SyntaxEffect>,
    pub construction: Vec<ConstructionExpr>,
    pub namespace_info_reads: Vec<NamespaceInfoRead>,
    pub namespace_enumerations: Vec<NamespaceEnumeration>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinnedDefault {
    pub name: String,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamespaceInfoRead {
    pub receiver: NamespaceInfoReceiver,
    pub field: Option<String>,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamespaceEnumeration {
    pub package: String,
    pub callee: String,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NamespaceInfoReceiver {
    Lexical,
    Namespace(String),
    Computed,
}

#[derive(Debug, Clone)]
pub struct ParsedRFile {
    pub expressions: Vec<ParsedExpression>,
    pub issues: Vec<SemanticIssue>,
    pub scope_parents: Vec<Option<LexicalScopeId>>,
    pub declarations: Vec<BindingDeclaration>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct LexicalScopeId(pub u32);

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LexicalBindingId {
    pub defining_scope: LexicalScopeId,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BindingDeclaration {
    pub declaring_scope: LexicalScopeId,
    pub binding: LexicalBindingId,
    pub domain: DeclaredDomain,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeclaredDomain {
    Classes(Vec<Vec<String>>),
    Strings(BTreeSet<String>),
    Callables(BTreeSet<DeclaredCallable>),
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DeclaredCallable {
    pub package: Option<String>,
    pub name: String,
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

    fn visible_domains<'a>(
        &'a self,
        binding: &'a LexicalBindingId,
        use_scope: LexicalScopeId,
    ) -> impl Iterator<Item = &'a DeclaredDomain> {
        self.declarations
            .iter()
            .filter(move |declaration| {
                &declaration.binding == binding
                    && self.scope_is_within(use_scope, declaration.declaring_scope)
            })
            .map(|declaration| &declaration.domain)
    }

    pub fn class_domain_for(
        &self,
        binding: &LexicalBindingId,
        use_scope: LexicalScopeId,
    ) -> Option<Vec<Vec<String>>> {
        self.visible_domains(binding, use_scope)
            .filter_map(|domain| match domain {
                DeclaredDomain::Classes(classes) => Some(classes.clone()),
                DeclaredDomain::Strings(_) | DeclaredDomain::Callables(_) => None,
            })
            .reduce(|domain, classes| {
                domain
                    .into_iter()
                    .filter(|class| classes.contains(class))
                    .collect()
            })
    }

    pub fn string_domain_for(
        &self,
        binding: &LexicalBindingId,
        use_scope: LexicalScopeId,
    ) -> Option<BTreeSet<String>> {
        self.visible_domains(binding, use_scope)
            .filter_map(|domain| match domain {
                DeclaredDomain::Strings(strings) => Some(strings.clone()),
                DeclaredDomain::Classes(_) | DeclaredDomain::Callables(_) => None,
            })
            .reduce(|domain, strings| domain.intersection(&strings).cloned().collect())
    }

    pub fn callable_domain_for(
        &self,
        binding: &LexicalBindingId,
        use_scope: LexicalScopeId,
    ) -> Option<BTreeSet<DeclaredCallable>> {
        self.visible_domains(binding, use_scope)
            .filter_map(|domain| match domain {
                DeclaredDomain::Callables(callables) => Some(callables.clone()),
                DeclaredDomain::Classes(_) | DeclaredDomain::Strings(_) => None,
            })
            .reduce(|domain, callables| domain.intersection(&callables).cloned().collect())
    }
}
