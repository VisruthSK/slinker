pub mod facts;
pub mod oak;
pub mod source;

pub use facts::{
    ActiveBindingDef, BindingCertainty, BindingDeclaration, BindingDef, CallSite, CalleeKind,
    ConstructionArgument, ConstructionCall, ConstructionExpr, ConstructionExprKind,
    ConstructionTarget, DeclaredCallable, DeclaredDomain, EvalPhase, LexicalBindingId,
    LexicalScopeId, NameRef, NameRefKind, NamespaceInfoRead, NamespaceInfoReceiver, PackageGuard,
    PackageRef, ParsedExpression, ParsedRFile, ResourcePackage, ResourceRef, SemanticIssue,
    SemanticIssueKind, StaticArg, StaticEnvironment, SyntaxEffect, SyntaxEffectKind,
};
pub use oak::assigned_value_start;
pub(crate) use oak::{
    NamespaceImportResolution, NamespaceImports, closure_definitely_non_returning,
};
pub use oak::{OakParseContext, OakParser, RParser};
pub use source::{SourceId, SourceKey, SourceOrigin, Sources, Span, TextRange};
