pub mod facts;
pub mod oak;
pub mod source;

pub use facts::{
    ActiveBindingDef, BindingDeclaration, CallSite, CalleeKind, ConstructionArgument,
    ConstructionCall, ConstructionExpr, ConstructionExprKind, ConstructionTarget, DeclaredCallable,
    DeclaredDomain, EvalPhase, LexicalBindingId, LexicalScopeId, NameRef, NameRefKind,
    NamespaceEnumeration, NamespaceInfoRead, NamespaceInfoReceiver, PackageGuard, PackageRef,
    ParsedExpression, ParsedRFile, PinnedDefault, ResourceArguments, ResourcePackage, ResourceRef,
    SemanticIssue, SemanticIssueKind, StaticArg, StaticEnvironment, SyntaxEffect, SyntaxEffectKind,
};
pub(crate) use oak::SharedNames;
pub(crate) use oak::{
    NamespaceImportResolution, NamespaceImports, closure_definitely_non_returning,
};
pub use oak::{OakParseContext, OakParser};
pub use source::{SourceId, SourceKey, SourceLocation, SourceOrigin, Sources, Span, TextRange};
