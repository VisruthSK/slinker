use crate::package::{BindingName, PackageId};
use crate::syntax::{ResolvedName, Span};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SyntaxObservationKind {
    Deparse,
    Substitute,
    MatchCall,
    Other(String),
}

#[derive(Clone, Debug)]
pub enum SemanticRef {
    Binding(ResolvedName),
    NamespaceAccess {
        package: PackageId,
        binding: BindingName,
        internal: bool,
        span: Span,
    },
    ResourceAccess {
        package: PackageId,
        path: String,
        span: Span,
    },
    NamespaceDiscovery {
        package: PackageId,
        span: Span,
    },
    PackageAttachment {
        package: Option<PackageId>,
        span: Span,
    },
    VersionQuery {
        package: PackageId,
        span: Span,
    },
    LocationQuery {
        package: PackageId,
        span: Span,
    },
    NativeCall {
        symbol: String,
        span: Span,
    },
    SyntaxObservation {
        kind: SyntaxObservationKind,
        span: Span,
    },
    UnknownCall {
        callee: ResolvedName,
        span: Span,
    },
}
