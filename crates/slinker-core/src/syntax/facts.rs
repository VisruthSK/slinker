use crate::package::{Atom, BindingName, PackageName};
use crate::syntax::source::Span;
use std::sync::Arc;

use std::collections::BTreeSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NameRefKind {
    External,
    ConditionalFallthrough,
    MaybeLocal,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NameRef {
    pub name: BindingName,
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
    Available(PackageName),
    Loaded(PackageName),
    Selected(PackageName),
}

impl PackageGuard {
    pub fn package(&self) -> &PackageName {
        match self {
            Self::Available(package) | Self::Loaded(package) | Self::Selected(package) => package,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageRef {
    pub package: PackageName,
    pub symbol: BindingName,
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
    pub callee: Atom,
    pub callee_kind: CalleeKind,
    pub qualified_package: Option<Atom>,
    pub arguments: Box<[CallArgument]>,
    pub scope: LexicalScopeId,
    pub phase: EvalPhase,
    pub guards: Vec<PackageGuard>,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallArgument {
    pub value: Option<StaticArg>,
    pub name: Option<Atom>,
    pub span: Option<Span>,
    pub is_local_closure: bool,
    pub binding: Option<LexicalBindingId>,
}

impl CallSite {
    pub fn arg_count(&self) -> usize {
        self.arguments.len()
    }

    pub fn static_arg(&self, index: usize) -> Option<&StaticArg> {
        self.arguments.get(index)?.value.as_ref()
    }

    pub fn arg_name(&self, index: usize) -> Option<&str> {
        self.arguments.get(index)?.name.as_deref()
    }

    pub fn arg_span(&self, index: usize) -> Option<&Span> {
        self.arguments.get(index)?.span.as_ref()
    }

    pub fn arg_binding(&self, index: usize) -> Option<&LexicalBindingId> {
        self.arguments.get(index)?.binding.as_ref()
    }

    pub fn arg_is_local_closure(&self, index: usize) -> bool {
        self.arguments
            .get(index)
            .is_some_and(|argument| argument.is_local_closure)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceRef {
    pub package: ResourcePackage,
    pub arguments: ResourceArguments,
    pub guards: Vec<PackageGuard>,
    pub scope: LexicalScopeId,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResourceArguments {
    Static { path: String, must_work: bool },
    Unsupported,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResourcePackage {
    Literal(String),
    Computed(Option<LexicalBindingId>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StaticArg {
    String(Atom),
    Symbol(Atom),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StaticEnvironment {
    ClosureBinding(BindingName),
    Namespace(PackageName),
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
        value: Atom,
    },
    String {
        value: Atom,
    },
    Symbol {
        name: Atom,
    },
    Sequence {
        expressions: Arc<[ConstructionExpr]>,
    },
    Call {
        call: Arc<ConstructionCall>,
    },
    Member {
        object: Arc<ConstructionExpr>,
        name: Option<Atom>,
    },
    Index {
        object: Arc<ConstructionExpr>,
        index: Arc<ConstructionExpr>,
    },
    Assign {
        target: ConstructionTarget,
        value: Arc<ConstructionExpr>,
    },
    If {
        condition: Arc<ConstructionExpr>,
        consequence: Arc<ConstructionExpr>,
        alternative: Option<Arc<ConstructionExpr>>,
    },
    Function {
        parameters: Arc<[Atom]>,
        body: Arc<ConstructionExpr>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ConstructionCall {
    pub callee: Atom,
    pub callee_kind: CalleeKind,
    pub qualified_package: Option<Atom>,
    pub arguments: Arc<[ConstructionArgument]>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ConstructionArgument {
    pub name: Option<Atom>,
    pub value: Option<ConstructionExpr>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ConstructionTarget {
    Local {
        name: Atom,
    },
    Member {
        object: Arc<ConstructionExpr>,
        name: Option<Atom>,
    },
    ClosureEnvironment {
        closure: Arc<ConstructionExpr>,
    },
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveBindingDef {
    pub name: Atom,
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
    pub target: Option<Atom>,
    pub target_enclosing_local: bool,
    pub value_symbol: Option<Atom>,
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
    pub assigned_value_start: Option<usize>,
    pub parameters: Vec<Atom>,
    pub used_parameters: Vec<Atom>,
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
    pub name: Atom,
    pub value: Atom,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamespaceInfoRead {
    pub receiver: NamespaceInfoReceiver,
    pub field: Option<String>,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamespaceEnumeration {
    pub package: PackageName,
    pub callee: Atom,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NamespaceInfoReceiver {
    Lexical,
    Namespace(PackageName),
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
    pub name: Atom,
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
    pub package: Option<PackageName>,
    pub name: BindingName,
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
